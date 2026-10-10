//! Serbero alerts: relay to Telegram what Serbero, Mostro's dispute
//! assistant, reports about each dispute.
//!
//! Serbero mediates disputes as a read-only solver and hands them to human
//! solvers when needed. With the watchdog's key in its `[[observers]]`, it
//! writes the watchdog one line per mediation step (serbero
//! `docs/messages.md` §3): `mediating`, `mediation could not start`,
//! `handed off: <reason>` and `guidance sent: <path>`. The watchdog shows
//! that state on the dispute's Telegram message and sends a new message,
//! which notifies, when a solver must take the dispute over, and another
//! when a solver did take it over from Serbero (`takeover`).
//!
//! Only the first line of a DM is ever read. If Serbero sends the watchdog
//! full solver messages (registered as a solver by mistake), the rest,
//! which may quote the parties, is dropped and never stored, logged or
//! forwarded.

pub mod alerts;
pub mod discovery;
pub mod dm;
pub mod render;
pub mod sync;
pub mod takeover;
pub mod telegram;
#[cfg(test)]
pub(crate) mod testing;

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::{mpsc, Notify, RwLock};
use tracing::{debug, error, info, warn};

use crate::config::SerberoSettings;
use alerts::{Outcome, SerberoAlerts};
use dm::parse_dm;
use sync::{SerberoSync, TrustedSerbero};
use telegram::Messenger;

/// The Serbero side of the event loop: opens DMs with the watchdog's keys
/// and relays the ones written by the trusted Serbero.
pub struct SerberoInbox {
    keys: Keys,
    trusted: TrustedSerbero,
    /// Wakes the sync task for an early catch-up when relaying failed.
    retry: Arc<Notify>,
    warned_full_messages: bool,
}

impl SerberoInbox {
    pub fn new(keys: Keys, trusted: TrustedSerbero, retry: Arc<Notify>) -> Self {
        Self {
            keys,
            trusted,
            retry,
            warned_full_messages: false,
        }
    }

    /// Relays `event` when it is a DM from the trusted Serbero; `None` when
    /// it is not one the watchdog acts on. Logs name the dispute and the
    /// subject, never the text.
    pub async fn receive<M: Messenger>(
        &mut self,
        event: &Event,
        alerts: &SerberoAlerts<'_, M>,
    ) -> Option<Outcome> {
        let serbero = (*self.trusted.read().await)?;
        let dm = parse_dm(event, &self.keys, &serbero)?;
        if dm.full_message && !self.warned_full_messages {
            warn!(
                "Serbero sends this watchdog full solver messages; only their first line is used. \
                 List the watchdog's key in Serbero's [[observers]], not as a solver"
            );
            self.warned_full_messages = true;
        }
        let update = dm.into_update()?;
        let subject = update.update.subject();
        match alerts.relay(&update).await {
            Ok(outcome) => {
                match outcome {
                    Outcome::Duplicate => debug!(
                        dispute_id = %update.dispute_id,
                        subject = %subject,
                        "Serbero update already relayed"
                    ),
                    Outcome::Relayed { redrawn, alerted } => info!(
                        dispute_id = %update.dispute_id,
                        subject = %subject,
                        redrawn,
                        alerted,
                        "🤖 Serbero update relayed"
                    ),
                }
                Some(outcome)
            }
            Err(e) => {
                error!(
                    dispute_id = %update.dispute_id,
                    subject = %subject,
                    error = %e,
                    "Failed to relay a Serbero update; an early catch-up retries it"
                );
                self.retry.notify_one();
                None
            }
        }
    }

    /// Relays caught-up DMs oldest first, so Serbero's steps show in order.
    pub async fn receive_batch<M: Messenger>(
        &mut self,
        mut events: Vec<Event>,
        alerts: &SerberoAlerts<'_, M>,
    ) {
        events.sort_by_key(|event| (event.created_at, event.id));
        for event in &events {
            self.receive(event, alerts).await;
        }
    }
}

/// Starts keeping up with Serbero in the background. Returns the inbox for
/// the event loop and the channel caught-up DMs arrive on.
pub fn start(
    client: &Client,
    settings: SerberoSettings,
    mostro: PublicKey,
    relays_changed: Arc<Notify>,
    nip65_refresh: Duration,
) -> (SerberoInbox, mpsc::UnboundedReceiver<Vec<Event>>) {
    let trusted: TrustedSerbero = Arc::new(RwLock::new(None));
    let retry = Arc::new(Notify::new());
    let (backlog, caught_up) = mpsc::unbounded_channel();
    SerberoSync {
        client: client.clone(),
        watchdog: settings.keys.public_key(),
        mostro,
        configured: settings.pubkey,
        trusted: trusted.clone(),
        backlog,
        relays_changed,
        refresh: sync::sync_interval(nip65_refresh),
        retry: retry.clone(),
        first_retry: sync::FIRST_RETRY_DELAY,
        no_serbero: 0,
    }
    .spawn();
    (SerberoInbox::new(settings.keys, trusted, retry), caught_up)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DisputeMessageStore;
    use crate::serbero::testing::{serbero_dm, Call, FakeTelegram};
    use crate::timeline::Names;

    const DISPUTE: &str = "58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a";
    const CHAT: i64 = -100_123;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: DisputeMessageStore,
        telegram: FakeTelegram,
        serbero: Keys,
        watchdog: Keys,
        retry: Arc<Notify>,
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
                serbero: Keys::generate(),
                watchdog: Keys::generate(),
                retry: Arc::new(Notify::new()),
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

        fn inbox(&self, trusted: Option<PublicKey>) -> SerberoInbox {
            SerberoInbox::new(
                self.watchdog.clone(),
                Arc::new(RwLock::new(trusted)),
                self.retry.clone(),
            )
        }

        fn dm(&self, subject: &str, created_at: u64) -> Event {
            serbero_dm(
                &self.serbero,
                self.watchdog.public_key(),
                Some(DISPUTE),
                &format!("Dispute {DISPUTE} · {subject}"),
                Some(Timestamp::from_secs(created_at)),
            )
        }
    }

    #[tokio::test]
    async fn relays_a_dm_from_the_trusted_serbero() {
        let fx = Fixture::new().await;
        let mut inbox = fx.inbox(Some(fx.serbero.public_key()));

        let outcome = inbox
            .receive(&fx.dm("handed off: conflicting_claims", 100), &fx.alerts())
            .await;

        assert_eq!(
            outcome,
            Some(Outcome::Relayed {
                redrawn: false,
                alerted: true
            })
        );
        assert_eq!(fx.telegram.sends().len(), 1);
    }

    #[tokio::test]
    async fn ignores_dms_until_serberos_key_is_known() {
        let fx = Fixture::new().await;
        let mut inbox = fx.inbox(None);

        let outcome = inbox
            .receive(&fx.dm("handed off: flood", 100), &fx.alerts())
            .await;

        assert_eq!(outcome, None);
        assert!(fx.telegram.calls().is_empty());
    }

    #[tokio::test]
    async fn ignores_dms_from_a_key_that_is_not_trusted() {
        let fx = Fixture::new().await;
        let mut inbox = fx.inbox(Some(Keys::generate().public_key()));

        let outcome = inbox
            .receive(&fx.dm("handed off: flood", 100), &fx.alerts())
            .await;

        assert_eq!(outcome, None);
        assert!(fx.telegram.calls().is_empty());
    }

    #[tokio::test]
    async fn ignores_subjects_it_does_not_relay() {
        let fx = Fixture::new().await;
        let mut inbox = fx.inbox(Some(fx.serbero.public_key()));

        let outcome = inbox
            .receive(&fx.dm("resolved: settled", 100), &fx.alerts())
            .await;

        assert_eq!(outcome, None);
        assert!(fx.telegram.calls().is_empty());
    }

    #[tokio::test]
    async fn a_failed_relay_asks_for_an_early_catch_up() {
        let fx = Fixture::new().await;
        let mut inbox = fx.inbox(Some(fx.serbero.public_key()));
        fx.telegram.set_down(true);

        let outcome = inbox
            .receive(&fx.dm("handed off: flood", 100), &fx.alerts())
            .await;

        assert_eq!(outcome, None);
        tokio::time::timeout(Duration::from_secs(1), fx.retry.notified())
            .await
            .expect("the sync task is woken");
    }

    #[tokio::test]
    async fn a_caught_up_batch_is_relayed_oldest_first() {
        let fx = Fixture::new().await;
        fx.store
            .insert(DISPUTE, 42, CHAT, "in-progress", "base")
            .await
            .unwrap();
        let mut inbox = fx.inbox(Some(fx.serbero.public_key()));
        let batch = vec![
            fx.dm("handed off: round_limit", 200),
            fx.dm("mediating", 100),
        ];

        inbox.receive_batch(batch, &fx.alerts()).await;

        let edits: Vec<String> = fx
            .telegram
            .edits()
            .into_iter()
            .map(|call| match call {
                Call::Edit { text, .. } => text,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(edits.len(), 2, "{edits:?}");
        assert!(edits[0].contains("*Status:* 🤖 WITH SERBERO · mediating"));
        assert!(!edits[0].contains("handed off"));
        assert!(edits[1].contains("*Status:* 🙋 NEEDS A SOLVER · handed off · round limit"));
        assert!(
            edits[1].contains("Serbero mediating\n🙋 `00:03:20` Serbero handed off · round limit")
        );
        assert_eq!(fx.telegram.sends().len(), 1);
    }
}
