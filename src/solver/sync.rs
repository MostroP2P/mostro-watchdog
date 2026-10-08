//! Keeping up with solvers in the background: the live subscriptions to
//! Mostrix's messages and to the watched conversations, and a catch-up of
//! the last day. Like Serbero's sync, nothing here holds the event loop
//! back: live events reach it as notifications and caught-up ones through
//! the backlog channel.

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::{mpsc, Notify};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::{debug, warn};

use super::store::SolverStore;
use crate::serbero::sync::{catch_up_since, refusals, CATCH_UP_TIMEOUT};

/// Id of the live subscription to messages written to the watchdog.
const DM_SUBSCRIPTION: &str = "mostro-watchdog-solver-dms";

/// Start of the ids of the live subscriptions to the watched conversations,
/// one per chunk of keys.
const CHAT_SUBSCRIPTION: &str = "mostro-watchdog-solver-chats";

/// Keys per chat subscription: relays refuse a REQ with too many authors.
pub const MAX_AUTHORS_PER_FILTER: usize = 200;

/// Mostro's dispute status events.
pub const DISPUTE_KIND: u16 = 38386;

/// Dispute ids per status request, for the same reason.
pub const MAX_DISPUTES_PER_FILTER: usize = 200;

/// Messages written to the watchdog: Mostrix's, from any key, since a key
/// is linked by its first message.
pub fn dm_filter(watchdog: PublicKey) -> Filter {
    Filter::new()
        .kind(Kind::PrivateDirectMessage)
        .pubkey(watchdog)
}

/// Chat events signed by the watched conversations' keys, in chunks of at
/// most [`MAX_AUTHORS_PER_FILTER`] keys.
pub fn chat_filters(sign_pubkeys: &[PublicKey]) -> Vec<Filter> {
    sign_pubkeys
        .chunks(MAX_AUTHORS_PER_FILTER)
        .map(|chunk| {
            Filter::new()
                .kind(Kind::PrivateDirectMessage)
                .authors(chunk.iter().copied())
        })
        .collect()
}

/// Mostro's latest status of each watched dispute, in chunks of at most
/// [`MAX_DISPUTES_PER_FILTER`] ids. No `since`: a dispute may have ended
/// long before a stopped watchdog comes back.
pub fn dispute_filters(mostro: PublicKey, dispute_ids: &[String]) -> Vec<Filter> {
    dispute_ids
        .chunks(MAX_DISPUTES_PER_FILTER)
        .map(|chunk| {
            Filter::new()
                .kind(Kind::Custom(DISPUTE_KIND))
                .author(mostro)
                .identifiers(chunk.iter().cloned())
        })
        .collect()
}

fn chat_subscription_id(chunk: usize) -> SubscriptionId {
    SubscriptionId::new(format!("{CHAT_SUBSCRIPTION}-{chunk}"))
}

pub struct SolverSync {
    pub client: Client,
    pub watchdog: PublicKey,
    /// Whose dispute events can end a watch.
    pub mostro: PublicKey,
    pub store: SolverStore,
    pub backlog: mpsc::UnboundedSender<Vec<Event>>,
    /// Notified when NIP-65 discovery swaps the relays.
    pub relays_changed: Arc<Notify>,
    /// Notified when the watched conversations change.
    pub conversations_changed: Arc<Notify>,
    pub refresh: Duration,
    /// The keys the live chat subscriptions follow.
    pub followed: Vec<PublicKey>,
    /// How many chat subscriptions were sent for `followed`.
    pub chat_subscriptions: usize,
}

impl SolverSync {
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(self.run())
    }

    async fn run(mut self) {
        let mut interval = tokio::time::interval(self.refresh);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            // The first tick is immediate: catch up at startup.
            tokio::select! {
                _ = interval.tick() => {}
                () = self.relays_changed.notified() => {}
                () = self.conversations_changed.notified() => {}
            }
            if self.backlog.is_closed() {
                return;
            }
            self.sync_once().await;
        }
    }

    /// One round: send the live subscriptions, following the conversations
    /// watched now, then catch up.
    pub async fn sync_once(&mut self) {
        self.subscribe(DM_SUBSCRIPTION, dm_filter(self.watchdog))
            .await;
        match self.store.watched_sign_pubkeys().await {
            Ok(watched) if watched != self.followed => {
                // nostr-sdk keeps an id's first filter; a new set needs the
                // old subscriptions closed. Until they are, keep following
                // the old set and try again next round.
                if self.close_chat_subscriptions().await {
                    self.followed = watched;
                }
            }
            Ok(_) => {}
            Err(e) => warn!(error = %e, "Failed to read the watched dispute chats"),
        }
        let filters = chat_filters(&self.followed);
        self.chat_subscriptions = filters.len();
        for (chunk, filter) in filters.into_iter().enumerate() {
            self.subscribe(&chat_subscription_id(chunk).to_string(), filter)
                .await;
        }
        self.catch_up().await;
    }

    /// Closes the chat subscriptions sent so far. Returns whether all closed.
    async fn close_chat_subscriptions(&mut self) -> bool {
        let mut closed = true;
        for chunk in 0..self.chat_subscriptions {
            if let Err(e) = self.client.unsubscribe(&chat_subscription_id(chunk)).await {
                warn!(error = %e, "Failed to close a dispute chat subscription");
                closed = false;
            }
        }
        if closed {
            self.chat_subscriptions = 0;
        }
        closed
    }

    /// Sends a live subscription. Relays that already hold it keep it;
    /// relays that lost it (a relay swap, a refused REQ) get it again.
    async fn subscribe(&self, id: &str, filter: Filter) {
        match self
            .client
            .subscribe(filter.since(Timestamp::now()))
            .with_id(SubscriptionId::new(id))
            .await
        {
            Ok(output) => {
                for (relay, reason) in refusals(&output.failed) {
                    warn!(%relay, reason, subscription = id, "A relay refused a solver subscription");
                }
            }
            Err(e) => warn!(error = %e, subscription = id, "Failed to send a solver subscription"),
        }
    }

    /// Fetches the last day of Mostrix's messages and watched chats, and the
    /// status of the watched disputes, and hands them to the event loop,
    /// which skips what it already handled. The live dispute subscription
    /// starts at launch, so a dispute resolved while the watchdog was
    /// stopped is only seen here.
    async fn catch_up(&self) {
        let since = catch_up_since(Timestamp::now());
        let watched_disputes = match self.store.watched_dispute_ids().await {
            Ok(ids) => ids,
            Err(e) => {
                warn!(error = %e, "Failed to read the watched disputes");
                Vec::new()
            }
        };
        let filters = std::iter::once(dm_filter(self.watchdog))
            .chain(chat_filters(&self.followed))
            .map(|filter| filter.since(since))
            .chain(dispute_filters(self.mostro, &watched_disputes));
        let mut caught_up = Vec::new();
        for filter in filters {
            match self
                .client
                .fetch_events(filter)
                .timeout(CATCH_UP_TIMEOUT)
                .await
            {
                Ok(events) => caught_up.extend(events),
                Err(e) => warn!(error = %e, "Failed to catch up on solver events"),
            }
        }
        if caught_up.is_empty() {
            return;
        }
        debug!(count = caught_up.len(), "Caught up on solver events");
        if self.backlog.send(caught_up).is_err() {
            debug!("The event loop stopped; dropping caught-up solver events");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DisputeMessageStore;
    use crate::solver::protocol::{Conversation, Party};
    use nostr_sdk::prelude::MockRelay;

    const DISPUTE: &str = "58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a";
    const WAIT: Duration = Duration::from_secs(5);

    struct Setup {
        _dir: tempfile::TempDir,
        relay: MockRelay,
        client: Client,
        publisher: Client,
        store: SolverStore,
        watchdog: Keys,
        mostro: Keys,
        solver: Keys,
    }

    async fn connect(url: &str) -> Client {
        let client = Client::default();
        client.add_relay(url).await.unwrap();
        client.connect().await;
        client
    }

    impl Setup {
        async fn new() -> Self {
            let relay = MockRelay::run().await.unwrap();
            let url = relay.url().await.to_string();
            let dir = tempfile::tempdir().unwrap();
            let disputes = DisputeMessageStore::new(&dir.path().join("disputes.db"))
                .await
                .unwrap();
            let store = SolverStore::new(disputes.pool()).await.unwrap();
            let solver = Keys::generate();
            store
                .create_link_code("K7QM-2XPA", 4242, 10, 1)
                .await
                .unwrap();
            store
                .redeem_link_code("K7QM-2XPA", &solver.public_key(), 2)
                .await
                .unwrap();
            Self {
                _dir: dir,
                client: connect(&url).await,
                publisher: connect(&url).await,
                relay,
                store,
                watchdog: Keys::generate(),
                mostro: Keys::generate(),
                solver,
            }
        }

        fn sync(&self) -> (SolverSync, mpsc::UnboundedReceiver<Vec<Event>>) {
            let (backlog, received) = mpsc::unbounded_channel();
            let sync = SolverSync {
                client: self.client.clone(),
                watchdog: self.watchdog.public_key(),
                mostro: self.mostro.public_key(),
                store: self.store.clone(),
                backlog,
                relays_changed: Arc::new(Notify::new()),
                conversations_changed: Arc::new(Notify::new()),
                refresh: Duration::from_secs(3600),
                followed: Vec::new(),
                chat_subscriptions: 0,
            };
            (sync, received)
        }

        async fn watch(&self, sign: &Keys, at: i64) {
            let conversation = Conversation {
                party: Party::Buyer,
                sign_pubkey: sign.public_key(),
            };
            self.store
                .apply_watch(&self.solver.public_key(), DISPUTE, &[conversation], at)
                .await
                .unwrap();
        }

        async fn publish(&self, event: &Event) {
            self.publisher.send_event(event).await.unwrap();
        }

        async fn shutdown(self) {
            self.client.shutdown().await;
            self.publisher.shutdown().await;
            drop(self.relay);
        }
    }

    fn chat_event(sign: &Keys, at: Timestamp) -> Event {
        EventBuilder::new(Kind::PrivateDirectMessage, "ciphertext")
            .tag(Tag::public_key(Keys::generate().public_key()))
            .custom_created_at(at)
            .finalize(sign)
            .unwrap()
    }

    /// The next kind 14 event the client is notified of, if one comes
    /// within `wait`.
    async fn next_event(
        notifications: &mut (impl StreamExt<Item = ClientNotification> + Unpin),
        wait: Duration,
    ) -> Option<(SubscriptionId, Event)> {
        tokio::time::timeout(wait, async {
            while let Some(notification) = notifications.next().await {
                if let ClientNotification::Event {
                    subscription_id,
                    event,
                    ..
                } = notification
                {
                    if event.kind == Kind::PrivateDirectMessage {
                        return Some((subscription_id, *event));
                    }
                }
            }
            None
        })
        .await
        .ok()
        .flatten()
    }

    #[test]
    fn the_filters_read_the_watchdogs_messages_and_the_watched_chats() {
        let watchdog = Keys::generate().public_key();
        let sign = Keys::generate().public_key();

        let dms = serde_json::to_value(dm_filter(watchdog)).unwrap();
        let chats = serde_json::to_value(&chat_filters(&[sign])[0]).unwrap();

        assert_eq!(dms["kinds"], serde_json::json!([14]));
        assert_eq!(dms["#p"], serde_json::json!([watchdog.to_hex()]));
        assert!(dms.get("authors").is_none());
        assert_eq!(chats["kinds"], serde_json::json!([14]));
        assert_eq!(chats["authors"], serde_json::json!([sign.to_hex()]));
    }

    /// Mostro's status event for `dispute_id`, dated `at`.
    fn dispute_event(mostro: &Keys, dispute_id: &str, status: &str, at: Timestamp) -> Event {
        EventBuilder::new(Kind::Custom(DISPUTE_KIND), "")
            .tags([
                Tag::identifier(dispute_id),
                Tag::parse(["s", status]).unwrap(),
            ])
            .custom_created_at(at)
            .finalize(mostro)
            .unwrap()
    }

    #[test]
    fn the_dispute_filters_read_mostros_status_of_watched_disputes() {
        let mostro = Keys::generate().public_key();
        let ids: Vec<String> = (0..MAX_DISPUTES_PER_FILTER + 1)
            .map(|n| format!("dispute-{n}"))
            .collect();

        let filters = dispute_filters(mostro, &ids);

        assert_eq!(filters.len(), 2);
        let first = serde_json::to_value(&filters[0]).unwrap();
        assert_eq!(first["kinds"], serde_json::json!([DISPUTE_KIND]));
        assert_eq!(first["authors"], serde_json::json!([mostro.to_hex()]));
        assert_eq!(
            first["#d"].as_array().map(Vec::len),
            Some(MAX_DISPUTES_PER_FILTER)
        );
        assert!(first.get("since").is_none());
        assert!(dispute_filters(mostro, &[]).is_empty());
    }

    #[test]
    fn many_watched_keys_are_split_across_subscriptions() {
        let keys: Vec<PublicKey> = (0..MAX_AUTHORS_PER_FILTER * 2 + 1)
            .map(|_| Keys::generate().public_key())
            .collect();

        let filters = chat_filters(&keys);

        assert_eq!(filters.len(), 3);
        let authors = |f: &Filter| f.authors.as_ref().map_or(0, |a| a.len());
        assert_eq!(authors(&filters[0]), MAX_AUTHORS_PER_FILTER);
        assert_eq!(authors(&filters[2]), 1);
        assert!(chat_filters(&[]).is_empty());
    }

    #[tokio::test]
    async fn live_chat_events_of_a_watched_conversation_arrive() {
        let setup = Setup::new().await;
        let buyer = Keys::generate();
        setup.watch(&buyer, 1).await;
        let (mut sync, _backlog) = setup.sync();
        let mut notifications = setup.client.notifications();
        sync.sync_once().await;

        let live = chat_event(&buyer, Timestamp::now());
        setup.publish(&live).await;

        let (subscription, event) = next_event(&mut notifications, WAIT)
            .await
            .expect("the chat event");
        assert_eq!(subscription, chat_subscription_id(0));
        assert_eq!(event, live);
        setup.shutdown().await;
    }

    #[tokio::test]
    async fn an_unwatched_conversation_is_no_longer_followed() {
        let setup = Setup::new().await;
        let (old, new) = (Keys::generate(), Keys::generate());
        setup.watch(&old, 1).await;
        let (mut sync, _backlog) = setup.sync();
        sync.sync_once().await;
        setup.watch(&new, 2).await;
        let mut notifications = setup.client.notifications();
        sync.sync_once().await;

        setup.publish(&chat_event(&old, Timestamp::now())).await;
        let fresh = chat_event(&new, Timestamp::now());
        setup.publish(&fresh).await;

        let (_, event) = next_event(&mut notifications, WAIT)
            .await
            .expect("the new conversation's event");
        assert_eq!(event, fresh);
        assert_eq!(sync.followed, vec![new.public_key()]);
        setup.shutdown().await;
    }

    #[tokio::test]
    async fn the_catch_up_brings_the_days_messages_and_chats() {
        let setup = Setup::new().await;
        let buyer = Keys::generate();
        setup.watch(&buyer, 1).await;
        let hour_ago = Timestamp::now() - Duration::from_secs(3600);
        let missed_chat = chat_event(&buyer, hour_ago);
        let missed_dm = EventBuilder::new(Kind::PrivateDirectMessage, "ciphertext")
            .tag(Tag::public_key(setup.watchdog.public_key()))
            .custom_created_at(hour_ago)
            .finalize(&setup.solver)
            .unwrap();
        setup.publish(&missed_chat).await;
        setup.publish(&missed_dm).await;
        let (mut sync, mut backlog) = setup.sync();

        sync.sync_once().await;

        let caught_up = tokio::time::timeout(WAIT, backlog.recv())
            .await
            .expect("a batch")
            .expect("the channel is open");
        let ids: Vec<EventId> = caught_up.iter().map(|e| e.id).collect();
        assert!(ids.contains(&missed_chat.id), "{ids:?}");
        assert!(ids.contains(&missed_dm.id), "{ids:?}");
        setup.shutdown().await;
    }

    #[tokio::test]
    async fn the_catch_up_brings_the_status_of_watched_disputes_of_any_age() {
        // A dispute resolved while the watchdog was stopped, days ago.
        let setup = Setup::new().await;
        setup.watch(&Keys::generate(), 1).await;
        let days_ago = Timestamp::now() - Duration::from_secs(3 * 24 * 3600);
        let resolved = dispute_event(&setup.mostro, DISPUTE, "settled", days_ago);
        let unwatched = dispute_event(&setup.mostro, "other-dispute", "settled", days_ago);
        setup.publish(&resolved).await;
        setup.publish(&unwatched).await;
        let (mut sync, mut backlog) = setup.sync();

        sync.sync_once().await;

        let caught_up = tokio::time::timeout(WAIT, backlog.recv())
            .await
            .expect("a batch")
            .expect("the channel is open");
        let ids: Vec<EventId> = caught_up.iter().map(|e| e.id).collect();
        assert!(ids.contains(&resolved.id), "{ids:?}");
        assert!(!ids.contains(&unwatched.id), "{ids:?}");
        setup.shutdown().await;
    }

    #[tokio::test]
    async fn without_watched_conversations_only_messages_are_followed() {
        let setup = Setup::new().await;
        let (mut sync, _backlog) = setup.sync();
        let mut notifications = setup.client.notifications();
        sync.sync_once().await;

        let stranger = Keys::generate();
        setup
            .publish(&chat_event(&stranger, Timestamp::now()))
            .await;

        assert!(next_event(&mut notifications, Duration::from_millis(500))
            .await
            .is_none());
        assert!(sync.followed.is_empty());
        setup.shutdown().await;
    }
}
