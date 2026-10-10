//! Dispute alerts: the kind-38386 status of a dispute, as a message in the
//! disputes channel. Only the live subscription may post a new message;
//! a catch-up only brings an existing message up to date.

use nostr_sdk::prelude::*;
use tracing::{debug, error, info, warn};

use crate::config::AlertsConfig;
use crate::db::DisputeMessageStore;
use crate::serbero::alerts::SerberoAlerts;
use crate::serbero::telegram::Messenger;
use crate::{alert_enabled, chrono_timestamp, escape_markdown, escape_markdown_code};

/// Where a dispute event came from, which decides what it may do to the
/// channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertMode {
    /// The live subscription: a status change as it happens. Sends a new
    /// message or edits the dispute's message.
    Live,
    /// A catch-up fetch (the solver sync reads the status of every watched
    /// dispute, with no lower time bound): mostly statuses already shown.
    /// Edits the dispute's message when the event is newer than the status
    /// recorded for it, and never sends a new message.
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

/// Sends or updates a dispute's alert, as `mode` allows. `serbero` (with a
/// `[serbero]` section) turns on the Serbero bookkeeping, the takeover
/// message and the Serbero line.
pub async fn handle_dispute_event<M: Messenger>(
    telegram: &M,
    chat_id: i64,
    event: &Event,
    mode: AlertMode,
    alerts_config: &AlertsConfig,
    dispute_store: &DisputeMessageStore,
    serbero: Option<&SerberoAlerts<'_, M>>,
) {
    let DisputeEvent {
        dispute_id,
        status,
        initiator,
        solver_pubkey,
    } = parse(event);
    let created_at = event.created_at.as_secs();

    info!(
        "Dispute event received: id={}, status={}, initiator={}, mode={:?}",
        dispute_id, status, initiator, mode
    );

    // The status recorded before this event, to tell a caught-up event
    // that is news from one already shown. Read before Serbero's
    // bookkeeping records this one.
    let previous = match dispute_store.recorded_status(&dispute_id).await {
        Ok(previous) => previous,
        Err(e) => {
            error!("Failed to read the dispute's recorded status: {}", e);
            None
        }
    };

    // Recorded before the alert toggles and the cooperative-cancel delete,
    // so a late Serbero handoff knows the dispute already ended and a
    // solver taking over from Serbero is announced whatever the status
    // alert toggles.
    if let Some(serbero) = serbero {
        serbero
            .note_dispute_status(&dispute_id, &status, created_at, mode == AlertMode::Live)
            .await;
    }
    // Recorded with or without Serbero: the next catch-up compares
    // against it.
    if let Err(e) = dispute_store
        .record_dispute_status(&dispute_id, &status, seconds(created_at))
        .await
    {
        error!("Failed to record the dispute's status: {}", e);
    }

    // Check if we have an existing message for this dispute
    let existing_message = match dispute_store.get_message(&dispute_id).await {
        Ok(result) => result,
        Err(e) => {
            error!("Failed to query dispute store: {}", e);
            None
        }
    };

    let shown_status = existing_message.as_ref().map(|m| m.status.as_str());
    if mode == AlertMode::CatchUp
        && !is_news(
            previous.as_ref(),
            seconds(created_at),
            &status,
            shown_status,
        )
    {
        debug!(
            "Caught-up status '{}' for dispute {} is already shown, skipping",
            status, dispute_id
        );
        return;
    }

    // Check if this alert type is enabled
    if !alert_enabled(&status, alerts_config) {
        info!(
            "Alert for status '{}' is disabled, skipping notification",
            status
        );
        return;
    }

    // A catch-up may only bring the dispute's message up to date.
    let existing_message = existing_message.map(|m| (m.message_id, m.chat_id));
    if mode == AlertMode::CatchUp && existing_message.is_none() {
        info!(
            "Caught-up status '{}' for dispute {} has no channel message, not posting it",
            status, dispute_id
        );
        return;
    }

    // Handle cooperative cancellation: delete the message
    if status == "canceled" {
        if let Some((message_id, stored_chat_id)) = existing_message {
            if let Err(e) = telegram.delete(stored_chat_id, message_id).await {
                warn!("Failed to delete dispute message: {}", e);
            } else {
                info!(
                    "🗑️ Deleted dispute message for {} (cooperative cancel)",
                    dispute_id
                );
            }
            if let Err(e) = dispute_store.delete(&dispute_id).await {
                error!("Failed to remove dispute from store: {}", e);
            }
        }
        return;
    }

    let message = alert_text(
        &dispute_id,
        &status,
        &initiator,
        solver_pubkey.as_deref(),
        created_at,
    );

    // Serbero's latest state for the dispute shows below the alert; the
    // store keeps the alert without it, to redraw it when the state changes.
    let shown = crate::serbero::alerts::decorate(
        dispute_store,
        &dispute_id,
        &status,
        &message,
        serbero.is_some_and(|s| s.show_progress),
    )
    .await;

    let Some((message_id, stored_chat_id)) = existing_message else {
        send_new_dispute_message(
            telegram,
            chat_id,
            &dispute_id,
            &status,
            &shown,
            &message,
            dispute_store,
        )
        .await;
        return;
    };

    match telegram.edit(stored_chat_id, message_id, &shown).await {
        Ok(()) => {
            info!(
                "✏️ Updated dispute message for {} (status: {})",
                dispute_id, status
            );
            if let Err(e) = dispute_store
                .update_status(&dispute_id, &status, &message)
                .await
            {
                error!("Failed to update dispute status in store: {}", e);
            }
        }
        // Live: the message was deleted, say; the status change still
        // has to show. Catch-up: nothing new may be posted.
        Err(e) if mode == AlertMode::Live => {
            warn!("Failed to edit message, sending new one: {}", e);
            send_new_dispute_message(
                telegram,
                chat_id,
                &dispute_id,
                &status,
                &shown,
                &message,
                dispute_store,
            )
            .await;
        }
        Err(e) => {
            warn!(
                "Failed to edit the dispute message for a caught-up status, not resending: {}",
                e
            );
        }
    }
}

/// Whether a caught-up `status` dated `created_at` has something to show:
/// it is newer than the recorded status, or it is the recorded status and
/// the message does not show it yet (an edit that failed; the status is
/// recorded before the edit). Nothing recorded means nothing was shown.
fn is_news(
    previous: Option<&crate::db::RecordedStatus>,
    created_at: i64,
    status: &str,
    shown_status: Option<&str>,
) -> bool {
    previous.is_none_or(|p| {
        created_at > p.created_at || (created_at == p.created_at && shown_status != Some(status))
    })
}

/// Event times fit; saturate instead of wrapping if one does not.
fn seconds(secs: u64) -> i64 {
    i64::try_from(secs).unwrap_or(i64::MAX)
}

/// The alert for a dispute status, in MarkdownV2.
fn alert_text(
    dispute_id: &str,
    status: &str,
    initiator: &str,
    solver_pubkey: Option<&str>,
    created_at: u64,
) -> String {
    match status {
        "initiated" => {
            format!(
                "🚨 *NEW DISPUTE*\n\n\
                 📋 *Dispute ID:* `{}`\n\
                 👤 *Initiated by:* {}\n\
                 ⏰ *Time:* {}\n\n\
                 ⚡ Please take this dispute in Mostrix or your admin client\\.",
                escape_markdown_code(dispute_id),
                escape_markdown(initiator),
                escape_markdown(&chrono_timestamp(created_at)),
            )
        }
        "in-progress" => {
            let solver_info = solver_pubkey
                .map(|pk| format!("\n👨‍⚖️ *Taken by:* `{}`", escape_markdown_code(pk)))
                .unwrap_or_default();
            format!(
                "🔄 *DISPUTE IN PROGRESS*\n\n\
                 📋 *Dispute ID:* `{}`{}\n\
                 ⏰ *Time:* {}\n\n\
                 ℹ️ Dispute is now being handled\\.",
                escape_markdown_code(dispute_id),
                solver_info,
                escape_markdown(&chrono_timestamp(created_at)),
            )
        }
        "seller-refunded" => {
            let solver_info = solver_pubkey
                .map(|pk| format!("\n👨‍⚖️ *Resolved by:* `{}`", escape_markdown_code(pk)))
                .unwrap_or_default();
            format!(
                "💰 *DISPUTE RESOLVED \\- SELLER REFUNDED*\n\n\
                 📋 *Dispute ID:* `{}`{}\n\
                 ⏰ *Time:* {}\n\n\
                 ✔️ Dispute closed: funds returned to seller\\.",
                escape_markdown_code(dispute_id),
                solver_info,
                escape_markdown(&chrono_timestamp(created_at)),
            )
        }
        "settled" => {
            let solver_info = solver_pubkey
                .map(|pk| format!("\n👨‍⚖️ *Resolved by:* `{}`", escape_markdown_code(pk)))
                .unwrap_or_default();
            format!(
                "✅ *DISPUTE RESOLVED \\- SETTLED*\n\n\
                 📋 *Dispute ID:* `{}`{}\n\
                 ⏰ *Time:* {}\n\n\
                 ✔️ Dispute closed: buyer receives payment\\.",
                escape_markdown_code(dispute_id),
                solver_info,
                escape_markdown(&chrono_timestamp(created_at)),
            )
        }
        "cooperatively-canceled" => {
            format!(
                "🤝 *DISPUTE RESOLVED \\- COOPERATIVELY CANCELED*\n\n\
                 📋 *Dispute ID:* `{}`\n\
                 🤝 *Resolution:* Both parties agreed to cancel\n\
                 ⏰ *Time:* {}\n\n\
                 ✔️ Dispute closed: funds returned to seller, no solver needed\\.",
                escape_markdown_code(dispute_id),
                escape_markdown(&chrono_timestamp(created_at)),
            )
        }
        "released" => {
            format!(
                "🔓 *DISPUTE RESOLVED \\- RELEASED*\n\n\
                 📋 *Dispute ID:* `{}`\n\
                 🤝 *Resolution:* Released by seller\n\
                 ⏰ *Time:* {}\n\n\
                 ✔️ Dispute closed: trade completed\\.",
                escape_markdown_code(dispute_id),
                escape_markdown(&chrono_timestamp(created_at)),
            )
        }
        _ => {
            format!(
                "📡 *DISPUTE STATUS UPDATE*\n\n\
                 📋 *Dispute ID:* `{}`\n\
                 📊 *Status:* {}\n\
                 ⏰ *Time:* {}\n\n\
                 ℹ️ Status changed\\.",
                escape_markdown_code(dispute_id),
                escape_markdown(status),
                escape_markdown(&chrono_timestamp(created_at)),
            )
        }
    }
}

/// Sends `shown` and stores the message, with `alert` (the text without
/// Serbero's line) for later redraws.
async fn send_new_dispute_message<M: Messenger>(
    telegram: &M,
    chat_id: i64,
    dispute_id: &str,
    status: &str,
    shown: &str,
    alert: &str,
    dispute_store: &DisputeMessageStore,
) {
    match telegram.send(chat_id, shown, None).await {
        Ok(message_id) => {
            info!(
                "✅ Telegram alert sent for dispute {} (status: {})",
                dispute_id, status
            );
            // Store the message ID for future updates
            if let Err(e) = dispute_store
                .insert(dispute_id, message_id, chat_id, status, alert)
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

    const DISPUTE: &str = "51733e4d-a155-465b-a97e-07fba5f0e485";
    const CHAT: i64 = -100_123;
    /// 2026-03-29 14:16:31 UTC, the incident's `in-progress` event.
    const TAKEN: u64 = 1_774_793_791;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: DisputeMessageStore,
        telegram: FakeTelegram,
        mostro: Keys,
        alerts: AlertsConfig,
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
            }
        }

        fn dispute_event(&self, status: &str, created_at: u64) -> Event {
            EventBuilder::new(DISPUTE_KIND, "")
                .tag(Tag::identifier(DISPUTE))
                .tag(Tag::parse(["s", status]).unwrap())
                .tag(Tag::parse(["initiator", "buyer"]).unwrap())
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
                &self.alerts,
                &self.store,
                None,
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
    }

    const DISPUTE_KIND: Kind = Kind::Custom(38386);

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
            [Call::Edit { chat_id: CHAT, message_id: 1, text }] if text.contains("SETTLED")
        ));
        assert_eq!(fx.stored_status().await.as_deref(), Some("settled"));
    }

    #[tokio::test]
    async fn a_caught_up_older_status_changes_nothing() {
        let fx = Fixture::new().await;
        fx.handle(&fx.dispute_event("settled", TAKEN + 3_600), AlertMode::Live)
            .await;
        fx.telegram.clear();

        fx.handle(&fx.dispute_event("in-progress", TAKEN), AlertMode::CatchUp)
            .await;

        assert!(fx.telegram.calls().is_empty());
        assert_eq!(fx.stored_status().await.as_deref(), Some("settled"));
    }

    #[tokio::test]
    async fn a_caught_up_status_already_recorded_is_not_shown_again() {
        let fx = Fixture::new().await;
        let settled = fx.dispute_event("settled", TAKEN + 3_600);
        fx.handle(&settled, AlertMode::Live).await;
        fx.telegram.clear();

        // The same event again: the catch-up fetched what the live
        // subscription already delivered.
        fx.handle(&settled, AlertMode::CatchUp).await;

        assert!(fx.telegram.calls().is_empty());
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
            [Call::Edit { message_id: 1, text, .. }] if text.contains("SETTLED")
        ));
        assert_eq!(fx.stored_status().await.as_deref(), Some("settled"));
    }

    #[tokio::test]
    async fn a_live_status_sends_a_new_message_and_then_edits_it() {
        let fx = Fixture::new().await;

        fx.handle(&fx.dispute_event("initiated", TAKEN - 60), AlertMode::Live)
            .await;
        fx.handle(&fx.dispute_event("in-progress", TAKEN), AlertMode::Live)
            .await;

        assert!(matches!(
            fx.telegram.calls().as_slice(),
            [
                Call::Send { chat_id: CHAT, text: sent, reply_to: None },
                Call::Edit { chat_id: CHAT, message_id: 1, text: edited },
            ] if sent.contains("NEW DISPUTE") && edited.contains("IN PROGRESS")
        ));
        assert_eq!(fx.stored_status().await.as_deref(), Some("in-progress"));
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

        // The message was deleted by hand, say: the status still shows.
        assert!(matches!(
            fx.telegram.sends().as_slice(),
            [Call::Send { chat_id: CHAT, text, reply_to: None }] if text.contains("IN PROGRESS")
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
        // Still what the message shows: the next newer status redraws it.
        assert_eq!(fx.stored_status().await.as_deref(), Some("in-progress"));
    }

    #[tokio::test]
    async fn a_live_cancel_deletes_the_disputes_message() {
        let fx = Fixture::new().await;
        fx.handle(&fx.dispute_event("initiated", TAKEN - 60), AlertMode::Live)
            .await;
        fx.telegram.clear();

        fx.handle(&fx.dispute_event("canceled", TAKEN), AlertMode::Live)
            .await;

        assert_eq!(
            fx.telegram.deletes(),
            vec![Call::Delete {
                chat_id: CHAT,
                message_id: 1
            }]
        );
        assert_eq!(fx.store.get_message(DISPUTE).await.unwrap(), None);
    }

    #[tokio::test]
    async fn every_status_is_recorded_whatever_the_mode() {
        let fx = Fixture::new().await;

        fx.handle(&fx.dispute_event("in-progress", TAKEN), AlertMode::CatchUp)
            .await;

        let recorded = fx.store.recorded_status(DISPUTE).await.unwrap().unwrap();
        assert_eq!(recorded.status, "in-progress");
        assert_eq!(recorded.created_at, TAKEN as i64);
    }

    #[test]
    fn an_event_is_news_when_newer_than_the_recorded_status() {
        let recorded = crate::db::RecordedStatus {
            status: "in-progress".into(),
            created_at: 100,
        };
        assert!(is_news(None, 100, "in-progress", None));
        assert!(is_news(
            Some(&recorded),
            101,
            "settled",
            Some("in-progress")
        ));
        assert!(!is_news(
            Some(&recorded),
            99,
            "initiated",
            Some("in-progress")
        ));
        // The recorded status, already shown.
        assert!(!is_news(
            Some(&recorded),
            100,
            "in-progress",
            Some("in-progress")
        ));
        // The recorded status, not shown yet: the edit failed before.
        assert!(is_news(
            Some(&recorded),
            100,
            "in-progress",
            Some("initiated")
        ));
    }
}
