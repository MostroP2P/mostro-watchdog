//! Relaying Serbero's updates: what to record, which dispute message to
//! redraw and when to ask the team for a solver.

use super::dm::{HeaderUpdate, Update};
use super::render::{
    dispute_is_open, dispute_is_resolved, needs_human_alert, status_line, with_status_line,
};
use super::telegram::Messenger;
use crate::db::{DisputeMessageStore, SerberoState, StoredMessage};
use tracing::{error, info, warn};

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
        let is_latest = self.record_state(update, &subject).await?;
        let message = self.store.get_message(&update.dispute_id).await?;
        let redrawn =
            is_latest && self.show_progress && self.redraw(update, message.as_ref()).await;
        let alerted = self.send_handoffs && self.alert(update, message.as_ref()).await?;
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

    /// Shows `update` on the dispute's message. Edits do not notify, so a
    /// failure is logged and not retried: the line also shows on the
    /// dispute's next alert.
    async fn redraw(&self, update: &HeaderUpdate, message: Option<&StoredMessage>) -> bool {
        // No message yet: the state shows on the dispute's first alert.
        let Some(message) = message else {
            return false;
        };
        let Some(base) = message.text.as_deref() else {
            info!(
                dispute_id = %update.dispute_id,
                "Dispute message predates Serbero alerts; Serbero's state shows from its next update"
            );
            return false;
        };
        let line = status_line(&update.update, dispute_is_open(&message.status));
        let text = with_status_line(base, Some(&line));
        match self
            .telegram
            .edit(message.chat_id, message.message_id, &text)
            .await
        {
            Ok(()) => true,
            Err(e) => {
                warn!(
                    dispute_id = %update.dispute_id,
                    error = %e,
                    "Failed to show Serbero's state on the dispute message"
                );
                false
            }
        }
    }

    /// Asks the team for a solver when the update needs one. Returns whether
    /// a message was sent.
    async fn alert(
        &self,
        update: &HeaderUpdate,
        message: Option<&StoredMessage>,
    ) -> Result<bool, AlertError> {
        let Some(text) = needs_human_alert(&update.dispute_id, &update.update, update.created_at)
        else {
            return Ok(false);
        };
        // Caught up after the dispute ended (a restart, the first start):
        // nobody has to take it over any more.
        if message.is_some_and(|m| dispute_is_resolved(&m.status)) {
            info!(
                dispute_id = %update.dispute_id,
                "Dispute already resolved; no Serbero handoff alert"
            );
            return Ok(false);
        }
        // A reply shows the dispute's alert as context; it is only possible
        // in the chat that message lives in.
        let reply_to = message
            .filter(|m| m.chat_id == self.chat_id)
            .map(|m| m.message_id);
        self.telegram.send(self.chat_id, &text, reply_to).await?;
        Ok(true)
    }
}

/// Event times are capped at the time they were read, so they fit.
fn seconds(created_at: u64) -> i64 {
    i64::try_from(created_at).unwrap_or(i64::MAX)
}

/// Whether `update` replaces the `stored` state: it was written later, or
/// in the same second at a later stage of the mediation.
pub fn supersedes(update: &HeaderUpdate, stored: Option<&SerberoState>) -> bool {
    let Some(stored) = stored else {
        return true;
    };
    let stored_stage = Update::from_subject(&stored.subject).map_or(0, |u| u.stage());
    (seconds(update.created_at), update.update.stage()) >= (stored.created_at, stored_stage)
}

/// `base` with Serbero's latest state for the dispute appended, when there
/// is one and progress is shown. A store error leaves `base` unchanged: the
/// dispute alert matters more than Serbero's line.
pub async fn decorate(
    store: &DisputeMessageStore,
    dispute_id: &str,
    status: &str,
    base: &str,
    show_progress: bool,
) -> String {
    if !show_progress {
        return base.to_owned();
    }
    let update = match store.serbero_state(dispute_id).await {
        Ok(state) => state.and_then(|s| Update::from_subject(&s.subject)),
        Err(e) => {
            error!(dispute_id, error = %e, "Failed to read Serbero's state for the dispute");
            None
        }
    };
    let line = update.map(|u| status_line(&u, dispute_is_open(status)));
    with_status_line(base, line.as_deref())
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

    #[tokio::test]
    async fn a_handoff_redraws_the_dispute_message_and_replies_with_an_alert() {
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "🔄 *DISPUTE IN PROGRESS*")
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
                alerted: true
            }
        );
        assert_eq!(
            fx.telegram.calls(),
            vec![
                Call::Edit {
                    chat_id: CHAT,
                    message_id: 42,
                    text: "🔄 *DISPUTE IN PROGRESS*\n\n🙋 *Serbero:* handed off \\(conflicting claims\\) — a solver must take it over".into(),
                },
                Call::Send {
                    chat_id: CHAT,
                    text: needs_human_alert(DISPUTE, &handed_off("conflicting_claims"), AT)
                        .unwrap(),
                    reply_to: Some(42),
                },
            ]
        );
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
        assert_eq!(
            fx.telegram.calls(),
            vec![Call::Edit {
                chat_id: CHAT,
                message_id: 42,
                text: "base\n\n🤖 *Serbero:* mediating".into(),
            }]
        );
    }

    #[tokio::test]
    async fn a_redelivered_header_is_not_relayed_again() {
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "base")
            .await
            .unwrap();
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
        assert_eq!(
            fx.telegram.edits(),
            vec![Call::Edit {
                chat_id: CHAT,
                message_id: 42,
                text: "base\n\n🙋 *Serbero:* handed off \\(conflicting claims\\)".into(),
            }]
        );
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
        // The catch-up fetch returns events in any order.
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "base")
            .await
            .unwrap();
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
                redrawn: false,
                alerted: false
            }
        );
        assert_eq!(fx.telegram.edits().len(), 1);
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
    async fn a_message_stored_before_the_upgrade_is_not_redrawn() {
        // Its text was never stored, so redrawing it would lose the alert.
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
                redrawn: false,
                alerted: true
            }
        );
        assert_eq!(fx.telegram.edits(), vec![]);
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
    fn a_later_update_supersedes_the_stored_state() {
        let stored = state("mediating", 100);

        assert!(supersedes(&update(handed_off("flood"), 101), Some(&stored)));
        assert!(!supersedes(&update(handed_off("flood"), 99), Some(&stored)));
        assert!(supersedes(&update(Update::Mediating, 5), None));
    }

    #[test]
    fn in_the_same_second_the_later_stage_wins() {
        let mediating = state("mediating", 100);
        let handed = state("handed off: flood", 100);

        assert!(supersedes(
            &update(handed_off("flood"), 100),
            Some(&mediating)
        ));
        assert!(!supersedes(&update(Update::Mediating, 100), Some(&handed)));
        // The same update again (a retry) is still the latest.
        assert!(supersedes(&update(handed_off("flood"), 100), Some(&handed)));
    }

    #[tokio::test]
    async fn dispute_alerts_show_serberos_latest_state() {
        let fx = Fixture::new().await;
        fx.store
            .save_serbero_state(DISPUTE, &state("handed off: conflicting_claims", 5))
            .await
            .unwrap();

        let open = decorate(&fx.store, DISPUTE, "in-progress", "base", true).await;
        let closed = decorate(&fx.store, DISPUTE, "settled", "base", true).await;

        assert_eq!(
            open,
            "base\n\n🙋 *Serbero:* handed off \\(conflicting claims\\) — a solver must take it over"
        );
        assert_eq!(
            closed,
            "base\n\n🙋 *Serbero:* handed off \\(conflicting claims\\)"
        );
    }

    #[tokio::test]
    async fn dispute_alerts_are_unchanged_without_serbero() {
        let fx = Fixture::new().await;

        assert_eq!(
            decorate(&fx.store, DISPUTE, "initiated", "base", true).await,
            "base"
        );
        fx.store
            .save_serbero_state(DISPUTE, &state("mediating", 5))
            .await
            .unwrap();
        assert_eq!(
            decorate(&fx.store, DISPUTE, "in-progress", "base", false).await,
            "base"
        );
    }
}
