//! Spotting a solver taking a dispute over from Serbero.
//!
//! Mostro's kind-38386 events name no solver, so a takeover shows only as a
//! second, later `in-progress` revision of the dispute. Serbero is a
//! read-only solver and Mostro lets no read-only solver take an
//! `in-progress` dispute, so a later `in-progress` for a dispute Serbero
//! reported on means a person took it over from Serbero.

use mostro_core::dispute::Status;
use tracing::{error, info, warn};

use super::alerts::{reply_target, seconds, AlertError, SerberoAlerts};
use super::dm::Update;
use super::render::{status_line, takeover_alert, with_status_line, DisputeStage};
use super::telegram::Messenger;
use crate::db::{RecordedStatus, StoredMessage};

/// `handle_dispute_event`'s placeholder for an event without a `d` tag.
const UNKNOWN_DISPUTE: &str = "unknown";

impl<M: Messenger> SerberoAlerts<'_, M> {
    /// Records a dispute's kind-38386 status, so a late Serbero handoff
    /// knows the dispute already ended, and announces a solver taking the
    /// dispute over from Serbero. Called for every dispute event, before
    /// the alert toggles. Errors are logged: the dispute's own alert goes
    /// out regardless.
    pub async fn note_dispute_status(&self, dispute_id: &str, status: &str, created_at: u64) {
        if dispute_id == UNKNOWN_DISPUTE {
            return;
        }
        if let Err(e) = self.track_status(dispute_id, status, created_at).await {
            error!(dispute_id, error = %e, "Failed to track the dispute's status");
        }
    }

    async fn track_status(
        &self,
        dispute_id: &str,
        status: &str,
        created_at: u64,
    ) -> Result<(), AlertError> {
        let at = seconds(created_at);
        let previous = self.store.recorded_status(dispute_id).await?;
        self.store
            .record_dispute_status(dispute_id, status, at)
            .await?;
        if !is_takeover(previous.as_ref(), status, at) {
            return Ok(());
        }
        let Some(state) = self.store.serbero_state(dispute_id).await? else {
            return Ok(());
        };
        if !self.store.record_takeover(dispute_id, at).await? {
            return Ok(());
        }
        info!(dispute_id, "A solver took the dispute over from Serbero");
        let message = self.store.get_message(dispute_id).await?;
        if self.show_progress {
            if let (Some(update), Some(message)) =
                (Update::from_subject(&state.subject), message.as_ref())
            {
                self.redraw_taken_over(dispute_id, &update, message).await;
            }
        }
        // Recorded before sending: a takeover is seen once, so a failed
        // send cannot be retried, but the dispute's message still shows it.
        if self.send_handoffs {
            let reply_to = reply_target(message.as_ref(), self.chat_id);
            self.telegram
                .send(
                    self.chat_id,
                    &takeover_alert(dispute_id, created_at),
                    reply_to,
                )
                .await?;
        }
        Ok(())
    }

    /// Makes Serbero's line on the dispute's message say a solver took it
    /// over. Done here because the dispute's own alert may be turned off for
    /// `in-progress`, leaving the message asking for a solver. Edits do not
    /// notify, so a failure is logged: the line also shows on the dispute's
    /// next alert.
    async fn redraw_taken_over(&self, dispute_id: &str, update: &Update, message: &StoredMessage) {
        // Stored before Serbero alerts: redrawing it would lose the alert.
        let Some(base) = message.text.as_deref() else {
            return;
        };
        let line = status_line(update, DisputeStage::TakenOver);
        let text = with_status_line(base, Some(&line));
        if let Err(e) = self
            .telegram
            .edit(message.chat_id, message.message_id, &text)
            .await
        {
            warn!(
                dispute_id,
                error = %e,
                "Failed to show the takeover on the dispute message"
            );
        }
    }
}

/// Whether `status`, written at `created_at`, is a later `in-progress`
/// revision of a dispute already `in-progress`: a new solver took it. A
/// redelivered revision has the same time and is not a takeover.
pub fn is_takeover(previous: Option<&RecordedStatus>, status: &str, created_at: i64) -> bool {
    let in_progress = |s: &str| matches!(s.parse(), Ok(Status::InProgress));
    in_progress(status)
        && previous.is_some_and(|p| in_progress(&p.status) && created_at > p.created_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DisputeMessageStore, SerberoState};
    use crate::serbero::testing::{Call, FakeTelegram};

    const DISPUTE: &str = "58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a";
    const CHAT: i64 = -100_123;
    /// Serbero took the dispute.
    const TAKEN: u64 = 1_609_459_200;
    /// A person took it over.
    const TAKEN_OVER: u64 = TAKEN + 600;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: DisputeMessageStore,
        telegram: FakeTelegram,
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

        /// Serbero took the dispute and handed it off; its message shows it.
        async fn handed_off(&self) {
            self.store
                .insert(DISPUTE, 42, CHAT, "in-progress", "base")
                .await
                .unwrap();
            self.alerts()
                .note_dispute_status(DISPUTE, "in-progress", TAKEN)
                .await;
            self.store
                .save_serbero_state(
                    DISPUTE,
                    &SerberoState {
                        subject: "handed off: conflicting_claims".into(),
                        created_at: TAKEN as i64 + 60,
                    },
                )
                .await
                .unwrap();
        }
    }

    /// The dispute's message once Serbero's line says a solver took over.
    fn taken_over_redraw() -> Call {
        Call::Edit {
            chat_id: CHAT,
            message_id: 42,
            text:
                "base\n\n🙋 *Serbero:* handed off \\(conflicting claims\\) — a solver took it over"
                    .into(),
        }
    }

    fn recorded(status: &str, created_at: i64) -> RecordedStatus {
        RecordedStatus {
            status: status.into(),
            created_at,
        }
    }

    #[test]
    fn a_later_in_progress_revision_is_a_takeover() {
        assert!(is_takeover(
            Some(&recorded("in-progress", 10)),
            "in-progress",
            20
        ));
    }

    #[test]
    fn the_first_take_and_redeliveries_are_not_takeovers() {
        // Serbero (or anyone) taking a waiting dispute.
        assert!(!is_takeover(
            Some(&recorded("initiated", 10)),
            "in-progress",
            20
        ));
        assert!(!is_takeover(None, "in-progress", 20));
        // The same revision again, or an older one replayed by a relay.
        assert!(!is_takeover(
            Some(&recorded("in-progress", 20)),
            "in-progress",
            20
        ));
        assert!(!is_takeover(
            Some(&recorded("in-progress", 20)),
            "in-progress",
            10
        ));
        // A resolution is not a takeover.
        assert!(!is_takeover(
            Some(&recorded("in-progress", 10)),
            "settled",
            20
        ));
    }

    #[tokio::test]
    async fn a_solver_taking_over_from_serbero_is_announced_in_reply() {
        let fx = Fixture::new().await;
        fx.handed_off().await;

        fx.alerts()
            .note_dispute_status(DISPUTE, "in-progress", TAKEN_OVER)
            .await;

        assert_eq!(
            fx.telegram.calls(),
            vec![
                taken_over_redraw(),
                Call::Send {
                    chat_id: CHAT,
                    text: takeover_alert(DISPUTE, TAKEN_OVER),
                    reply_to: Some(42),
                },
            ]
        );
        assert!(fx.store.taken_over(DISPUTE).await.unwrap());
    }

    #[tokio::test]
    async fn turned_off_progress_leaves_the_dispute_message_alone() {
        let fx = Fixture::new().await;
        fx.handed_off().await;
        let alerts = SerberoAlerts {
            show_progress: false,
            ..fx.alerts()
        };

        alerts
            .note_dispute_status(DISPUTE, "in-progress", TAKEN_OVER)
            .await;

        assert_eq!(fx.telegram.edits(), vec![]);
        assert_eq!(fx.telegram.sends().len(), 1);
    }

    #[tokio::test]
    async fn a_failed_redraw_keeps_the_takeover_recorded() {
        // The line also shows on the dispute's next alert.
        let fx = Fixture::new().await;
        fx.handed_off().await;
        let alerts = SerberoAlerts {
            send_handoffs: false,
            ..fx.alerts()
        };
        fx.telegram.set_down(true);

        alerts
            .note_dispute_status(DISPUTE, "in-progress", TAKEN_OVER)
            .await;

        assert!(fx.store.taken_over(DISPUTE).await.unwrap());
    }

    #[tokio::test]
    async fn a_redelivered_takeover_is_announced_once() {
        // A relay reconnect delivers the same revision again.
        let fx = Fixture::new().await;
        fx.handed_off().await;

        fx.alerts()
            .note_dispute_status(DISPUTE, "in-progress", TAKEN_OVER)
            .await;
        fx.alerts()
            .note_dispute_status(DISPUTE, "in-progress", TAKEN_OVER)
            .await;

        assert_eq!(fx.telegram.sends().len(), 1);
    }

    #[tokio::test]
    async fn a_dispute_serbero_never_reported_on_is_not_a_takeover() {
        let fx = Fixture::new().await;
        fx.alerts()
            .note_dispute_status(DISPUTE, "in-progress", TAKEN)
            .await;

        fx.alerts()
            .note_dispute_status(DISPUTE, "in-progress", TAKEN_OVER)
            .await;

        assert_eq!(fx.telegram.calls(), vec![]);
        assert!(!fx.store.taken_over(DISPUTE).await.unwrap());
    }

    #[tokio::test]
    async fn turned_off_handoff_alerts_still_redraw_the_dispute_message() {
        // Serbero's line must stop asking for a solver either way, also when
        // the `in-progress` alert is off and nothing else edits the message.
        let fx = Fixture::new().await;
        fx.handed_off().await;
        let alerts = SerberoAlerts {
            send_handoffs: false,
            ..fx.alerts()
        };

        alerts
            .note_dispute_status(DISPUTE, "in-progress", TAKEN_OVER)
            .await;

        assert_eq!(fx.telegram.calls(), vec![taken_over_redraw()]);
        assert!(fx.store.taken_over(DISPUTE).await.unwrap());
    }

    #[tokio::test]
    async fn a_takeover_without_a_dispute_message_is_sent_on_its_own() {
        // The `in-progress` alert is turned off, or the alert chat changed.
        let fx = Fixture::new().await;
        fx.handed_off().await;
        fx.store.delete(DISPUTE).await.unwrap();

        fx.alerts()
            .note_dispute_status(DISPUTE, "in-progress", TAKEN_OVER)
            .await;

        assert!(matches!(
            fx.telegram.sends().as_slice(),
            [Call::Send { reply_to: None, .. }]
        ));
    }

    #[tokio::test]
    async fn dispute_statuses_are_recorded_for_late_handoffs() {
        let fx = Fixture::new().await;

        fx.alerts()
            .note_dispute_status(DISPUTE, "settled", TAKEN)
            .await;

        assert_eq!(
            fx.store.dispute_status(DISPUTE).await.unwrap().as_deref(),
            Some("settled")
        );
    }

    #[tokio::test]
    async fn an_event_without_a_dispute_id_records_nothing() {
        let fx = Fixture::new().await;

        fx.alerts()
            .note_dispute_status(UNKNOWN_DISPUTE, "settled", TAKEN)
            .await;

        assert_eq!(
            fx.store.dispute_status(UNKNOWN_DISPUTE).await.unwrap(),
            None
        );
    }
}
