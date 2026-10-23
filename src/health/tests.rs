//! Health monitoring end to end, without relays: events in, Telegram
//! messages out, one evaluation per simulated minute.

use super::*;
use crate::health::content::tests::payload;
use crate::health::event::tests::{health_tags, set_tag, sign, NOW};
use crate::health::store::tests::store;
use crate::serbero::testing::{Call, FakeTelegram};
use mostro_core::prelude::NOSTR_INFO_EVENT_KIND;

const CHAT: i64 = -100_777;

struct Harness {
    _dir: tempfile::TempDir,
    store: HealthStore,
    mostro: Keys,
    watchdog: Keys,
    monitor: Monitor<FakeTelegram>,
}

fn config() -> MonitorConfig {
    MonitorConfig {
        mostro_health_enabled: true,
        ..MonitorConfig::default()
    }
}

impl Harness {
    async fn new(config: MonitorConfig) -> Self {
        let (dir, store) = store().await;
        let mostro = Keys::generate();
        let watchdog = Keys::generate();
        let monitor = Monitor::new(
            config,
            mostro.public_key(),
            WatchdogKeys {
                sign: Some(watchdog.clone()),
                decrypt: Some(watchdog.clone()),
            },
            CHAT,
            NOW - 3600,
            FakeTelegram::default(),
            store.clone(),
        )
        .await
        .unwrap();
        Self {
            _dir: dir,
            store,
            mostro,
            watchdog,
            monitor,
        }
    }

    /// The monitor again on the same store: a restart.
    async fn restart(&mut self, config: MonitorConfig) {
        self.monitor = Monitor::new(
            config,
            self.mostro.public_key(),
            WatchdogKeys {
                sign: Some(self.watchdog.clone()),
                decrypt: Some(self.watchdog.clone()),
            },
            CHAT,
            NOW - 3600,
            FakeTelegram::default(),
            self.store.clone(),
        )
        .await
        .unwrap();
    }

    fn sends(&self) -> Vec<String> {
        self.monitor
            .messenger
            .sends()
            .into_iter()
            .map(|call| match call {
                Call::Send { chat_id, text } => {
                    assert_eq!(chat_id, CHAT);
                    text
                }
                _ => unreachable!(),
            })
            .collect()
    }

    /// One evaluation at `now`, the relays answering with `events`.
    async fn round(&mut self, now: u64, events: &[Event], reachable: bool) {
        self.monitor
            .assess(Round {
                now,
                relays_reachable: Some(reachable),
                events,
                explorer: Some((971_485, "mempool.space".into())),
            })
            .await;
    }

    fn info(&self, created_at: u64) -> Event {
        EventBuilder::new(Kind::Custom(NOSTR_INFO_EVENT_KIND), "")
            .tags([
                Tag::identifier(self.mostro.public_key().to_hex()),
                Tag::parse(["maintenance_mode", "false"]).unwrap(),
            ])
            .custom_created_at(Timestamp::from(created_at))
            .finalize(&self.mostro)
            .unwrap()
    }

    /// A health event published at `created_at`: healthy, or the
    /// 2026-10-14 incident (LND 136 blocks behind, unsynced, down).
    fn health(&self, created_at: u64, seq: u64, stalled: bool) -> Event {
        let mut tags = health_tags(&self.mostro, created_at, seq);
        if stalled {
            set_tag(&mut tags, "status", &["down"]);
            set_tag(&mut tags, "lnd_block_height", &["971349"]);
            set_tag(&mut tags, "lnd_synced_to_chain", &["false"]);
            tags.retain(|t| t[0] != "check" || t[1] != "lnd_sync");
            tags.push(vec![
                "check".into(),
                "lnd_sync".into(),
                "fail".into(),
                (NOW - 6 * 3600).to_string(),
            ]);
        }
        sign(&self.mostro, tags, "", created_at)
    }
}

/// Feeds a fresh event every minute for `minutes` from `start`.
async fn minutes(h: &mut Harness, start: u64, minutes: u64, stalled: bool, seq0: u64) {
    for i in 0..minutes {
        let now = start + 60 * i;
        let events = [h.info(now), h.health(now, seq0 + i, stalled)];
        h.round(now, &events, true).await;
    }
}

#[tokio::test]
async fn the_incident_replayed_alerts_once_and_resolves_once() {
    let mut h = Harness::new(config()).await;

    // Healthy for ten minutes: nothing is sent.
    minutes(&mut h, NOW, 10, false, 1).await;
    assert!(h.sends().is_empty());

    // Stalled: one alert after three samples, nothing more for 50 minutes.
    minutes(&mut h, NOW + 600, 50, true, 100).await;
    let sends = h.sends();
    assert_eq!(sends.len(), 1, "{sends:#?}");
    let alert = &sends[0];
    assert!(alert.starts_with("🚨 *Mostro health alert*"), "{alert}");
    assert!(alert.contains("*LND behind the chain*"), "{alert}");
    assert!(
        alert.contains(r"LND is 136 blocks behind \(971349 vs 971485 from mempool\.space\)"),
        "{alert}"
    );
    assert!(alert.contains("*LND not synced to chain*"), "{alert}");
    assert!(alert.contains("*Mostro reports itself down*"), "{alert}");
    assert!(alert.contains("*Test Mostro*"), "{alert}");

    // Back to normal: one recovery message for everything.
    minutes(&mut h, NOW + 3600, 10, false, 200).await;
    let sends = h.sends();
    assert_eq!(sends.len(), 2, "{sends:#?}");
    assert!(sends[1].starts_with("✅ *Resolved*"), "{}", sends[1]);
    assert!(sends[1].contains("*LND behind the chain*"), "{}", sends[1]);
}

#[tokio::test]
async fn a_long_critical_incident_is_repeated_hourly() {
    let mut h = Harness::new(config()).await;

    minutes(&mut h, NOW, 130, true, 1).await;

    let sends = h.sends();
    assert_eq!(sends.len(), 3, "alert + 2 reminders: {sends:#?}");
    assert!(sends[1].starts_with("🔁 *Still active*"), "{}", sends[1]);
}

#[tokio::test]
async fn unreachable_relays_are_only_logged_by_default_and_hold_silence() {
    let mut h = Harness::new(config()).await;

    for i in 0..10 {
        h.round(NOW + 60 * i, &[], false).await;
    }

    assert!(h.sends().is_empty(), "{:#?}", h.sends());
}

#[tokio::test]
async fn a_silent_mostro_is_reported_when_relays_round_trip() {
    let mut h = Harness::new(config()).await;
    minutes(&mut h, NOW, 3, false, 1).await;

    // mostrod stops: relays still work, its events just stop coming.
    for i in 0..10 {
        h.round(NOW + 180 + 60 * i, &[], true).await;
    }

    let sends = h.sends();
    assert_eq!(sends.len(), 1, "{sends:#?}");
    assert!(sends[0].contains("*Mostro is silent*"), "{}", sends[0]);
}

#[tokio::test]
async fn phase_zero_reports_a_stopped_mostrod_once_and_its_return() {
    let config = MonitorConfig {
        info_silence_threshold: 900,
        ..MonitorConfig::default()
    };
    let mut h = Harness::new(config).await;
    let last_info = h.info(NOW);

    // The info event stops at NOW; the threshold is 900 s.
    for i in 0..30 {
        h.round(NOW + 60 * i, std::slice::from_ref(&last_info), true)
            .await;
    }
    let sends = h.sends();
    assert_eq!(sends.len(), 1, "{sends:#?}");
    assert!(
        sends[0].contains("*Mostro info event silent*"),
        "{}",
        sends[0]
    );

    for i in 30..35 {
        let fresh = h.info(NOW + 60 * i);
        h.round(NOW + 60 * i, &[fresh], true).await;
    }
    let sends = h.sends();
    assert_eq!(sends.len(), 2, "{sends:#?}");
    assert!(
        sends[1].contains("*Mostro info event silent*"),
        "{}",
        sends[1]
    );
    assert!(sends[1].starts_with("✅ *Resolved*"), "{}", sends[1]);
}

#[tokio::test]
async fn nothing_is_sent_while_everything_is_healthy() {
    let mut h = Harness::new(config()).await;

    minutes(&mut h, NOW, 24 * 60, false, 1).await;

    assert!(h.sends().is_empty(), "{:#?}", h.sends());
}

#[tokio::test]
async fn a_restart_neither_repeats_nor_forgets_an_open_incident() {
    let mut h = Harness::new(config()).await;
    minutes(&mut h, NOW, 10, true, 1).await;
    assert_eq!(h.sends().len(), 1);

    h.restart(config()).await;
    minutes(&mut h, NOW + 600, 10, true, 100).await;
    assert!(h.sends().is_empty(), "no new alert: {:#?}", h.sends());

    minutes(&mut h, NOW + 1200, 5, false, 200).await;
    let sends = h.sends();
    assert_eq!(sends.len(), 1, "{sends:#?}");
    assert!(sends[0].starts_with("✅ *Resolved*"), "{}", sends[0]);
}

#[tokio::test]
async fn an_older_revision_served_after_a_restart_is_not_accepted() {
    let config = MonitorConfig {
        info_silence_threshold: 0,
        ..config()
    };
    let mut h = Harness::new(config.clone()).await;
    // The instance published seq 50, stalled, then recovered with seq 51.
    let stalled = h.health(NOW, 50, true);
    let recovered = h.health(NOW + 60, 51, false);
    h.round(NOW, std::slice::from_ref(&stalled), true).await;
    h.round(NOW + 60, std::slice::from_ref(&recovered), true)
        .await;

    h.restart(config).await;
    // A relay still serves the unexpired, older revision.
    for i in 0..5 {
        h.round(NOW + 90 + i, std::slice::from_ref(&stalled), true)
            .await;
    }

    assert!(
        h.monitor.health.is_none(),
        "the replay must not become current"
    );
    assert_eq!(
        h.store
            .accepted(&h.mostro.public_key())
            .await
            .unwrap()
            .map(|a| a.seq),
        Some(51)
    );
    // Without the current revision the instance is unknown; it is never
    // reported down on the strength of the replay.
    for text in h.sends() {
        assert!(!text.contains("not synced"), "{text}");
        assert!(!text.contains("reports itself down"), "{text}");
    }

    // The current revision, served again, is still accepted.
    h.round(NOW + 100, std::slice::from_ref(&recovered), true)
        .await;
    assert_eq!(h.monitor.health.as_ref().map(|r| r.envelope.seq), Some(51));
}

#[tokio::test]
async fn forged_or_future_events_are_ignored() {
    let mut h = Harness::new(config()).await;
    let impostor = Keys::generate();
    // Claims to be Mostro's address but is signed by someone else.
    let mut tags = health_tags(&h.mostro, NOW, 9);
    set_tag(&mut tags, "status", &["down"]);
    let forged = sign(&impostor, tags, "", NOW);
    let future = h.health(NOW + 400, 10, true);

    h.round(NOW, &[forged, future], true).await;

    assert!(h.monitor.health.is_none());
}

#[tokio::test]
async fn the_encrypted_details_feed_the_payout_rules() {
    let mut h = Harness::new(config()).await;
    let mut body = payload();
    body["payouts"]["payouts_succeeded_1h"] = 0.into();
    body["payouts"]["payouts_failed_1h"] = 9.into();
    body["payouts"]["failure_reasons_1h"] = serde_json::json!({"no_route": 9});

    for i in 0..3 {
        let now = NOW + 60 * i;
        body["generated_at"] = now.into();
        let mut tags = health_tags(&h.mostro, now, 1 + i);
        set_tag(&mut tags, "p", &[&h.watchdog.public_key().to_hex()]);
        let content = nip44::encrypt(
            h.mostro.secret_key(),
            &h.watchdog.public_key(),
            body.to_string(),
            nip44::Version::V2,
        )
        .unwrap();
        let event = sign(&h.mostro, tags, &content, now);
        let info = h.info(now);
        h.round(now, &[info, event], true).await;
    }

    let sends = h.sends();
    assert_eq!(sends.len(), 1, "{sends:#?}");
    assert!(sends[0].contains("*Buyer payouts failing*"), "{}", sends[0]);
    assert!(sends[0].contains("Reasons: no\\_route 9"), "{}", sends[0]);
}

#[tokio::test]
async fn the_heartbeat_keeps_rising_across_restarts() {
    let mut h = Harness::new(config()).await;
    let first = h.monitor.next_heartbeat(NOW * 1000).await.unwrap();

    h.restart(config()).await;
    // The clock stepped back a minute meanwhile.
    let second = h.monitor.next_heartbeat((NOW - 60) * 1000).await.unwrap();

    let seq = |e: &Event| {
        e.tags
            .iter()
            .find(|t| t.as_slice()[0] == "seq")
            .unwrap()
            .as_slice()[1]
            .parse::<u64>()
            .unwrap()
    };
    assert!(seq(&second) > seq(&first));
    assert!(second.created_at > first.created_at);
}

#[tokio::test]
async fn a_silent_peer_watchdog_is_reported() {
    let peer = Keys::generate();
    let config = MonitorConfig {
        peer_watchdogs: vec![peer.public_key().to_hex()],
        ..MonitorConfig::default()
    };
    let mut h = Harness::new(config).await;
    let beat = heartbeat::build(&peer, heartbeat::next(None, NOW * 1000), 60, None).unwrap();

    for i in 0..10 {
        let info = h.info(NOW + 60 * i);
        h.round(NOW + 60 * i, &[info, beat.clone()], true).await;
    }

    let sends = h.sends();
    assert_eq!(sends.len(), 1, "{sends:#?}");
    assert!(sends[0].contains("*Peer watchdog silent*"), "{}", sends[0]);
}

#[tokio::test]
async fn after_a_restart_the_current_event_restores_the_state_at_once() {
    let mut h = Harness::new(config()).await;
    let current = h.health(NOW, 42, false);
    h.round(NOW, std::slice::from_ref(&current), true).await;

    h.restart(config()).await;
    // Same seq, another event: not the one accepted, so rejected.
    let impostor_revision = h.health(NOW + 1, 42, true);
    h.round(NOW + 30, std::slice::from_ref(&impostor_revision), true)
        .await;
    assert!(h.monitor.health.is_none());

    // The relay returns the very event accepted before the restart.
    h.round(NOW + 60, std::slice::from_ref(&current), true)
        .await;

    let restored = h.monitor.health.as_ref().expect("state restored");
    assert_eq!(restored.envelope.id, current.id);
    assert!(h.sends().is_empty(), "{:#?}", h.sends());
}

#[tokio::test]
async fn a_shared_decrypt_key_reads_the_details_while_the_heartbeat_keeps_its_own_key() {
    let mut h = Harness::new(config()).await;
    let shared = Keys::generate();
    h.monitor.keys = WatchdogKeys {
        sign: Some(h.watchdog.clone()),
        decrypt: Some(shared.clone()),
    };
    let mut body = payload();
    body["payouts"]["payouts_succeeded_1h"] = 0.into();
    body["payouts"]["payouts_failed_1h"] = 9.into();

    for i in 0..3 {
        let now = NOW + 60 * i;
        body["generated_at"] = now.into();
        let mut tags = health_tags(&h.mostro, now, 1 + i);
        set_tag(&mut tags, "p", &[&shared.public_key().to_hex()]);
        let content = nip44::encrypt(
            h.mostro.secret_key(),
            &shared.public_key(),
            body.to_string(),
            nip44::Version::V2,
        )
        .unwrap();
        let event = sign(&h.mostro, tags, &content, now);
        let info = h.info(now);
        h.round(now, &[info, event], true).await;
    }
    let heartbeat = h.monitor.next_heartbeat((NOW + 200) * 1000).await.unwrap();

    assert!(
        h.monitor.details.is_some(),
        "details read with the shared key"
    );
    assert!(h.sends()[0].contains("*Buyer payouts failing*"));
    assert_eq!(heartbeat.pubkey, h.watchdog.public_key());
}

#[tokio::test]
async fn a_recovery_telegram_refused_is_sent_after_a_restart() {
    let mut h = Harness::new(config()).await;
    minutes(&mut h, NOW, 5, true, 1).await;
    assert_eq!(h.sends().len(), 1);

    // It clears while Telegram is down, then the watchdog restarts.
    h.monitor.messenger.set_down(true);
    minutes(&mut h, NOW + 300, 5, false, 100).await;
    h.restart(config()).await;
    minutes(&mut h, NOW + 600, 1, false, 200).await;

    let sends = h.sends();
    assert_eq!(sends.len(), 1, "{sends:#?}");
    assert!(sends[0].starts_with("✅ *Resolved*"), "{}", sends[0]);
    assert!(h.store.pending_recoveries().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_peer_watchdog_listed_twice_counts_once() {
    let peer = Keys::generate();
    let npub = nostr_sdk::prelude::ToBech32::to_bech32(&peer.public_key()).unwrap();
    let mut h = Harness::new(MonitorConfig {
        peer_watchdogs: vec![npub, peer.public_key().to_hex()],
        ..MonitorConfig::default()
    })
    .await;

    // Two rounds: with the key counted twice, the streak would reach three.
    for i in 0..2 {
        let info = h.info(NOW + 60 * i);
        h.round(NOW + 60 * i, &[info], true).await;
    }
    assert!(h.sends().is_empty(), "{:#?}", h.sends());

    let info = h.info(NOW + 120);
    h.round(NOW + 120, &[info], true).await;
    let sends = h.sends();
    assert_eq!(sends.len(), 1);
    assert_eq!(
        sends[0].matches("*Peer watchdog silent*").count(),
        1,
        "{}",
        sends[0]
    );
}

#[tokio::test]
async fn an_upgraded_watchdog_with_a_key_publishes_no_heartbeat() {
    let mut h = Harness::new(MonitorConfig::default()).await;

    assert!(h.monitor.next_heartbeat(NOW * 1000).await.is_none());
}

#[tokio::test]
async fn one_evaluation_sends_one_message_across_subjects_and_kinds() {
    let peer = Keys::generate();
    let config = MonitorConfig {
        peer_watchdogs: vec![peer.public_key().to_hex()],
        ..config()
    };
    let mut h = Harness::new(config).await;
    // Mostro stalls and the peer watchdog is silent from the start: both
    // open in the same round.
    minutes(&mut h, NOW, 3, true, 1).await;

    let sends = h.sends();
    assert_eq!(sends.len(), 1, "{sends:#?}");
    assert!(
        sends[0].contains("*LND not synced to chain*"),
        "{}",
        sends[0]
    );
    assert!(sends[0].contains("*Peer watchdog silent*"), "{}", sends[0]);

    // Mostro recovers while the peer appears: one round resolves both.
    let beat =
        heartbeat::build(&peer, heartbeat::next(None, (NOW + 180) * 1000), 60, None).unwrap();
    for i in 0..3 {
        let now = NOW + 180 + 60 * i;
        let events = [h.info(now), h.health(now, 10 + i, false), beat.clone()];
        h.round(now, &events, true).await;
    }
    let sends = h.sends();
    assert_eq!(sends.len(), 2, "{sends:#?}");
    assert!(
        sends[1].contains("*LND not synced to chain*"),
        "{}",
        sends[1]
    );
    assert!(sends[1].contains("*Peer watchdog silent*"), "{}", sends[1]);
}

#[tokio::test]
async fn an_older_revision_of_a_trusted_peer_cannot_lower_the_median() {
    let peers: Vec<Keys> = (0..3).map(|_| Keys::generate()).collect();
    let config = MonitorConfig {
        trusted_peers: peers.iter().map(|k| k.public_key().to_hex()).collect(),
        ..config()
    };
    let mut h = Harness::new(config.clone()).await;
    let event = |keys: &Keys, seq: u64, height: &str, at: u64| {
        let mut tags = health_tags(keys, at, seq);
        set_tag(&mut tags, "lnd_block_height", &[height]);
        sign(keys, tags, "", at)
    };
    let mostro = h.health(NOW, 1, false);
    let current: Vec<Event> = peers.iter().map(|k| event(k, 10, "971485", NOW)).collect();
    // Older revisions, still fresh, with a lower height.
    let older: Vec<Event> = peers
        .iter()
        .map(|k| event(k, 9, "971300", NOW - 30))
        .collect();

    h.monitor
        .ingest(&[current, vec![mostro.clone()]].concat(), NOW)
        .await;
    assert_eq!(h.monitor.peer_heights, vec![971_485; 3]);

    h.monitor
        .ingest(&[older.clone(), vec![mostro.clone()]].concat(), NOW + 10)
        .await;
    assert_eq!(h.monitor.peer_heights, vec![971_485; 3]);

    // After a restart the persisted seq still rejects them.
    h.restart(config).await;
    h.monitor
        .ingest(&[older, vec![mostro]].concat(), NOW + 20)
        .await;
    assert!(
        h.monitor.peer_heights.is_empty(),
        "{:?}",
        h.monitor.peer_heights
    );
}

#[tokio::test]
async fn an_alert_whose_incident_cleared_while_its_write_waited_is_never_sent() {
    let mut h = Harness::new(config()).await;
    // The incidents table is gone: the opening is held behind its write.
    sqlx::query("DROP TABLE health_incidents")
        .execute(h.store.pool())
        .await
        .unwrap();
    minutes(&mut h, NOW, 3, true, 1).await;
    // Healthy again, the write still failing: nothing is sent.
    minutes(&mut h, NOW + 180, 2, false, 100).await;
    assert!(h.sends().is_empty(), "{:#?}", h.sends());

    // The store recovers on the round whose good reading clears the
    // incident: the held opening is released and must not be sent.
    HealthStore::new(h.store.pool().clone()).await.unwrap();
    minutes(&mut h, NOW + 300, 1, false, 102).await;

    assert!(h.sends().is_empty(), "{:#?}", h.sends());
    assert_eq!(h.monitor.tracker.open_incidents().count(), 0);
}

#[tokio::test]
async fn a_failed_seq_write_is_retried_on_the_next_round() {
    let mut h = Harness::new(config()).await;
    let event = h.health(NOW, 5, false);
    // The table is gone: the write fails.
    sqlx::query("DROP TABLE health_accepted_seq")
        .execute(h.store.pool())
        .await
        .unwrap();

    h.monitor.ingest(std::slice::from_ref(&event), NOW).await;
    assert!(h.monitor.health.is_some(), "the valid event is still used");
    assert!(!h.monitor.accepted.contains_key(&h.mostro.public_key()));

    // The store works again: the same revision is written this time.
    HealthStore::new(h.store.pool().clone()).await.unwrap();
    h.monitor
        .ingest(std::slice::from_ref(&event), NOW + 60)
        .await;
    assert_eq!(
        h.store
            .accepted(&h.mostro.public_key())
            .await
            .unwrap()
            .map(|a| a.seq),
        Some(5)
    );
}

#[tokio::test]
async fn a_split_round_counts_the_messages_that_went_out_as_delivered() {
    let peers: Vec<Keys> = (0..40).map(|_| Keys::generate()).collect();
    let config = MonitorConfig {
        peer_watchdogs: peers.iter().map(|k| k.public_key().to_hex()).collect(),
        ..MonitorConfig::default()
    };
    let mut h = Harness::new(config).await;
    for i in 0..3 {
        let info = h.info(NOW + 60 * i);
        h.round(NOW + 60 * i, &[info], true).await;
    }
    let alerts = h.sends();
    assert!(alerts.len() > 1, "40 alerts don't fit one message");
    let alerted: usize = alerts
        .iter()
        .map(|t| t.matches("*Peer watchdog silent*").count())
        .sum();
    assert_eq!(alerted, 40);
    h.monitor.messenger.clear();

    // Every peer comes back; Telegram takes only the first message.
    *h.monitor.messenger.sends_left.lock().unwrap() = Some(1);
    for i in 3..6 {
        let now = NOW + 60 * i;
        let mut events = vec![h.info(now)];
        events.extend(
            peers
                .iter()
                .map(|k| heartbeat::build(k, heartbeat::next(None, now * 1000), 60, None).unwrap()),
        );
        h.round(now, &events, true).await;
    }
    let first = h.sends();
    assert_eq!(first.len(), 1);
    let sent_first = first[0].matches("*Peer watchdog silent*").count();
    assert!(sent_first > 0 && sent_first < 40);
    assert_eq!(
        h.store.pending_recoveries().await.unwrap().len(),
        40 - sent_first
    );

    // Telegram is back: only what didn't go out is sent.
    *h.monitor.messenger.sends_left.lock().unwrap() = None;
    h.monitor.messenger.clear();
    let info = h.info(NOW + 360);
    h.round(NOW + 360, &[info], true).await;
    let rest: usize = h
        .sends()
        .iter()
        .map(|t| t.matches("*Peer watchdog silent*").count())
        .sum();
    assert_eq!(rest, 40 - sent_first);
    assert!(h.store.pending_recoveries().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_recovery_is_told_only_once_it_is_stored() {
    let mut h = Harness::new(config()).await;
    minutes(&mut h, NOW, 5, true, 1).await;
    assert_eq!(h.sends().len(), 1);
    sqlx::query(
        "CREATE TRIGGER no_recovery BEFORE INSERT ON health_pending_recoveries \
         BEGIN SELECT RAISE(ABORT, 'disk full'); END",
    )
    .execute(h.store.pool())
    .await
    .unwrap();

    minutes(&mut h, NOW + 300, 5, false, 100).await;
    assert_eq!(h.sends().len(), 1, "not told while it can't be stored");
    assert_eq!(
        h.store.open_incidents().await.unwrap().len(),
        3,
        "still open on disk"
    );

    sqlx::query("DROP TRIGGER no_recovery")
        .execute(h.store.pool())
        .await
        .unwrap();
    minutes(&mut h, NOW + 600, 1, false, 200).await;
    let sends = h.sends();
    assert_eq!(sends.len(), 2, "{sends:#?}");
    assert!(sends[1].starts_with("✅ *Resolved*"), "{}", sends[1]);
    assert!(h.store.open_incidents().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_acknowledgement_is_retried_and_never_resends() {
    let mut h = Harness::new(config()).await;
    sqlx::query(
        "CREATE TRIGGER no_ack BEFORE UPDATE ON health_incidents \
         WHEN NEW.notified_at IS NOT NULL BEGIN SELECT RAISE(ABORT, 'disk full'); END",
    )
    .execute(h.store.pool())
    .await
    .unwrap();

    minutes(&mut h, NOW, 6, true, 1).await;
    assert_eq!(h.sends().len(), 1, "sent once, even though the ack failed");
    assert!(h
        .store
        .open_incidents()
        .await
        .unwrap()
        .iter()
        .all(|i| i.notified_at.is_none()));

    sqlx::query("DROP TRIGGER no_ack")
        .execute(h.store.pool())
        .await
        .unwrap();
    minutes(&mut h, NOW + 360, 1, true, 100).await;
    assert_eq!(h.sends().len(), 1);
    assert!(h
        .store
        .open_incidents()
        .await
        .unwrap()
        .iter()
        .all(|i| i.notified_at.is_some()));
}

#[tokio::test]
async fn an_older_recovery_is_told_before_the_same_rule_opening_again() {
    let mut h = Harness::new(config()).await;
    minutes(&mut h, NOW, 5, true, 1).await;
    h.monitor.messenger.clear();

    // Telegram is down while it clears and then breaks again.
    h.monitor.messenger.set_down(true);
    minutes(&mut h, NOW + 300, 4, false, 100).await;
    minutes(&mut h, NOW + 540, 4, true, 200).await;
    h.monitor.messenger.set_down(false);
    minutes(&mut h, NOW + 780, 1, true, 300).await;

    let sends = h.sends();
    assert_eq!(sends.len(), 1, "{sends:#?}");
    let resolved = sends[0].find("✅ *Resolved*").expect("the recovery");
    let opened = sends[0]
        .find("🚨 *Mostro health alert*")
        .expect("the new alert");
    assert!(resolved < opened, "{}", sends[0]);
}

#[tokio::test]
async fn an_alert_is_told_only_once_its_incident_is_stored() {
    let mut h = Harness::new(config()).await;
    sqlx::query(
        "CREATE TRIGGER no_open BEFORE INSERT ON health_incidents \
         BEGIN SELECT RAISE(ABORT, 'disk full'); END",
    )
    .execute(h.store.pool())
    .await
    .unwrap();

    minutes(&mut h, NOW, 5, true, 1).await;
    assert!(h.sends().is_empty(), "not told while it can't be stored");

    sqlx::query("DROP TRIGGER no_open")
        .execute(h.store.pool())
        .await
        .unwrap();
    minutes(&mut h, NOW + 300, 3, true, 100).await;
    let sends = h.sends();
    assert_eq!(sends.len(), 1, "{sends:#?}");
    // Each rule once, though the tracker repeated the undelivered alert.
    assert_eq!(
        sends[0].matches("*LND not synced to chain*").count(),
        1,
        "{}",
        sends[0]
    );
    assert_eq!(h.store.open_incidents().await.unwrap().len(), 3);
}

#[test]
fn startup_notes_only_speak_of_what_is_on() {
    let keys = WatchdogKeys {
        sign: Some(Keys::generate()),
        decrypt: None,
    };

    // Phase 0 alone: no heartbeat, no decryption, nothing to say.
    assert!(key_notes(&MonitorConfig::default(), &keys).is_empty());

    let notes = key_notes(&config(), &keys);
    assert_eq!(notes.len(), 2);
    assert!(notes[0].1.starts_with("Heartbeat signed by"));
    assert!(notes[1].0, "no decrypt key is a warning");

    let no_heartbeat = MonitorConfig {
        publish_heartbeat: false,
        ..config()
    };
    let notes = key_notes(&no_heartbeat, &keys);
    assert_eq!(notes.len(), 1);
    assert!(!notes[0].1.contains("Heartbeat"));
}

#[tokio::test]
async fn a_revision_in_use_but_not_stored_is_never_rolled_back() {
    let mut h = Harness::new(config()).await;
    let five = h.health(NOW, 5, false);
    let six = h.health(NOW + 60, 6, true);
    h.monitor.ingest(std::slice::from_ref(&five), NOW).await;
    // The store fails while seq 6 comes in: it is used, not stored.
    sqlx::query("DROP TABLE health_accepted_seq")
        .execute(h.store.pool())
        .await
        .unwrap();
    h.monitor.ingest(std::slice::from_ref(&six), NOW + 60).await;
    assert_eq!(h.monitor.health.as_ref().map(|r| r.envelope.seq), Some(6));

    // A relay serves only the older revision now.
    h.monitor
        .ingest(std::slice::from_ref(&five), NOW + 120)
        .await;
    assert_eq!(h.monitor.health.as_ref().map(|r| r.envelope.seq), Some(6));
}

#[tokio::test]
async fn an_unstored_alert_whose_incident_cleared_is_never_sent() {
    let mut h = Harness::new(config()).await;
    sqlx::query(
        "CREATE TRIGGER no_open BEFORE INSERT ON health_incidents \
         BEGIN SELECT RAISE(ABORT, 'disk full'); END",
    )
    .execute(h.store.pool())
    .await
    .unwrap();
    // Opens (unstored, held), then clears while the store is still down.
    minutes(&mut h, NOW, 4, true, 1).await;
    minutes(&mut h, NOW + 240, 4, false, 100).await;
    assert!(h.sends().is_empty());

    sqlx::query("DROP TRIGGER no_open")
        .execute(h.store.pool())
        .await
        .unwrap();
    minutes(&mut h, NOW + 480, 2, false, 200).await;

    assert!(h.sends().is_empty(), "{:#?}", h.sends());
    assert!(h.store.open_incidents().await.unwrap().is_empty());
}
