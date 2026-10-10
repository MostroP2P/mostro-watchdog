//! Relaying Serbero's updates: what to record, which dispute message to
//! redraw and when to ask the team for a solver.

use super::dm::{HeaderUpdate, Update};
use super::render::{dispute_is_resolved, needs_human_alert};
use super::telegram::Messenger;
use crate::db::{DisputeMessageStore, SerberoState, StoredMessage};
use crate::timeline::{self, EntryKind, Names};
use tracing::{info, warn};

/// Why an update could not be relayed. It is not recorded as relayed, so
/// the next delivery of the same DM tries again.
#[derive(Debug, thiserror::Error)]
pub enum AlertError {
    #[error("dispute store: {0}")]
    Store(#[from] sqlx::Error),
    #[error("Telegram: {0}")]
    Telegram(#[from] teloxide::RequestError),
}

/// What relaying an update did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Already relayed: a redelivery, a re-fetch or a restart.
    Duplicate,
    /// Recorded. `redrawn` when the dispute's message now shows it,
    /// `alerted` when a new message asked for a solver.
    Relayed { redrawn: bool, alerted: bool },
}

/// Relays Serbero updates to Telegram.
pub struct SerberoAlerts<'a, M> {
    pub store: &'a DisputeMessageStore,
    pub telegram: &'a M,
    /// The configured alert chat.
    pub chat_id: i64,
    /// `[alerts] serbero_progress`: show Serbero's state on the dispute's
    /// message.
    pub show_progress: bool,
    /// `[alerts] serbero_handoff`: send a message when a solver is needed.
    pub send_handoffs: bool,
    /// `[alerts] takeover_message`: send a message when a solver takes the
    /// dispute over from Serbero.
    pub send_takeovers: bool,
    /// How solvers are named on the timeline.
    pub names: Names,
}

/// The timeline step a Serbero update adds.
fn timeline_step(update: &Update) -> (EntryKind, Option<&str>) {
    match update {
        Update::Mediating => (EntryKind::SerberoMediating, None),
        Update::GuidanceSent { path } => (EntryKind::SerberoGuided, path.as_deref()),
        Update::HandedOff { reason } => (EntryKind::SerberoHandedOff, reason.as_deref()),
        Update::CouldNotStart => (EntryKind::SerberoCouldNotStart, None),
    }
}

impl<M: Messenger> SerberoAlerts<'_, M> {
    /// Relays one update, at most once per dispute and subject. The update
    /// is marked as relayed only once its alert went out, so a failed send
    /// is retried when the DM arrives again (catch-up, restart).
    pub async fn relay(&self, update: &HeaderUpdate) -> Result<Outcome, AlertError> {
        let subject = update.update.subject();
        if self
            .store
            .serbero_header_handled(&update.dispute_id, &subject)
            .await?
        {
            return Ok(Outcome::Duplicate);
        }
        self.record_state(update, &subject).await?;
        // With progress off, Serbero's steps stay off the timeline for
        // good: a later redraw would show them otherwise.
        let redrawn = self.show_progress && {
            timeline::backfill(self.store, &update.dispute_id).await?;
            let (kind, detail) = timeline_step(&update.update);
            self.store
                .append_timeline(
                    &update.dispute_id,
                    kind.as_str(),
                    detail,
                    seconds(update.created_at),
                )
                .await?;
            self.redraw(&update.dispute_id).await
        };
        let message = self.store.get_message(&update.dispute_id).await?;
        let taken_over = self.store.taken_over(&update.dispute_id).await?;
        let alerted =
            self.send_handoffs && !taken_over && self.alert(update, message.as_ref()).await?;
        self.store
            .mark_serbero_header_handled(&update.dispute_id, &subject, seconds(update.created_at))
            .await?;
        Ok(Outcome::Relayed { redrawn, alerted })
    }

    /// Saves `update` as the dispute's state unless a later one is stored.
    /// Returns whether it is the latest.
    async fn record_state(&self, update: &HeaderUpdate, subject: &str) -> Result<bool, AlertError> {
        let stored = self.store.serbero_state(&update.dispute_id).await?;
        if !supersedes(update, stored.as_ref()) {
            return Ok(false);
        }
        let state = SerberoState {
            subject: subject.to_owned(),
            created_at: seconds(update.created_at),
        };
        self.store
            .save_serbero_state(&update.dispute_id, &state)
            .await?;
        Ok(true)
    }

    /// Shows the dispute's timeline, with the step just added, on its
    /// message. Edits do not notify, so a failure is logged and not
    /// retried: the step also shows on the next redraw.
    async fn redraw(&self, dispute_id: &str) -> bool {
        match timeline::redraw(self.store, self.telegram, &self.names, dispute_id).await {
            Ok(Some(_)) => true,
            // No message yet: the step shows on the dispute's first alert.
            Ok(None) => false,
            Err(e) => {
                warn!(
                    dispute_id,
                    error = %e,
                    "Failed to show Serbero's update on the dispute message"
                );
                false
            }
        }
    }

    /// Asks the team for a solver when the update needs one and no solver
    /// took the dispute over yet. Returns whether a message was sent.
    async fn alert(
        &self,
        update: &HeaderUpdate,
        message: Option<&StoredMessage>,
    ) -> Result<bool, AlertError> {
        let Some(text) = needs_human_alert(&update.dispute_id, &update.update, update.created_at)
        else {
            return Ok(false);
        };
        // Caught up after the dispute ended (a restart, the first start, a
        // status alert turned off, a message deleted on a cooperative
        // cancel): nobody has to take it over any more.
        let recorded = self.store.dispute_status(&update.dispute_id).await?;
        let resolved = recorded.as_deref().is_some_and(dispute_is_resolved)
            || message.is_some_and(|m| dispute_is_resolved(&m.status));
        if resolved {
            info!(
                dispute_id = %update.dispute_id,
                "Dispute already resolved; no Serbero handoff alert"
            );
            return Ok(false);
        }
        let reply_to = reply_target(message, self.chat_id);
        self.telegram.send(self.chat_id, &text, reply_to).await?;
        Ok(true)
    }
}

/// The dispute's message to reply to. A reply shows the dispute's alert as
/// context; it is only possible in the chat that message lives in.
pub(super) fn reply_target(message: Option<&StoredMessage>, chat_id: i64) -> Option<i32> {
    message
        .filter(|m| m.chat_id == chat_id)
        .map(|m| m.message_id)
}

/// Event times are capped at the time they were read, so they fit.
pub(super) fn seconds(created_at: u64) -> i64 {
    i64::try_from(created_at).unwrap_or(i64::MAX)
}

/// Whether `update` replaces the `stored` state: it is a later stage of the
/// mediation, or the same stage written later. The stage comes first because
/// each subject happens once per dispute and Serbero signs a retried notice
/// when it sends it, so a late `mediating` must never undo a handoff.
pub fn supersedes(update: &HeaderUpdate, stored: Option<&SerberoState>) -> bool {
    let Some(stored) = stored else {
        return true;
    };
    let stored_stage = Update::from_subject(&stored.subject).map_or(0, |u| u.stage());
    (update.update.stage(), seconds(update.created_at)) >= (stored_stage, stored.created_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serbero::dm::parse_dm;
    use crate::serbero::testing::{serbero_dm, Call, FakeTelegram};
    use nostr_sdk::prelude::Keys;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::path::{Path, PathBuf};
    use std::str::FromStr;

    const DISPUTE: &str = "58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a";
    const CHAT: i64 = -100_123;
    const AT: u64 = 1_609_459_200;

    struct Fixture {
        _dir: tempfile::TempDir,
        path: PathBuf,
        store: DisputeMessageStore,
        telegram: FakeTelegram,
    }

    impl Fixture {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("disputes.db");
            let store = DisputeMessageStore::new(&path).await.unwrap();
            Self {
                _dir: dir,
                path,
                store,
                telegram: FakeTelegram::default(),
            }
        }

        fn alerts(&self) -> SerberoAlerts<'_, FakeTelegram> {
            SerberoAlerts {
                store: &self.store,
                telegram: &self.telegram,
                chat_id: CHAT,
                show_progress: true,
                send_handoffs: true,
                send_takeovers: true,
                names: Names::default(),
            }
        }
    }

    fn update(update: Update, created_at: u64) -> HeaderUpdate {
        HeaderUpdate {
            dispute_id: DISPUTE.into(),
            update,
            created_at,
        }
    }

    fn handed_off(reason: &str) -> Update {
        Update::HandedOff {
            reason: Some(reason.into()),
        }
    }

    fn state(subject: &str, created_at: i64) -> SerberoState {
        SerberoState {
            subject: subject.into(),
            created_at,
        }
    }

    /// The texts of the edits made, in order.
    fn edit_texts(telegram: &FakeTelegram) -> Vec<String> {
        telegram
            .edits()
            .into_iter()
            .map(|call| match call {
                Call::Edit { text, .. } => text,
                Call::Send { .. } => unreachable!(),
            })
            .collect()
    }

    #[tokio::test]
    async fn a_handoff_redraws_the_dispute_message_and_replies_with_an_alert() {
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "🔄 *DISPUTE IN PROGRESS*")
            .await
            .unwrap();
        fx.store.set_sent_at(DISPUTE, AT as i64).await.unwrap();

        let outcome = fx
            .alerts()
            .relay(&update(handed_off("conflicting_claims"), AT))
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Relayed {
                redrawn: true,
                alerted: true
            }
        );
        assert!(matches!(
            fx.telegram.calls().as_slice(),
            [
                Call::Edit { chat_id: CHAT, message_id: 42, text },
                Call::Send { chat_id: CHAT, text: alert, reply_to: Some(42) },
            ] if text.contains("*Status:* 🙋 NEEDS A SOLVER · handed off · conflicting claims")
                && text.contains("🙋 `00:00:00` Serbero handed off · conflicting claims")
                && *alert == needs_human_alert(DISPUTE, &handed_off("conflicting_claims"), AT).unwrap()
        ));
        assert_eq!(
            fx.store.serbero_state(DISPUTE).await.unwrap(),
            Some(state("handed off: conflicting_claims", AT as i64))
        );
    }

    #[tokio::test]
    async fn progress_only_redraws_the_dispute_message() {
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "base")
            .await
            .unwrap();
        fx.store.set_sent_at(DISPUTE, AT as i64).await.unwrap();

        let outcome = fx
            .alerts()
            .relay(&update(Update::Mediating, AT))
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Relayed {
                redrawn: true,
                alerted: false
            }
        );
        assert!(matches!(
            fx.telegram.calls().as_slice(),
            [Call::Edit { chat_id: CHAT, message_id: 42, text }]
                if text.contains("*Status:* 🤖 WITH SERBERO · mediating")
                    && text.contains("Serbero mediating")
        ));
    }

    #[tokio::test]
    async fn a_redelivered_header_is_not_relayed_again() {
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "base")
            .await
            .unwrap();
        fx.store.set_sent_at(DISPUTE, AT as i64).await.unwrap();
        let handoff = update(handed_off("flood"), AT);

        fx.alerts().relay(&handoff).await.unwrap();
        let again = fx.alerts().relay(&handoff).await.unwrap();

        assert_eq!(again, Outcome::Duplicate);
        assert_eq!(fx.telegram.sends().len(), 1);
        assert_eq!(fx.telegram.edits().len(), 1);
    }

    #[tokio::test]
    async fn a_restart_does_not_repeat_an_alert() {
        let fx = Fixture::new().await;
        let handoff = update(Update::CouldNotStart, AT);
        fx.alerts().relay(&handoff).await.unwrap();

        // A new process on the same database gets the DM again from the
        // catch-up fetch.
        let store = DisputeMessageStore::new(&fx.path).await.unwrap();
        let telegram = FakeTelegram::default();
        let alerts = SerberoAlerts {
            store: &store,
            telegram: &telegram,
            chat_id: CHAT,
            show_progress: true,
            send_handoffs: true,
            send_takeovers: true,
            names: Names::default(),
        };

        assert_eq!(alerts.relay(&handoff).await.unwrap(), Outcome::Duplicate);
        assert!(telegram.calls().is_empty());
    }

    #[tokio::test]
    async fn a_handoff_for_a_dispute_without_a_message_is_sent_on_its_own() {
        // The watchdog started after the dispute's alert went out.
        let fx = Fixture::new().await;

        let outcome = fx
            .alerts()
            .relay(&update(handed_off("human_requested"), AT))
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Relayed {
                redrawn: false,
                alerted: true
            }
        );
        assert_eq!(
            fx.telegram.calls(),
            vec![Call::Send {
                chat_id: CHAT,
                text: needs_human_alert(DISPUTE, &handed_off("human_requested"), AT).unwrap(),
                reply_to: None,
            }]
        );
        // The state is kept for the dispute's next alert.
        assert_eq!(
            fx.store.serbero_state(DISPUTE).await.unwrap(),
            Some(state("handed off: human_requested", AT as i64))
        );
    }

    #[tokio::test]
    async fn a_handoff_for_a_resolved_dispute_sends_no_alert() {
        // Caught up after the dispute was settled: nobody has to act.
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "settled", "base")
            .await
            .unwrap();

        let outcome = fx
            .alerts()
            .relay(&update(handed_off("conflicting_claims"), AT))
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Relayed {
                redrawn: true,
                alerted: false
            }
        );
        assert_eq!(fx.telegram.sends(), vec![]);
        assert!(
            matches!(
                fx.telegram.edits().as_slice(),
                [Call::Edit { chat_id: CHAT, message_id: 42, text }]
                    if text.contains("Serbero handed off · conflicting claims")
            ),
            "{:?}",
            fx.telegram.edits()
        );
    }

    #[tokio::test]
    async fn a_handoff_after_an_unalerted_resolution_sends_no_alert() {
        // The settled alert was turned off (or the cooperative cancel
        // deleted the message): only the recorded status says it ended.
        let fx = Fixture::new().await;
        fx.store
            .record_dispute_status(DISPUTE, "settled", AT as i64 - 10)
            .await
            .unwrap();

        let outcome = fx
            .alerts()
            .relay(&update(handed_off("conflicting_claims"), AT))
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Relayed {
                redrawn: false,
                alerted: false
            }
        );
        assert_eq!(fx.telegram.calls(), vec![]);
    }

    #[tokio::test]
    async fn a_handoff_after_a_cooperative_cancel_sends_no_alert() {
        // `canceled` deletes the dispute's message, so only the recorded
        // status says the dispute ended.
        let fx = Fixture::new().await;
        fx.store
            .record_dispute_status(DISPUTE, "canceled", AT as i64 - 10)
            .await
            .unwrap();

        let outcome = fx
            .alerts()
            .relay(&update(Update::CouldNotStart, AT))
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Relayed {
                redrawn: false,
                alerted: false
            }
        );
        assert_eq!(fx.telegram.calls(), vec![]);
    }

    #[tokio::test]
    async fn a_message_in_another_chat_gets_a_standalone_alert() {
        // The alert chat changed since the dispute's message was sent.
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, -999, "in-progress", "base")
            .await
            .unwrap();

        fx.alerts()
            .relay(&update(Update::CouldNotStart, AT))
            .await
            .unwrap();

        assert_eq!(
            fx.telegram.sends(),
            vec![Call::Send {
                chat_id: CHAT,
                text: needs_human_alert(DISPUTE, &Update::CouldNotStart, AT).unwrap(),
                reply_to: None,
            }]
        );
    }

    #[tokio::test]
    async fn a_failed_alert_is_retried_on_the_next_delivery() {
        let fx = Fixture::new().await;
        let handoff = update(handed_off("uncertain"), AT);
        fx.telegram.set_down(true);

        let first = fx.alerts().relay(&handoff).await;
        fx.telegram.set_down(false);
        let second = fx.alerts().relay(&handoff).await.unwrap();

        assert!(matches!(first, Err(AlertError::Telegram(_))));
        assert_eq!(
            second,
            Outcome::Relayed {
                redrawn: false,
                alerted: true
            }
        );
        assert_eq!(fx.telegram.sends().len(), 1);
    }

    #[tokio::test]
    async fn a_failed_redraw_does_not_hold_the_alert_back() {
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "base")
            .await
            .unwrap();
        fx.store.set_sent_at(DISPUTE, AT as i64).await.unwrap();
        fx.telegram.set_down(true);

        let outcome = fx.alerts().relay(&update(Update::Mediating, AT)).await;

        // Progress has no alert to retry, so the update counts as relayed.
        assert_eq!(
            outcome.unwrap(),
            Outcome::Relayed {
                redrawn: false,
                alerted: false
            }
        );
    }

    #[tokio::test]
    async fn an_older_update_does_not_replace_a_later_state() {
        // The catch-up fetch returns events in any order. The older step
        // still joins the timeline, in its place.
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "base")
            .await
            .unwrap();
        fx.store.set_sent_at(DISPUTE, AT as i64).await.unwrap();
        fx.alerts()
            .relay(&update(handed_off("round_limit"), AT + 60))
            .await
            .unwrap();

        let outcome = fx
            .alerts()
            .relay(&update(Update::Mediating, AT))
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Relayed {
                redrawn: true,
                alerted: false
            }
        );
        let edits = edit_texts(&fx.telegram);
        assert_eq!(edits.len(), 2);
        assert!(edits[1].contains("*Status:* 🙋 NEEDS A SOLVER · handed off · round limit"));
        let mediating = edits[1].find("Serbero mediating").unwrap();
        let handed_off = edits[1].find("Serbero handed off").unwrap();
        assert!(mediating < handed_off);
        assert_eq!(
            fx.store.serbero_state(DISPUTE).await.unwrap(),
            Some(state("handed off: round_limit", (AT + 60) as i64))
        );
    }

    #[tokio::test]
    async fn turned_off_alerts_still_record_the_state() {
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "base")
            .await
            .unwrap();
        fx.store.set_sent_at(DISPUTE, AT as i64).await.unwrap();
        let alerts = SerberoAlerts {
            show_progress: false,
            send_handoffs: false,
            ..fx.alerts()
        };

        let outcome = alerts
            .relay(&update(handed_off("fraud_signal"), AT))
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Relayed {
                redrawn: false,
                alerted: false
            }
        );
        assert!(fx.telegram.calls().is_empty());
        assert!(fx.store.serbero_state(DISPUTE).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_message_stored_before_the_upgrade_is_redrawn_from_what_is_known() {
        // Its text was never stored; the timeline starts with this step.
        let fx = Fixture::new().await;
        insert_legacy_row(&fx.path, DISPUTE, 42).await;

        let outcome = fx
            .alerts()
            .relay(&update(handed_off("flood"), AT))
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Relayed {
                redrawn: true,
                alerted: true
            }
        );
        assert!(matches!(
            fx.telegram.edits().as_slice(),
            [Call::Edit { message_id: 42, text, .. }] if text.contains("Serbero handed off · flood")
        ));
        assert!(matches!(
            fx.telegram.sends().as_slice(),
            [Call::Send {
                reply_to: Some(42),
                ..
            }]
        ));
    }

    async fn insert_legacy_row(path: &Path, dispute_id: &str, message_id: i32) {
        let options =
            SqliteConnectOptions::from_str(&format!("sqlite:{}", path.display())).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO dispute_messages (dispute_id, message_id, chat_id, status, created_at, updated_at) \
             VALUES (?, ?, ?, 'in-progress', 1, 1)",
        )
        .bind(dispute_id)
        .bind(message_id)
        .bind(CHAT)
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn nothing_after_the_header_of_a_full_brief_reaches_telegram_or_disk() {
        // A watchdog registered as a solver by mistake receives full briefs.
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "base")
            .await
            .unwrap();
        fx.store.set_sent_at(DISPUTE, AT as i64).await.unwrap();
        let serbero = Keys::generate();
        let watchdog = Keys::generate();
        let brief = format!(
            "Dispute {DISPUTE} · handed off: conflicting_claims\n\
             Buyer — says sent (0.96)\n  \"PARTY-SECRET ya envié el pago desde mi cuenta\"\n\
             Transcript (2 messages) follows in the next message."
        );
        let event = serbero_dm(&serbero, watchdog.public_key(), Some(DISPUTE), &brief, None);

        let dm = parse_dm(&event, &watchdog, &serbero.public_key()).expect("accepted");
        let outcome = fx
            .alerts()
            .relay(&dm.into_update().expect("relayed subject"))
            .await
            .unwrap();
        fx.store.close().await;

        assert_eq!(
            outcome,
            Outcome::Relayed {
                redrawn: true,
                alerted: true
            }
        );
        for call in fx.telegram.calls() {
            assert!(!format!("{call:?}").contains("PARTY-SECRET"), "{call:?}");
            assert!(!format!("{call:?}").contains("Buyer"), "{call:?}");
        }
        for entry in std::fs::read_dir(fx.path.parent().unwrap()).unwrap() {
            let bytes = std::fs::read(entry.unwrap().path()).unwrap();
            assert!(
                !bytes.windows(12).any(|w| w == b"PARTY-SECRET"),
                "the brief's body was written to disk"
            );
        }
    }

    #[test]
    fn a_later_stage_supersedes_whatever_its_date() {
        let stored = state("mediating", 100);

        assert!(supersedes(&update(handed_off("flood"), 101), Some(&stored)));
        assert!(supersedes(&update(handed_off("flood"), 99), Some(&stored)));
        assert!(supersedes(&update(Update::Mediating, 5), None));
    }

    #[test]
    fn a_late_notice_never_moves_a_dispute_back() {
        // Serbero signs a retried notice when it sends it, so a `mediating`
        // that failed earlier can arrive after the handoff, with a later date.
        let stored = state("handed off: conflicting_claims", 1000);

        assert!(!supersedes(&update(Update::Mediating, 1100), Some(&stored)));
        assert!(!supersedes(
            &update(Update::GuidanceSent { path: None }, 1100),
            Some(&stored)
        ));
    }

    #[test]
    fn within_a_stage_the_later_update_wins() {
        let handed = state("handed off: flood", 100);

        assert!(supersedes(
            &update(Update::CouldNotStart, 200),
            Some(&handed)
        ));
        assert!(!supersedes(
            &update(Update::CouldNotStart, 50),
            Some(&handed)
        ));
        // The same update again (a retry) is still the latest.
        assert!(supersedes(&update(handed_off("flood"), 100), Some(&handed)));
    }

    #[tokio::test]
    async fn a_late_mediating_notice_keeps_the_handoff_on_the_message() {
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "base")
            .await
            .unwrap();
        fx.store.set_sent_at(DISPUTE, AT as i64).await.unwrap();
        fx.alerts()
            .relay(&update(handed_off("conflicting_claims"), AT))
            .await
            .unwrap();

        let outcome = fx
            .alerts()
            .relay(&update(Update::Mediating, AT + 100))
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Relayed {
                redrawn: true,
                alerted: false
            }
        );
        let edits = edit_texts(&fx.telegram);
        assert_eq!(edits.len(), 2);
        // Shown on the timeline, after the handoff, without the header
        // asking for Serbero again.
        assert!(edits[1].contains("*Status:* 🙋 NEEDS A SOLVER · handed off · conflicting claims"));
        assert!(edits[1]
            .contains("Serbero handed off · conflicting claims\n🤖 `00:01:40` Serbero mediating"));
        assert_eq!(
            fx.store.serbero_state(DISPUTE).await.unwrap(),
            Some(state("handed off: conflicting_claims", AT as i64))
        );
    }

    #[tokio::test]
    async fn a_handoff_after_a_takeover_sends_no_alert() {
        // Caught up after a solver already took the dispute over.
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "base")
            .await
            .unwrap();
        fx.store.set_sent_at(DISPUTE, AT as i64).await.unwrap();
        fx.store.record_takeover(DISPUTE, AT as i64).await.unwrap();

        let outcome = fx
            .alerts()
            .relay(&update(handed_off("conflicting_claims"), AT - 10))
            .await
            .unwrap();

        assert_eq!(
            outcome,
            Outcome::Relayed {
                redrawn: true,
                alerted: false
            }
        );
        assert!(matches!(
            fx.telegram.calls().as_slice(),
            [Call::Edit { chat_id: CHAT, message_id: 42, text }]
                if text.contains("Serbero handed off · conflicting claims")
        ));
    }
}
