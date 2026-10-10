//! The solver side of the event loop: messages from solvers' Mostrix, chat
//! events of watched conversations, and the notifications they lead to.

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

use super::notifier::{notification_text, Batch, Pending};
use super::protocol::{parse_solver_dm, Received, Rejected, SolverMessage};
use super::store::{Redeemed, SolverStore, Watched, WatchedConversation, MOSTRIX_SCOPE};
use super::sync::DISPUTE_KIND;
use crate::serbero::render::dispute_is_resolved;
use crate::serbero::telegram::Messenger;
use crate::{escape_markdown, escape_markdown_code};

/// How long receipts, handled events and ended disputes are kept: past the
/// one-day catch-up, so a re-fetched event is still recognised.
const RETENTION_SECS: u64 = 2 * 24 * 60 * 60;

/// How often old records are dropped.
const PRUNE_INTERVAL_SECS: u64 = 60 * 60;

/// Whether a Mostrix message is done with, or must be applied again when
/// the catch-up delivers it once more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Applied {
    Done,
    /// The sender is not linked yet: its `link` may arrive later.
    Retry,
}

pub struct SolverInbox {
    keys: Keys,
    /// Only this key's dispute events can end a watch.
    mostro: PublicKey,
    store: SolverStore,
    pending: Pending,
    grace: u64,
    /// Wakes the sync task to follow the new set of watched conversations.
    conversations_changed: Arc<Notify>,
    last_prune: u64,
}

impl SolverInbox {
    pub fn new(
        keys: Keys,
        mostro: PublicKey,
        store: SolverStore,
        grace: Duration,
        conversations_changed: Arc<Notify>,
    ) -> Self {
        Self {
            keys,
            mostro,
            store,
            pending: Pending::default(),
            grace: grace.as_secs(),
            conversations_changed,
            last_prune: 0,
        }
    }

    /// Handles a kind 14 event: a message from a solver's Mostrix, or a chat
    /// event of a watched conversation. Anything else is ignored. Errors are
    /// logged; the event loop goes on.
    pub async fn receive<M: Messenger>(&mut self, event: &Event, telegram: &M, now: u64) {
        if event.kind != Kind::PrivateDirectMessage {
            return;
        }
        // Mostrix's messages come first: a watch may name any key,
        // including the public one that signs a solver's messages, and must
        // not turn those messages into chat events. Anything else tagged to
        // the watchdog is still a chat event: a party may tag the public
        // watchdog key to keep its messages from being notified.
        let parsed = is_tagged_to(event, &self.keys.public_key())
            .then(|| parse_solver_dm(event, &self.keys))
            .flatten();
        let result = match parsed {
            Some(parsed) => self.receive_dm(event, parsed, telegram, now).await,
            None => match self.store.conversations(&event.pubkey).await {
                Ok(watches) => self.hold(event, &watches, now).await,
                Err(e) => Err(e),
            },
        };
        if let Err(e) = result {
            error!(event_id = %event.id, error = %e, "Failed to handle a solver notification event");
        }
    }

    /// Handles caught-up events oldest first, so `watch` and `unwatch`
    /// apply in order and links come before what follows them. Mostro's
    /// dispute events go before the rest: a resolved dispute's chat is
    /// never held.
    pub async fn receive_batch<M: Messenger>(
        &mut self,
        mut events: Vec<Event>,
        telegram: &M,
        now: u64,
    ) {
        events.sort_by_key(|event| (event.created_at, event.id));
        let (disputes, rest): (Vec<Event>, Vec<Event>) = events
            .into_iter()
            .partition(|event| event.kind == Kind::Custom(DISPUTE_KIND));
        for event in &disputes {
            self.on_dispute_event(event).await;
        }
        for event in &rest {
            self.receive(event, telegram, now).await;
        }
    }

    /// Holds a chat event for the grace period, once for each solver
    /// watching its conversation, unless it predates that solver's watch or
    /// was handled for them already.
    async fn hold(
        &mut self,
        event: &Event,
        watches: &[WatchedConversation],
        now: u64,
    ) -> Result<(), sqlx::Error> {
        let created_at = seconds(event.created_at.as_secs());
        for watch in watches {
            let solver = watch.solver_pubkey.as_str();
            if created_at < watch.watched_since
                || self.pending.contains(&event.id, solver)
                || self.store.is_handled(&event.id, solver).await?
            {
                continue;
            }
            self.pending
                .add(event.pubkey, solver, event.id, now, self.grace);
            debug!(event_id = %event.id, "Holding a dispute chat event");
        }
        Ok(())
    }

    /// Applies a message from a solver's Mostrix once: the catch-up delivers
    /// the day's messages again. A message from a key that is not linked yet
    /// is kept for the next catch-up, since its `link` may come later.
    async fn receive_dm<M: Messenger>(
        &mut self,
        event: &Event,
        parsed: Result<Received, Rejected>,
        telegram: &M,
        now: u64,
    ) -> Result<(), sqlx::Error> {
        if self.store.is_handled(&event.id, MOSTRIX_SCOPE).await? {
            return Ok(());
        }
        let applied = match parsed {
            Ok(received) => self.apply(received, telegram, now).await?,
            Err(rejected) => {
                self.log_rejected(&rejected).await?;
                Applied::Done
            }
        };
        if applied == Applied::Done {
            self.store
                .mark_handled(&event.id, MOSTRIX_SCOPE, seconds(now))
                .await?;
        }
        Ok(())
    }

    /// A broken message from a linked key is worth a warning; from anyone
    /// else it is noise anybody can send.
    async fn log_rejected(&self, rejected: &Rejected) -> Result<(), sqlx::Error> {
        let sender = rejected.identity.to_hex();
        if self.store.chat_of(&rejected.identity).await?.is_some() {
            warn!(%sender, error = %rejected.error, "Ignoring a solver notification message");
        } else {
            debug!(%sender, error = %rejected.error, "Ignoring a message from an unlinked key");
        }
        Ok(())
    }

    async fn apply<M: Messenger>(
        &mut self,
        received: Received,
        telegram: &M,
        now: u64,
    ) -> Result<Applied, sqlx::Error> {
        let solver = received.identity;
        if let SolverMessage::Link { code } = &received.message {
            self.link(&solver, code, telegram, seconds(now)).await?;
            return Ok(Applied::Done);
        }
        if self.store.chat_of(&solver).await?.is_none() {
            debug!(solver = %solver.to_hex(), "Message from a key not linked yet; kept for the catch-up");
            return Ok(Applied::Retry);
        }
        let created_at = seconds(received.created_at);
        match received.message {
            SolverMessage::Link { .. } => Ok(Applied::Done),
            SolverMessage::Watch {
                dispute_id,
                conversations,
            } => {
                let outcome = self
                    .store
                    .apply_watch(&solver, &dispute_id, &conversations, created_at)
                    .await?;
                match outcome {
                    Watched::Applied => {
                        info!(%dispute_id, solver = %solver.to_hex(), "👀 Watching a dispute chat");
                        self.conversations_changed.notify_one();
                    }
                    Watched::TooMany => warn!(
                        %dispute_id,
                        solver = %solver.to_hex(),
                        "Not watching a dispute chat: the key watches too many disputes"
                    ),
                    Watched::Stale => {}
                    // Unlinked between the check above and the watch.
                    Watched::NotLinked => return Ok(Applied::Retry),
                }
                Ok(Applied::Done)
            }
            SolverMessage::Unwatch { dispute_id } => {
                if self
                    .store
                    .apply_unwatch(&solver, &dispute_id, created_at)
                    .await?
                {
                    info!(%dispute_id, solver = %solver.to_hex(), "Stopped watching a dispute chat");
                    self.conversations_changed.notify_one();
                }
                Ok(Applied::Done)
            }
            SolverMessage::Sent { event_id } => {
                if !self
                    .store
                    .record_receipt(&event_id, &solver, seconds(now))
                    .await?
                {
                    warn!(solver = %solver.to_hex(), "Dropping a receipt: the key holds too many");
                }
                Ok(Applied::Done)
            }
        }
    }

    async fn link<M: Messenger>(
        &mut self,
        solver: &PublicKey,
        code: &str,
        telegram: &M,
        now: i64,
    ) -> Result<(), sqlx::Error> {
        let text = match self.store.redeem_link_code(code, solver, now).await? {
            Redeemed::UnknownCode => {
                warn!(solver = %solver.to_hex(), "Ignoring a link with an unknown or expired code");
                return Ok(());
            }
            Redeemed::TooManyKeys(chat_id) => {
                warn!(solver = %solver.to_hex(), chat_id, "Not linking: the chat links too many keys");
                (chat_id, too_many_keys_text())
            }
            Redeemed::Linked(chat_id) => {
                info!(solver = %solver.to_hex(), chat_id, "🔗 Solver key linked");
                // Mostrix sends the disputes the solver already holds right
                // after; a watch that arrived first is applied by the
                // catch-up this triggers.
                self.conversations_changed.notify_one();
                (chat_id, linked_text(solver))
            }
        };
        let (chat_id, text) = text;
        if let Err(e) = telegram.send(chat_id, &text, None).await {
            warn!(chat_id, error = %e, "Failed to answer a solver link on Telegram");
        }
        Ok(())
    }

    /// Notifies the batches whose grace period is over, and drops old
    /// records now and then.
    pub async fn flush<M: Messenger>(&mut self, telegram: &M, now: u64) {
        for batch in self.pending.take_due(now) {
            self.deliver(batch, telegram, now).await;
        }
        if now >= self.last_prune.saturating_add(PRUNE_INTERVAL_SECS) {
            self.last_prune = now;
            let before = seconds(now.saturating_sub(RETENTION_SECS));
            if let Err(e) = self.store.prune(before, seconds(now)).await {
                warn!(error = %e, "Failed to drop old solver notification records");
            }
        }
    }

    /// Notifies the batch's solver of the events it sent no receipt for.
    async fn deliver<M: Messenger>(&mut self, batch: Batch, telegram: &M, now: u64) {
        let now_secs = seconds(now);
        let (from_party, watch) = match self.split(&batch, now_secs).await {
            Ok(parts) => parts,
            Err(e) => {
                error!(error = %e, "Failed to read a dispute chat batch; retrying");
                self.pending.retry(batch, now);
                return;
            }
        };
        let Some(watch) = watch.filter(|_| !from_party.is_empty()) else {
            // Only the solver's own messages, or no longer watched.
            self.mark_handled(&from_party, &batch.solver, now_secs)
                .await;
            return;
        };
        let text = notification_text(watch.party, &watch.dispute_id, from_party.len());
        match telegram.send(watch.chat_id, &text, None).await {
            Ok(_) => {
                info!(
                    dispute_id = %watch.dispute_id,
                    party = watch.party.as_str(),
                    count = from_party.len(),
                    "📩 Solver notified of new dispute chat messages"
                );
                self.mark_handled(&from_party, &batch.solver, now_secs)
                    .await;
            }
            Err(e) => {
                let solver = batch.solver.clone();
                let retry = Batch {
                    event_ids: from_party.iter().copied().collect(),
                    ..batch
                };
                if self.pending.retry(retry, now) {
                    warn!(error = %e, "Failed to notify a solver; retrying");
                } else {
                    error!(error = %e, "Failed to notify a solver; giving up");
                    self.mark_handled(&from_party, &solver, now_secs).await;
                }
            }
        }
    }

    /// The events of `batch` its solver sent no receipt for, and the
    /// solver's watch of the conversation. Events with a receipt are marked
    /// handled.
    async fn split(
        &self,
        batch: &Batch,
        now: i64,
    ) -> Result<(Vec<EventId>, Option<WatchedConversation>), sqlx::Error> {
        let mut from_party = Vec::new();
        for id in &batch.event_ids {
            if self.store.has_receipt(id, &batch.solver).await? {
                self.store.mark_handled(id, &batch.solver, now).await?;
            } else {
                from_party.push(*id);
            }
        }
        let watch = self
            .store
            .conversation(&batch.sign_pubkey, &batch.solver)
            .await?;
        Ok((from_party, watch))
    }

    async fn mark_handled(&self, ids: &[EventId], solver: &str, now: i64) {
        for id in ids {
            if let Err(e) = self.store.mark_handled(id, solver, now).await {
                warn!(event_id = %id, error = %e, "Failed to record a handled dispute chat event");
            }
        }
    }

    /// Stops watching a dispute once Mostro's kind-38386 status says it is
    /// over.
    pub async fn on_dispute_event(&self, event: &Event) {
        if event.pubkey != self.mostro {
            return;
        }
        let Some((dispute_id, status)) = dispute_tags(event) else {
            return;
        };
        if !dispute_is_resolved(&status) {
            return;
        }
        match self
            .store
            .end_dispute(&dispute_id, seconds(event.created_at.as_secs()))
            .await
        {
            Ok(0) => {}
            Ok(_) => {
                info!(%dispute_id, "Dispute resolved; stopped watching its chat");
                self.conversations_changed.notify_one();
            }
            Err(e) => error!(%dispute_id, error = %e, "Failed to stop watching a resolved dispute"),
        }
    }
}

/// The `d` (dispute id) and `s` (status) tags of a dispute event.
fn dispute_tags(event: &Event) -> Option<(String, String)> {
    let value = |name: &str| {
        event
            .tags
            .iter()
            .map(|tag| tag.as_slice())
            .find(|tag| tag.len() >= 2 && tag[0] == name)
            .map(|tag| tag[1].clone())
    };
    Some((value("d")?, value("s")?))
}

fn is_tagged_to(event: &Event, pubkey: &PublicKey) -> bool {
    event.tags.public_keys().any(|tagged| tagged == *pubkey)
}

/// Event and clock times fit; saturate instead of wrapping if one does not.
fn seconds(secs: u64) -> i64 {
    i64::try_from(secs).unwrap_or(i64::MAX)
}

/// Sent to the chat that linked `solver`.
fn linked_text(solver: &PublicKey) -> String {
    let key = solver.to_bech32().unwrap_or_else(|_| solver.to_hex());
    format!(
        "✅ *Solver key linked*\n\n\
         🔑 `{}`\n\n\
         {}",
        escape_markdown_code(&key),
        escape_markdown(
            "You will get a message here when a party writes to you in a dispute you take in Mostrix."
        ),
    )
}

/// Sent when the chat already links as many keys as it may.
fn too_many_keys_text() -> String {
    escape_markdown(&format!(
        "This chat already links {} solver keys. Send /unlink to remove them, then link again.",
        super::store::MAX_KEYS_PER_CHAT
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DisputeMessageStore;
    use crate::serbero::testing::{Call, FakeTelegram};
    use mostro_core::message::{Action, Message, Payload};
    use mostro_core::transport::{wrap_message_nip44, WrapOptions};

    const DISPUTE: &str = "58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a";
    const CHAT: i64 = 4242;
    const GRACE: u64 = 20;
    const T0: u64 = 1_700_000_000;

    struct Fixture {
        _dir: tempfile::TempDir,
        inbox: SolverInbox,
        store: SolverStore,
        telegram: FakeTelegram,
        watchdog: Keys,
        mostro: Keys,
        solver: Keys,
        changed: Arc<Notify>,
    }

    impl Fixture {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let disputes = DisputeMessageStore::new(&dir.path().join("disputes.db"))
                .await
                .unwrap();
            let store = SolverStore::new(disputes.pool()).await.unwrap();
            let watchdog = Keys::generate();
            let changed = Arc::new(Notify::new());
            let mostro = Keys::generate();
            let inbox = SolverInbox::new(
                watchdog.clone(),
                mostro.public_key(),
                store.clone(),
                Duration::from_secs(GRACE),
                changed.clone(),
            );
            Self {
                _dir: dir,
                inbox,
                store,
                telegram: FakeTelegram::default(),
                watchdog,
                mostro,
                solver: Keys::generate(),
                changed,
            }
        }

        /// A message from the solver's Mostrix, dated `at`.
        fn mostrix(&self, json: &str, at: u64) -> Event {
            self.mostrix_from(&self.solver, json, at)
        }

        /// A message from the Mostrix of `sender`, dated `at`.
        fn mostrix_from(&self, sender: &Keys, json: &str, at: u64) -> Event {
            let message = Message::new_dm(
                None,
                None,
                Action::SendDm,
                Some(Payload::TextMessage(json.to_owned())),
            );
            let event = wrap_message_nip44(
                &message,
                sender,
                sender,
                self.watchdog.public_key(),
                WrapOptions::default(),
            )
            .unwrap();
            redate(&event, sender, at)
        }

        /// Links `keys` to `chat_id` through a `/link` code.
        async fn link(&mut self, keys: &Keys, chat_id: i64, code: &str) {
            self.store
                .create_link_code(code, chat_id, (T0 + 600) as i64, T0 as i64)
                .await
                .unwrap();
            let link = self.mostrix_from(
                keys,
                &format!(r#"{{"v":1,"type":"link","code":"{code}"}}"#),
                T0,
            );
            self.receive(&link, T0).await;
        }

        async fn receive(&mut self, event: &Event, now: u64) {
            self.inbox.receive(event, &self.telegram, now).await;
        }

        /// Links the solver and watches the buyer conversation at `T0`.
        async fn watching(&mut self) -> Keys {
            self.store
                .create_link_code("K7QM-2XPA", CHAT, (T0 + 600) as i64, T0 as i64)
                .await
                .unwrap();
            let link = self.mostrix(r#"{"v":1,"type":"link","code":"K7QM-2XPA"}"#, T0);
            self.receive(&link, T0).await;
            let buyer = Keys::generate();
            let watch = self.mostrix(
                &format!(
                    r#"{{"v":1,"type":"watch","dispute_id":"{DISPUTE}","conversations":[{{"party":"buyer","sign_pubkey":"{}"}}]}}"#,
                    buyer.public_key().to_hex()
                ),
                T0,
            );
            self.receive(&watch, T0).await;
            self.telegram.clear();
            buyer
        }
    }

    /// `event` signed again by `keys` with another date.
    fn redate(event: &Event, keys: &Keys, at: u64) -> Event {
        EventBuilder::new(event.kind, event.content.clone())
            .tags(event.tags.clone())
            .custom_created_at(Timestamp::from(at))
            .finalize(keys)
            .unwrap()
    }

    /// A dispute chat event as `mostro_core::chat` publishes it: signed by
    /// the conversation's `K_sign`, tagged to its `K_conv`.
    fn chat_event(sign: &Keys, at: u64, n: u8) -> Event {
        EventBuilder::new(Kind::PrivateDirectMessage, format!("ciphertext-{n}"))
            .tag(Tag::public_key(Keys::generate().public_key()))
            .custom_created_at(Timestamp::from(at))
            .finalize(sign)
            .unwrap()
    }

    fn notified(count: usize) -> Call {
        Call::Send {
            chat_id: CHAT,
            text: notification_text(super::super::protocol::Party::Buyer, DISPUTE, count),
            reply_to: None,
        }
    }

    #[tokio::test]
    async fn a_valid_code_links_the_solver_and_confirms_on_telegram() {
        let mut fx = Fixture::new().await;
        fx.store
            .create_link_code("K7QM-2XPA", CHAT, (T0 + 600) as i64, T0 as i64)
            .await
            .unwrap();

        let link = fx.mostrix(r#"{"v":1,"type":"link","code":"k7qm2xpa"}"#, T0);
        fx.receive(&link, T0 + 5).await;

        assert_eq!(
            fx.store.chat_of(&fx.solver.public_key()).await.unwrap(),
            Some(CHAT)
        );
        assert!(matches!(
            fx.telegram.sends().as_slice(),
            [Call::Send { chat_id: CHAT, text, .. }] if text.starts_with("✅ *Solver key linked*")
        ));
    }

    #[tokio::test]
    async fn an_unknown_code_links_nothing() {
        let mut fx = Fixture::new().await;

        let link = fx.mostrix(r#"{"v":1,"type":"link","code":"K7QM2XPA"}"#, T0);
        fx.receive(&link, T0).await;

        assert_eq!(
            fx.store.chat_of(&fx.solver.public_key()).await.unwrap(),
            None
        );
        assert!(fx.telegram.calls().is_empty());
    }

    #[tokio::test]
    async fn a_watch_from_an_unlinked_key_is_ignored() {
        let mut fx = Fixture::new().await;
        let buyer = Keys::generate();
        let watch = fx.mostrix(
            &format!(
                r#"{{"v":1,"type":"watch","dispute_id":"{DISPUTE}","conversations":[{{"party":"buyer","sign_pubkey":"{}"}}]}}"#,
                buyer.public_key().to_hex()
            ),
            T0,
        );

        fx.receive(&watch, T0).await;

        assert!(fx.store.watched_sign_pubkeys().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_party_message_is_notified_after_the_grace_period() {
        let mut fx = Fixture::new().await;
        let buyer = fx.watching().await;
        fx.receive(&chat_event(&buyer, T0 + 10, 1), T0 + 10).await;

        fx.inbox.flush(&fx.telegram, T0 + 10 + GRACE - 1).await;
        assert!(fx.telegram.calls().is_empty());
        fx.inbox.flush(&fx.telegram, T0 + 10 + GRACE).await;

        assert_eq!(fx.telegram.calls(), vec![notified(1)]);
    }

    #[tokio::test]
    async fn the_solvers_own_message_is_never_notified() {
        let mut fx = Fixture::new().await;
        let buyer = fx.watching().await;
        let own = chat_event(&buyer, T0 + 10, 1);
        let sent = fx.mostrix(
            &format!(
                r#"{{"v":1,"type":"sent","event_id":"{}"}}"#,
                own.id.to_hex()
            ),
            T0 + 10,
        );

        // The receipt may land after the chat event, within the grace period.
        fx.receive(&own, T0 + 10).await;
        fx.receive(&sent, T0 + 15).await;
        fx.inbox.flush(&fx.telegram, T0 + 60).await;

        assert!(fx.telegram.calls().is_empty());
        assert!(fx
            .store
            .is_handled(&own.id, &fx.solver.public_key().to_hex())
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn a_burst_mixing_both_sides_counts_only_party_messages() {
        let mut fx = Fixture::new().await;
        let buyer = fx.watching().await;
        let own = chat_event(&buyer, T0 + 11, 2);
        let sent = fx.mostrix(
            &format!(
                r#"{{"v":1,"type":"sent","event_id":"{}"}}"#,
                own.id.to_hex()
            ),
            T0 + 11,
        );
        fx.receive(&sent, T0 + 11).await;
        for event in [
            chat_event(&buyer, T0 + 10, 1),
            own,
            chat_event(&buyer, T0 + 12, 3),
        ] {
            fx.receive(&event, T0 + 12).await;
        }

        fx.inbox.flush(&fx.telegram, T0 + 60).await;

        assert_eq!(fx.telegram.calls(), vec![notified(2)]);
    }

    #[tokio::test]
    async fn a_redelivered_chat_event_is_notified_once() {
        let mut fx = Fixture::new().await;
        let buyer = fx.watching().await;
        let event = chat_event(&buyer, T0 + 10, 1);
        fx.receive(&event, T0 + 10).await;
        fx.inbox.flush(&fx.telegram, T0 + 60).await;

        fx.receive(&event, T0 + 70).await;
        fx.inbox.flush(&fx.telegram, T0 + 200).await;

        assert_eq!(fx.telegram.sends().len(), 1);
    }

    #[tokio::test]
    async fn history_before_the_watch_is_not_notified() {
        let mut fx = Fixture::new().await;
        let buyer = fx.watching().await;

        fx.receive(&chat_event(&buyer, T0 - 1, 1), T0 + 5).await;
        fx.inbox.flush(&fx.telegram, T0 + 100).await;

        assert!(fx.telegram.calls().is_empty());
    }

    #[tokio::test]
    async fn an_unwatched_conversation_is_not_notified() {
        let mut fx = Fixture::new().await;
        let buyer = fx.watching().await;
        fx.receive(&chat_event(&buyer, T0 + 10, 1), T0 + 10).await;
        let unwatch = fx.mostrix(
            &format!(r#"{{"v":1,"type":"unwatch","dispute_id":"{DISPUTE}"}}"#),
            T0 + 12,
        );

        fx.receive(&unwatch, T0 + 12).await;
        fx.inbox.flush(&fx.telegram, T0 + 60).await;

        assert!(fx.telegram.calls().is_empty());
    }

    #[tokio::test]
    async fn a_resolved_dispute_stops_being_watched() {
        let mut fx = Fixture::new().await;
        let buyer = fx.watching().await;
        let settled = EventBuilder::new(Kind::Custom(38386), "")
            .tags([
                Tag::identifier(DISPUTE),
                Tag::parse(["s", "settled"]).unwrap(),
            ])
            .finalize(&fx.mostro)
            .unwrap();

        fx.inbox.on_dispute_event(&settled).await;
        fx.receive(&chat_event(&buyer, T0 + 10, 1), T0 + 10).await;
        fx.inbox.flush(&fx.telegram, T0 + 60).await;

        assert!(fx.telegram.calls().is_empty());
        assert!(fx.store.watched_sign_pubkeys().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_caught_up_resolution_ends_the_watch_before_its_chat_is_held() {
        // The watchdog was stopped while the dispute was resolved.
        let mut fx = Fixture::new().await;
        let buyer = fx.watching().await;
        let settled = EventBuilder::new(Kind::Custom(38386), "")
            .tags([
                Tag::identifier(DISPUTE),
                Tag::parse(["s", "settled"]).unwrap(),
            ])
            .custom_created_at(Timestamp::from(T0 + 20))
            .finalize(&fx.mostro)
            .unwrap();
        let before = chat_event(&buyer, T0 + 10, 1);
        let after = chat_event(&buyer, T0 + 30, 2);

        fx.inbox
            .receive_batch(vec![after, before, settled], &fx.telegram, T0 + 40)
            .await;
        fx.inbox.flush(&fx.telegram, T0 + 100).await;

        assert!(fx.telegram.calls().is_empty(), "{:?}", fx.telegram.calls());
        assert!(fx.store.watched_sign_pubkeys().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_refused_notification_is_retried() {
        let mut fx = Fixture::new().await;
        let buyer = fx.watching().await;
        fx.receive(&chat_event(&buyer, T0 + 10, 1), T0 + 10).await;
        fx.telegram.set_down(true);
        fx.inbox.flush(&fx.telegram, T0 + 30).await;
        fx.telegram.set_down(false);

        fx.inbox.flush(&fx.telegram, T0 + 89).await;
        assert!(fx.telegram.calls().is_empty());
        fx.inbox.flush(&fx.telegram, T0 + 90).await;

        assert_eq!(fx.telegram.calls(), vec![notified(1)]);
    }

    #[tokio::test]
    async fn a_watch_wakes_the_subscription_task() {
        let mut fx = Fixture::new().await;
        fx.watching().await;

        // A stored permit: `notified` returns at once.
        tokio::time::timeout(Duration::from_secs(1), fx.changed.notified())
            .await
            .expect("the watch notified the sync task");
    }

    #[tokio::test]
    async fn caught_up_messages_apply_oldest_first() {
        let mut fx = Fixture::new().await;
        fx.watching().await;
        let unwatch = fx.mostrix(
            &format!(r#"{{"v":1,"type":"unwatch","dispute_id":"{DISPUTE}"}}"#),
            T0 + 20,
        );
        let buyer = Keys::generate();
        let rewatch = fx.mostrix(
            &format!(
                r#"{{"v":1,"type":"watch","dispute_id":"{DISPUTE}","conversations":[{{"party":"buyer","sign_pubkey":"{}"}}]}}"#,
                buyer.public_key().to_hex()
            ),
            T0 + 30,
        );

        fx.inbox
            .receive_batch(vec![rewatch, unwatch], &fx.telegram, T0 + 40)
            .await;

        assert_eq!(
            fx.store.watched_sign_pubkeys().await.unwrap(),
            vec![buyer.public_key()]
        );
    }

    #[tokio::test]
    async fn a_replayed_mostrix_message_is_handled_once() {
        // The periodic catch-up delivers the day's messages again; a watch
        // applied twice would wake the sync task, which catches up again.
        let mut fx = Fixture::new().await;
        fx.watching().await;
        let buyer = Keys::generate();
        let watch = fx.mostrix(
            &format!(
                r#"{{"v":1,"type":"watch","dispute_id":"{DISPUTE}","conversations":[{{"party":"buyer","sign_pubkey":"{}"}}]}}"#,
                buyer.public_key().to_hex()
            ),
            T0 + 30,
        );
        fx.receive(&watch, T0 + 30).await;
        fx.changed.notified().await;

        fx.receive(&watch, T0 + 40).await;

        assert!(
            tokio::time::timeout(Duration::from_millis(50), fx.changed.notified())
                .await
                .is_err(),
            "the replay woke the sync task"
        );
    }

    fn watch_json(sign: &PublicKey) -> String {
        format!(
            r#"{{"v":1,"type":"watch","dispute_id":"{DISPUTE}","conversations":[{{"party":"buyer","sign_pubkey":"{}"}}]}}"#,
            sign.to_hex()
        )
    }

    #[tokio::test]
    async fn a_receipt_from_another_key_does_not_silence_a_party() {
        // The buyer links a throwaway key and claims their own message.
        let mut fx = Fixture::new().await;
        let buyer = fx.watching().await;
        let throwaway = Keys::generate();
        fx.link(&throwaway, CHAT + 1, "BBBB-BBBB").await;
        let message = chat_event(&buyer, T0 + 10, 1);
        let forged = fx.mostrix_from(
            &throwaway,
            &format!(
                r#"{{"v":1,"type":"sent","event_id":"{}"}}"#,
                message.id.to_hex()
            ),
            T0 + 10,
        );
        fx.telegram.clear();

        fx.receive(&forged, T0 + 10).await;
        fx.receive(&message, T0 + 10).await;
        fx.inbox.flush(&fx.telegram, T0 + 60).await;

        assert_eq!(fx.telegram.calls(), vec![notified(1)]);
    }

    #[tokio::test]
    async fn another_keys_watch_does_not_take_the_conversation() {
        // K_sign is public: it signs every chat event.
        let mut fx = Fixture::new().await;
        let buyer = fx.watching().await;
        let attacker = Keys::generate();
        fx.link(&attacker, CHAT + 1, "BBBB-BBBB").await;
        let hijack = fx.mostrix_from(&attacker, &watch_json(&buyer.public_key()), T0 + 5);
        fx.receive(&hijack, T0 + 5).await;
        fx.telegram.clear();

        fx.receive(&chat_event(&buyer, T0 + 10, 1), T0 + 10).await;
        fx.inbox.flush(&fx.telegram, T0 + 60).await;

        assert!(
            fx.telegram.sends().contains(&notified(1)),
            "{:?}",
            fx.telegram.sends()
        );
    }

    #[tokio::test]
    async fn a_watch_naming_a_solvers_key_does_not_swallow_their_messages() {
        // The author of Mostrix's messages is public: it is on the relays.
        let mut fx = Fixture::new().await;
        fx.watching().await;
        let attacker = Keys::generate();
        fx.link(&attacker, CHAT + 1, "BBBB-BBBB").await;
        let hijack = fx.mostrix_from(&attacker, &watch_json(&fx.solver.public_key()), T0 + 5);
        fx.receive(&hijack, T0 + 5).await;
        fx.telegram.clear();

        let seller = Keys::generate();
        let watch = fx.mostrix(&watch_json(&seller.public_key()), T0 + 10);
        fx.receive(&watch, T0 + 10).await;
        fx.inbox.flush(&fx.telegram, T0 + 60).await;

        assert!(fx
            .store
            .watched_sign_pubkeys()
            .await
            .unwrap()
            .contains(&seller.public_key()));
        assert!(fx.telegram.calls().is_empty(), "{:?}", fx.telegram.calls());
    }

    #[tokio::test]
    async fn a_party_tagging_the_watchdog_is_still_notified() {
        // The watchdog's key is public; a party may tag it to hide.
        let mut fx = Fixture::new().await;
        let buyer = fx.watching().await;
        let tagged = EventBuilder::new(Kind::PrivateDirectMessage, "ciphertext")
            .tags([
                Tag::public_key(Keys::generate().public_key()),
                Tag::public_key(fx.watchdog.public_key()),
            ])
            .custom_created_at(Timestamp::from(T0 + 10))
            .finalize(&buyer)
            .unwrap();

        fx.receive(&tagged, T0 + 10).await;
        fx.receive(&tagged, T0 + 11).await;
        fx.inbox.flush(&fx.telegram, T0 + 60).await;

        assert_eq!(fx.telegram.calls(), vec![notified(1)]);
    }

    #[tokio::test]
    async fn a_watch_that_arrives_before_its_link_applies_once_linked() {
        // Relays may deliver Mostrix's link and watch out of order.
        let mut fx = Fixture::new().await;
        let buyer = Keys::generate();
        let watch = fx.mostrix(&watch_json(&buyer.public_key()), T0 + 1);
        fx.receive(&watch, T0 + 1).await;
        fx.link(&fx.solver.clone(), CHAT, "K7QM-2XPA").await;

        // The catch-up that follows the link delivers the watch again.
        fx.receive(&watch, T0 + 2).await;

        assert_eq!(
            fx.store.watched_sign_pubkeys().await.unwrap(),
            vec![buyer.public_key()]
        );
    }

    #[tokio::test]
    async fn a_repeated_watch_keeps_messages_written_since_the_first() {
        // Mostrix sends every held dispute again after a new link.
        let mut fx = Fixture::new().await;
        let buyer = fx.watching().await;
        let again = fx.mostrix(&watch_json(&buyer.public_key()), T0 + 100);
        fx.receive(&again, T0 + 100).await;

        fx.receive(&chat_event(&buyer, T0 + 50, 1), T0 + 101).await;
        fx.inbox.flush(&fx.telegram, T0 + 200).await;

        assert_eq!(fx.telegram.calls(), vec![notified(1)]);
    }

    #[tokio::test]
    async fn a_dispute_event_from_another_author_ends_nothing() {
        let mut fx = Fixture::new().await;
        let buyer = fx.watching().await;
        let forged = EventBuilder::new(Kind::Custom(38386), "")
            .tags([
                Tag::identifier(DISPUTE),
                Tag::parse(["s", "settled"]).unwrap(),
            ])
            .finalize(&Keys::generate())
            .unwrap();

        fx.inbox.on_dispute_event(&forged).await;

        assert_eq!(
            fx.store.watched_sign_pubkeys().await.unwrap(),
            vec![buyer.public_key()]
        );
    }

    #[test]
    fn the_link_confirmation_escapes_the_key() {
        let text = linked_text(&Keys::generate().public_key());

        assert!(text.contains("`npub1"), "{text}");
        assert!(text.ends_with("in Mostrix\\."), "{text}");
    }
}
