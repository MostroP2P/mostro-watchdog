//! Relaying Serbero's updates: what to record and which dispute message to
//! redraw. Nothing here sends a new message: a Serbero update is a step on
//! the dispute's timeline, shown by editing the dispute's one message.

use super::dm::{HeaderUpdate, Update};
use super::telegram::Messenger;
use crate::db::{DisputeMessageStore, SerberoState};
use crate::timeline::{self, EntryKind, Names, RedrawError};
use tracing::info;

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
    /// Recorded. `redrawn` when the dispute's message now shows it.
    Relayed { redrawn: bool },
}

/// Relays Serbero updates to Telegram.
pub struct SerberoAlerts<'a, M> {
    pub store: &'a DisputeMessageStore,
    pub telegram: &'a M,
    /// `[alerts] serbero_progress`: show Serbero's state on the dispute's
    /// message.
    pub show_progress: bool,
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
    /// is marked as relayed only once the dispute's message shows it, so a
    /// redraw Telegram rejected is retried when the DM arrives again (an
    /// early catch-up, the next periodic one, a restart). The timeline step
    /// is kept once however many times that happens.
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
            self.redraw(&update.dispute_id).await?
        };
        self.store
            .mark_serbero_header_handled(&update.dispute_id, &subject, seconds(update.created_at))
            .await?;
        Ok(Outcome::Relayed { redrawn })
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
    /// message. The message is the only place the step shows, so a failed
    /// edit is an error: the update stays unrelayed and is retried.
    async fn redraw(&self, dispute_id: &str) -> Result<bool, AlertError> {
        match timeline::redraw(self.store, self.telegram, &self.names, dispute_id).await {
            Ok(Some(_)) => Ok(true),
            // No message yet: the step shows on the dispute's first alert.
            Ok(None) => {
                info!(
                    dispute_id,
                    "No dispute message yet; Serbero's step waits for it"
                );
                Ok(false)
            }
            Err(RedrawError::Store(e)) => Err(AlertError::Store(e)),
            Err(RedrawError::Telegram(e)) => Err(AlertError::Telegram(e)),
        }
    }
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
                show_progress: true,
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
    async fn a_handoff_redraws_the_dispute_message_and_sends_nothing_new() {
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

        assert_eq!(outcome, Outcome::Relayed { redrawn: true });
        assert!(matches!(
            fx.telegram.calls().as_slice(),
            [Call::Edit { chat_id: CHAT, message_id: 42, text }]
                if text.contains("*Status:* 🙋 NEEDS A SOLVER · handed off · conflicting claims")
                    && text.contains("🙋 `00:00:00` Serbero handed off · conflicting claims")
        ));
        assert_eq!(
            fx.store.serbero_state(DISPUTE).await.unwrap(),
            Some(state("handed off: conflicting_claims", AT as i64))
        );
    }

    #[tokio::test]
    async fn no_serbero_update_sends_a_new_message() {
        // Everything Serbero reports is a step on the dispute's message,
        // with or without that message; nothing else reaches the channel.
        let updates = [
            Update::Mediating,
            Update::GuidanceSent {
                path: Some("payment_arrived".into()),
            },
            handed_off("conflicting_claims"),
            Update::CouldNotStart,
        ];
        for (i, kind) in updates.into_iter().enumerate() {
            let fx = Fixture::new().await;
            if i % 2 == 0 {
                fx.store
                    .insert(DISPUTE, 42, CHAT, "in-progress", "base")
                    .await
                    .unwrap();
                fx.store.set_sent_at(DISPUTE, AT as i64).await.unwrap();
            }

            fx.alerts().relay(&update(kind, AT)).await.unwrap();

            assert!(fx.telegram.sends().is_empty(), "{:?}", fx.telegram.calls());
        }
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

        assert_eq!(outcome, Outcome::Relayed { redrawn: true });
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
        assert!(fx.telegram.sends().is_empty());
        assert_eq!(fx.telegram.edits().len(), 1);
    }

    #[tokio::test]
    async fn a_restart_does_not_relay_a_header_again() {
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
            show_progress: true,
            names: Names::default(),
        };

        assert_eq!(alerts.relay(&handoff).await.unwrap(), Outcome::Duplicate);
        assert!(telegram.calls().is_empty());
    }

    #[tokio::test]
    async fn a_handoff_for_a_dispute_without_a_message_only_records_the_state() {
        // The watchdog started after the dispute's alert went out. No
        // message of its own goes out: the channel is the dispute's message.
        let fx = Fixture::new().await;

        let outcome = fx
            .alerts()
            .relay(&update(handed_off("human_requested"), AT))
            .await
            .unwrap();

        assert_eq!(outcome, Outcome::Relayed { redrawn: false });
        assert!(fx.telegram.calls().is_empty());
        // The state is kept for the dispute's next alert.
        assert_eq!(
            fx.store.serbero_state(DISPUTE).await.unwrap(),
            Some(state("handed off: human_requested", AT as i64))
        );
    }

    #[tokio::test]
    async fn a_handoff_for_a_resolved_dispute_only_redraws() {
        // Caught up after the dispute was settled: the step joins the
        // timeline and nothing else happens.
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

        assert_eq!(outcome, Outcome::Relayed { redrawn: true });
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
    async fn a_failed_redraw_is_retried_on_the_next_delivery() {
        // The message is the only place a handoff shows, so an edit that
        // Telegram rejected leaves the update unrelayed; the next delivery
        // (an early catch-up) edits again, and the step is kept once.
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "base")
            .await
            .unwrap();
        fx.store.set_sent_at(DISPUTE, AT as i64).await.unwrap();
        let handoff = update(handed_off("uncertain"), AT);
        fx.telegram.set_down(true);

        let first = fx.alerts().relay(&handoff).await;
        fx.telegram.set_down(false);
        let second = fx.alerts().relay(&handoff).await.unwrap();

        assert!(matches!(first, Err(AlertError::Telegram(_))), "{first:?}");
        assert!(
            !fx.store
                .serbero_header_handled(DISPUTE, "handed off: uncertain")
                .await
                .unwrap()
                || second != Outcome::Duplicate
        );
        assert_eq!(second, Outcome::Relayed { redrawn: true });
        assert!(fx.telegram.sends().is_empty());
        assert!(matches!(
            fx.telegram.calls().as_slice(),
            [Call::Edit { message_id: 42, text, .. }]
                if text.matches("Serbero handed off · uncertain").count() == 1
        ));
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

        assert_eq!(outcome, Outcome::Relayed { redrawn: true });
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
    async fn turned_off_progress_still_records_the_state() {
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "base")
            .await
            .unwrap();
        fx.store.set_sent_at(DISPUTE, AT as i64).await.unwrap();
        let alerts = SerberoAlerts {
            show_progress: false,
            ..fx.alerts()
        };

        let outcome = alerts
            .relay(&update(handed_off("fraud_signal"), AT))
            .await
            .unwrap();

        assert_eq!(outcome, Outcome::Relayed { redrawn: false });
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

        assert_eq!(outcome, Outcome::Relayed { redrawn: true });
        assert!(matches!(
            fx.telegram.calls().as_slice(),
            [Call::Edit { message_id: 42, text, .. }] if text.contains("Serbero handed off · flood")
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

        assert_eq!(outcome, Outcome::Relayed { redrawn: true });
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

        assert_eq!(outcome, Outcome::Relayed { redrawn: true });
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
}
