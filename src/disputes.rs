//! Dispute alerts: the kind-38386 status of a dispute, as a step on the
//! dispute's timeline message in the disputes channel. Only the live
//! subscription may post a new message; a catch-up only brings an existing
//! message up to date.

use nostr_sdk::prelude::*;
use tracing::{debug, error, info, warn};

use crate::alert_enabled;
use crate::config::AlertsConfig;
use crate::db::DisputeMessageStore;
use crate::serbero::telegram::Messenger;
use crate::timeline::{self, status_step, Entry, Names, Nudge};

/// Where a dispute event came from, which decides what it may do to the
/// channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertMode {
    /// The live subscription: a status change as it happens. Sends a new
    /// message or edits the dispute's message.
    Live,
    /// A catch-up fetch (the solver sync reads the status of every watched
    /// dispute, with no lower time bound): mostly statuses already shown.
    /// Edits the dispute's message, and never sends a new one.
    CatchUp,
}

/// The fields of a dispute event the alerts use.
struct DisputeEvent {
    dispute_id: String,
    status: String,
    initiator: String,
    solver_pubkey: Option<String>,
}

fn parse(event: &Event) -> DisputeEvent {
    let mut parsed = DisputeEvent {
        dispute_id: String::from("unknown"),
        status: String::from("unknown"),
        initiator: String::from("unknown"),
        solver_pubkey: None,
    };
    for tag in event.tags.iter() {
        let tag_vec: Vec<String> = tag.as_slice().iter().map(|s| s.to_string()).collect();
        if tag_vec.len() >= 2 {
            match tag_vec[0].as_str() {
                "d" => parsed.dispute_id = tag_vec[1].clone(),
                "s" => parsed.status = tag_vec[1].clone(),
                "initiator" => parsed.initiator = tag_vec[1].clone(),
                "solver" => parsed.solver_pubkey = Some(tag_vec[1].clone()),
                _ => {}
            }
        }
    }
    parsed
}

/// How dispute alerts are shown: the `[alerts]` toggles, the solvers'
/// names and the notification that follows a live edit, if any.
#[derive(Debug, Clone, Copy)]
pub struct AlertSettings<'a> {
    pub config: &'a AlertsConfig,
    pub names: &'a Names,
    pub nudge: Option<Nudge>,
}

/// Adds a dispute's status to its timeline, as `mode` allows, and shows
/// the timeline on the dispute's message. A live edit is followed by the
/// settings' nudge, when there is one, so the status change notifies like
/// a new message would.
pub async fn handle_dispute_event<M: Messenger>(
    telegram: &M,
    chat_id: i64,
    event: &Event,
    mode: AlertMode,
    settings: AlertSettings<'_>,
    dispute_store: &DisputeMessageStore,
) {
    let AlertSettings {
        config: alerts_config,
        names,
        nudge,
    } = settings;
    let DisputeEvent {
        dispute_id,
        status,
        initiator,
        solver_pubkey,
    } = parse(event);
    let created_at = seconds(event.created_at.as_secs());

    info!(
        "Dispute event received: id={}, status={}, initiator={}, mode={:?}",
        dispute_id, status, initiator, mode
    );

    if let Err(e) = timeline::backfill(dispute_store, &dispute_id).await {
        error!("Failed to backfill the dispute's timeline: {}", e);
    }
    let (kind, detail) = status_step(
        &status,
        (initiator != "unknown").then_some(initiator.as_str()),
        solver_pubkey.as_deref(),
    );
    if let Err(e) = dispute_store
        .append_timeline(&dispute_id, kind.as_str(), detail.as_deref(), created_at)
        .await
    {
        // Shown anyway: the message is rendered from what is stored.
        error!("Failed to record the dispute's timeline step: {}", e);
    }

    let existing_message = match dispute_store.get_message(&dispute_id).await {
        Ok(message) => message,
        Err(e) => {
            error!("Failed to query dispute store: {}", e);
            None
        }
    };
    let text = match timeline::rendered(dispute_store, names, &dispute_id).await {
        Ok(text) => text,
        Err(e) => {
            error!("Failed to read the dispute's timeline: {}", e);
            return;
        }
    };
    // The stored text is written once an edit or a send succeeds: a
    // redelivered status with nothing new to show stops here, while one
    // whose edit failed is tried again.
    if existing_message
        .as_ref()
        .is_some_and(|m| m.text.as_deref() == Some(text.as_str()))
    {
        debug!(
            "Status '{}' of dispute {} already shown on its message, skipping",
            status, dispute_id
        );
        return;
    }

    // The timeline is kept up to date whatever the alert toggles and the
    // mode. Only a live, enabled status sends a new message, or notifies
    // of the edit.
    let may_send = mode == AlertMode::Live && alert_enabled(&status, alerts_config);

    let Some(message) = existing_message else {
        if may_send {
            send_new_dispute_message(
                telegram,
                chat_id,
                &dispute_id,
                &status,
                &text,
                dispute_store,
            )
            .await;
        } else {
            info!(
                "Status '{}' of dispute {} has no channel message and may not post one (mode {:?})",
                status, dispute_id, mode
            );
        }
        return;
    };

    match telegram
        .edit(message.chat_id, message.message_id, &text)
        .await
    {
        Ok(()) => {
            info!(
                "✏️ Updated dispute message for {} (status: {})",
                dispute_id, status
            );
            if let Err(e) = dispute_store
                .update_status(&dispute_id, &status, &text)
                .await
            {
                error!("Failed to update dispute status in store: {}", e);
            }
            if let Some(nudge) = nudge.filter(|_| may_send) {
                let added = Entry::new(kind, detail.as_deref(), created_at);
                if let Err(e) = timeline::nudge(
                    dispute_store,
                    telegram,
                    names,
                    &dispute_id,
                    &message,
                    &added,
                    nudge,
                )
                .await
                {
                    error!(
                        "Failed to read the timeline for the edit notification: {}",
                        e
                    );
                }
            }
        }
        // Live: the message was deleted, say; the status change still has
        // to show, with the whole timeline. Catch-up or a status turned
        // off: nothing new may be posted.
        Err(e) if may_send => {
            warn!("Failed to edit message, sending new one: {}", e);
            send_new_dispute_message(
                telegram,
                chat_id,
                &dispute_id,
                &status,
                &text,
                dispute_store,
            )
            .await;
        }
        Err(e) => {
            warn!(
                "Failed to edit the dispute message, not resending (mode {:?}): {}",
                mode, e
            );
        }
    }
}

/// Event times fit; saturate instead of wrapping if one does not.
fn seconds(secs: u64) -> i64 {
    i64::try_from(secs).unwrap_or(i64::MAX)
}

/// Sends `text` and stores the message.
async fn send_new_dispute_message<M: Messenger>(
    telegram: &M,
    chat_id: i64,
    dispute_id: &str,
    status: &str,
    text: &str,
    dispute_store: &DisputeMessageStore,
) {
    match telegram.send(chat_id, text).await {
        Ok(message_id) => {
            info!(
                "✅ Telegram alert sent for dispute {} (status: {})",
                dispute_id, status
            );
            // Store the message ID for future updates
            if let Err(e) = dispute_store
                .insert(dispute_id, message_id, chat_id, status, text)
                .await
            {
                error!("Failed to store dispute message ID: {}", e);
            }
        }
        Err(e) => {
            error!("Failed to send Telegram alert: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serbero::testing::{Call, FakeTelegram};
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    const DISPUTE: &str = "51733e4d-a155-465b-a97e-07fba5f0e485";
    const CHAT: i64 = -100_123;
    const SOLVER: &str = "000000e2fdb5000000000000000000000000000000000000000000000000a7f1";
    /// 2026-03-29 14:16:31 UTC, the incident's `in-progress` event.
    const TAKEN: u64 = 1_774_793_791;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: DisputeMessageStore,
        telegram: FakeTelegram,
        mostro: Keys,
        alerts: AlertsConfig,
        names: Names,
        nudge: Option<Nudge>,
    }

    impl Fixture {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = DisputeMessageStore::new(&dir.path().join("disputes.db"))
                .await
                .unwrap();
            Self {
                _dir: dir,
                store,
                telegram: FakeTelegram::default(),
                mostro: Keys::generate(),
                alerts: AlertsConfig::default(),
                names: Names::default(),
                nudge: None,
            }
        }

        fn dispute_event(&self, status: &str, created_at: u64) -> Event {
            let mut builder = EventBuilder::new(DISPUTE_KIND, "")
                .tag(Tag::identifier(DISPUTE))
                .tag(Tag::parse(["s", status]).unwrap())
                .tag(Tag::parse(["initiator", "buyer"]).unwrap());
            if matches!(status, "settled" | "seller-refunded") {
                builder = builder.tag(Tag::parse(["solver", SOLVER]).unwrap());
            }
            builder
                .custom_created_at(Timestamp::from_secs(created_at))
                .finalize(&self.mostro)
                .unwrap()
        }

        async fn handle(&self, event: &Event, mode: AlertMode) {
            handle_dispute_event(
                &self.telegram,
                CHAT,
                event,
                mode,
                AlertSettings {
                    config: &self.alerts,
                    names: &self.names,
                    nudge: self.nudge,
                },
                &self.store,
            )
            .await;
        }

        async fn stored_status(&self) -> Option<String> {
            self.store
                .get_message(DISPUTE)
                .await
                .unwrap()
                .map(|m| m.status)
        }

        async fn steps(&self) -> Vec<String> {
            self.store
                .timeline(DISPUTE)
                .await
                .unwrap()
                .into_iter()
                .map(|row| row.kind)
                .collect()
        }
    }

    const DISPUTE_KIND: Kind = Kind::Custom(38386);

    fn text_of(call: &Call) -> &str {
        match call {
            Call::Send { text, .. } | Call::Edit { text, .. } | Call::Nudge { text, .. } => text,
        }
    }

    /// The incident: a solver starts watching a dispute taken months ago,
    /// the catch-up fetches its `in-progress` status, and the channel has
    /// no message for it.
    #[tokio::test]
    async fn a_caught_up_status_without_a_channel_message_is_not_posted() {
        let fx = Fixture::new().await;
        let stale = fx.dispute_event("in-progress", TAKEN);

        fx.handle(&stale, AlertMode::CatchUp).await;

        assert!(fx.telegram.calls().is_empty());
        assert_eq!(fx.store.get_message(DISPUTE).await.unwrap(), None);
        // Kept for the day the dispute does get a message.
        assert_eq!(fx.steps().await, vec!["taken"]);
    }

    #[tokio::test]
    async fn a_live_edit_is_followed_by_a_nudge_replying_to_the_message() {
        let mut fx = Fixture::new().await;
        let lifetime = Duration::from_secs(5);
        fx.nudge = Some(Nudge { lifetime });
        fx.handle(&fx.dispute_event("initiated", TAKEN - 60), AlertMode::Live)
            .await;
        // The new message notifies by itself.
        assert!(fx.telegram.nudges().is_empty());
        fx.telegram.clear();

        fx.handle(&fx.dispute_event("in-progress", TAKEN), AlertMode::Live)
            .await;

        let calls = fx.telegram.calls();
        assert!(
            matches!(
                calls.as_slice(),
                [
                    Call::Edit { message_id: 1, .. },
                    Call::Nudge { chat_id: CHAT, reply_to: 1, text, lifetime: l }
                ] if *l == lifetime
                    && text.starts_with("🔔 *Dispute* `51733e4d-a155-465b-a97e-07fba5f0e485`\n")
                    && text.ends_with("Taken by a solver")
            ),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn a_caught_up_edit_or_a_status_turned_off_does_not_nudge() {
        let mut fx = Fixture::new().await;
        fx.nudge = Some(Nudge {
            lifetime: Duration::from_secs(5),
        });
        fx.alerts.settled = false;
        fx.handle(&fx.dispute_event("initiated", TAKEN - 60), AlertMode::Live)
            .await;
        fx.telegram.clear();

        fx.handle(&fx.dispute_event("in-progress", TAKEN), AlertMode::CatchUp)
            .await;
        fx.handle(&fx.dispute_event("settled", TAKEN + 3_600), AlertMode::Live)
            .await;

        assert_eq!(fx.telegram.edits().len(), 2);
        assert!(fx.telegram.nudges().is_empty());
    }

    #[tokio::test]
    async fn a_live_edit_is_silent_with_edit_notifications_off() {
        let fx = Fixture::new().await;
        fx.handle(&fx.dispute_event("initiated", TAKEN - 60), AlertMode::Live)
            .await;

        fx.handle(&fx.dispute_event("in-progress", TAKEN), AlertMode::Live)
            .await;

        assert_eq!(fx.telegram.edits().len(), 1);
        assert!(fx.telegram.nudges().is_empty());
    }

    #[tokio::test]
    async fn a_caught_up_newer_status_edits_the_disputes_message() {
        let fx = Fixture::new().await;
        fx.handle(&fx.dispute_event("in-progress", TAKEN), AlertMode::Live)
            .await;
        fx.telegram.clear();

        fx.handle(
            &fx.dispute_event("settled", TAKEN + 3_600),
            AlertMode::CatchUp,
        )
        .await;

        assert!(fx.telegram.sends().is_empty());
        assert!(matches!(
            fx.telegram.edits().as_slice(),
            [Call::Edit { chat_id: CHAT, message_id: 1, text }]
                if text.contains("RESOLVED · settled") && text.contains("Taken by")
        ));
        assert_eq!(fx.stored_status().await.as_deref(), Some("settled"));
    }

    /// A step that reaches the watchdog late still belongs on the
    /// timeline, in its place; the message's status stays the newest.
    #[tokio::test]
    async fn a_caught_up_older_status_is_added_to_the_timeline_without_a_new_message() {
        let fx = Fixture::new().await;
        fx.handle(&fx.dispute_event("settled", TAKEN + 3_600), AlertMode::Live)
            .await;
        fx.telegram.clear();

        fx.handle(&fx.dispute_event("in-progress", TAKEN), AlertMode::CatchUp)
            .await;

        assert!(fx.telegram.sends().is_empty());
        let edits = fx.telegram.edits();
        assert_eq!(edits.len(), 1);
        let text = text_of(&edits[0]);
        assert!(text.contains("*Status:* ✅ RESOLVED · settled"));
        let taken = text.find("Taken by").unwrap();
        let settled = text.find("Settled, buyer paid").unwrap();
        assert!(taken < settled, "{text}");
        assert_eq!(fx.steps().await, vec!["taken", "resolved"]);
    }

    #[tokio::test]
    async fn a_status_delivered_again_changes_nothing() {
        let fx = Fixture::new().await;
        let settled = fx.dispute_event("settled", TAKEN + 3_600);
        fx.handle(&settled, AlertMode::Live).await;
        fx.telegram.clear();

        // The same event again: the catch-up fetched what the live
        // subscription already delivered, then a relay redelivered it.
        fx.handle(&settled, AlertMode::CatchUp).await;
        fx.handle(&settled, AlertMode::Live).await;

        assert!(fx.telegram.calls().is_empty());
        assert_eq!(fx.steps().await, vec!["resolved"]);
    }

    #[tokio::test]
    async fn a_live_status_is_resent_when_the_edit_fails() {
        let fx = Fixture::new().await;
        fx.handle(&fx.dispute_event("initiated", TAKEN - 60), AlertMode::Live)
            .await;
        fx.telegram.clear();
        fx.telegram.edits_fail.store(true, Ordering::SeqCst);

        fx.handle(&fx.dispute_event("in-progress", TAKEN), AlertMode::Live)
            .await;

        // The message was deleted by hand, say: the whole timeline shows
        // again.
        assert!(matches!(
            fx.telegram.sends().as_slice(),
            [Call::Send { chat_id: CHAT, text }]
                if text.contains("Opened by buyer") && text.contains("Taken by a solver")
        ));
        let stored = fx.store.get_message(DISPUTE).await.unwrap().unwrap();
        assert_eq!(
            (stored.message_id, stored.status.as_str()),
            (2, "in-progress")
        );
    }

    #[tokio::test]
    async fn a_caught_up_status_is_not_resent_when_the_edit_fails() {
        let fx = Fixture::new().await;
        fx.handle(&fx.dispute_event("in-progress", TAKEN), AlertMode::Live)
            .await;
        fx.telegram.clear();
        fx.telegram.edits_fail.store(true, Ordering::SeqCst);

        fx.handle(
            &fx.dispute_event("settled", TAKEN + 3_600),
            AlertMode::CatchUp,
        )
        .await;

        assert!(fx.telegram.sends().is_empty());
        // Still what the message shows: the next step redraws it.
        assert_eq!(fx.stored_status().await.as_deref(), Some("in-progress"));
    }

    #[tokio::test]
    async fn a_caught_up_status_whose_edit_failed_is_retried_by_the_next_catch_up() {
        let fx = Fixture::new().await;
        fx.handle(&fx.dispute_event("in-progress", TAKEN), AlertMode::Live)
            .await;
        fx.telegram.clear();
        let settled = fx.dispute_event("settled", TAKEN + 3_600);
        fx.telegram.edits_fail.store(true, Ordering::SeqCst);
        fx.handle(&settled, AlertMode::CatchUp).await;
        assert!(fx.telegram.calls().is_empty());
        fx.telegram.edits_fail.store(false, Ordering::SeqCst);

        // The same event, fetched again by the next periodic catch-up.
        fx.handle(&settled, AlertMode::CatchUp).await;

        assert!(fx.telegram.sends().is_empty());
        assert!(matches!(
            fx.telegram.edits().as_slice(),
            [Call::Edit { message_id: 1, text, .. }] if text.contains("RESOLVED · settled")
        ));
        assert_eq!(fx.stored_status().await.as_deref(), Some("settled"));
    }

    #[tokio::test]
    async fn a_live_status_sends_one_message_and_then_edits_it_with_the_timeline() {
        let fx = Fixture::new().await;

        fx.handle(&fx.dispute_event("initiated", TAKEN - 60), AlertMode::Live)
            .await;
        fx.handle(&fx.dispute_event("in-progress", TAKEN), AlertMode::Live)
            .await;
        fx.handle(
            &fx.dispute_event("seller-refunded", TAKEN + 100),
            AlertMode::Live,
        )
        .await;

        let calls = fx.telegram.calls();
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert!(matches!(
            &calls[0],
            Call::Send { chat_id: CHAT, text }
                if text.contains("*Status:* 🚨 OPEN · needs a solver")
                    && text.contains("Opened by buyer")
        ));
        assert!(matches!(
            &calls[1],
            Call::Edit { chat_id: CHAT, message_id: 1, text }
                if text.contains("*Status:* 👨‍⚖️ WITH A SOLVER · a solver")
                    && text.contains("Opened by buyer")
                    && text.contains("Taken by a solver")
        ));
        assert!(matches!(
            &calls[2],
            Call::Edit { chat_id: CHAT, message_id: 1, text }
                if text.contains("*Status:* ✅ RESOLVED · seller refunded by solver 000000e2fdb5…a7f1")
                    && text.contains("Seller refunded · resolved by solver 000000e2fdb5…a7f1")
        ));
        assert_eq!(fx.stored_status().await.as_deref(), Some("seller-refunded"));
    }

    #[tokio::test]
    async fn a_live_cancel_closes_the_timeline_instead_of_deleting_the_message() {
        let fx = Fixture::new().await;
        fx.handle(&fx.dispute_event("initiated", TAKEN - 60), AlertMode::Live)
            .await;
        fx.telegram.clear();

        fx.handle(&fx.dispute_event("canceled", TAKEN), AlertMode::Live)
            .await;

        assert!(matches!(
            fx.telegram.calls().as_slice(),
            [Call::Edit { message_id: 1, text, .. }]
                if text.contains("*Status:* 🗑 CANCELED · cooperatively")
                    && text.contains("Canceled cooperatively")
        ));
        assert_eq!(fx.stored_status().await.as_deref(), Some("canceled"));
    }

    #[tokio::test]
    async fn a_status_turned_off_still_updates_the_message_but_never_sends_one() {
        let mut fx = Fixture::new().await;
        fx.alerts.in_progress = false;
        fx.alerts.initiated = false;

        fx.handle(&fx.dispute_event("initiated", TAKEN - 60), AlertMode::Live)
            .await;
        assert!(fx.telegram.calls().is_empty());
        fx.alerts.initiated = true;
        fx.handle(&fx.dispute_event("settled", TAKEN + 60), AlertMode::Live)
            .await;
        fx.telegram.clear();

        fx.handle(&fx.dispute_event("in-progress", TAKEN), AlertMode::Live)
            .await;

        assert!(fx.telegram.sends().is_empty());
        assert!(matches!(
            fx.telegram.edits().as_slice(),
            [Call::Edit { text, .. }] if text.contains("Taken by")
        ));
        assert_eq!(fx.steps().await, vec!["opened", "taken", "resolved"]);
    }

    #[tokio::test]
    async fn a_message_from_before_the_timeline_gets_its_stored_status_as_first_step() {
        let fx = Fixture::new().await;
        // Sent by an earlier version: a message, no timeline.
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "old alert")
            .await
            .unwrap();
        let sent_at = fx
            .store
            .get_message(DISPUTE)
            .await
            .unwrap()
            .unwrap()
            .created_at;

        fx.handle(
            &fx.dispute_event("settled", (sent_at + 60) as u64),
            AlertMode::Live,
        )
        .await;

        assert_eq!(fx.steps().await, vec!["taken", "resolved"]);
        assert!(matches!(
            fx.telegram.calls().as_slice(),
            [Call::Edit { message_id: 42, text, .. }]
                if text.contains("Taken by a solver") && text.contains("Settled, buyer paid")
        ));
    }
}
