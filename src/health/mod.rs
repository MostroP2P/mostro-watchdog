//! Mostro health monitoring (mostro-watchdog#46): alert when the Mostro
//! instance stops *working*, not only when it stops running.
//!
//! - Phase 0: Mostro's info event (kind 38385) must keep arriving, and its
//!   `maintenance_mode` flips are reported.
//! - Phase 1: Mostro's health event (kind 38389, protocol `health_event.md`)
//!   is verified, checked for freshness (a dead man's switch) and for what
//!   it says, and its block height is compared with an independent
//!   reference. The watchdog publishes its own heartbeat, which doubles as
//!   a relay round-trip probe so that "relays down" is not mistaken for
//!   "Mostro silent", and watches the heartbeats of peer watchdogs.
//!
//! Telegram only gets state changes: an alert when a condition starts, a
//! reminder while a critical one lasts, and a "resolved" message. Never a
//! periodic "all good" (#16).

pub mod content;
pub mod event;
pub mod heartbeat;
pub mod height;
pub mod info;
pub mod render;
pub mod rules;
pub mod settings;
pub mod store;
pub mod tracker;

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nostr_sdk::prelude::*;
use teloxide::Bot;
use tracing::{debug, error, info, warn};

use crate::serbero::discovery::info_filter;
use crate::serbero::telegram::Messenger;
use content::{Content, HealthDetails};
use event::{newest, parse_health, parse_watcher, Accepted, Envelope, HealthReport, HEALTH_KIND};
use info::{newest_info, InfoSnapshot};
use rules::{evaluate, Snapshot, Subject};
use settings::MonitorConfig;
use store::{HealthStore, LastHeartbeat};
use tracker::{Change, Notice, Policy, Tracker};

/// How long a round of relay queries may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(15);

/// How long the watchdog waits to read its own heartbeat back.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Trusted instances' health events older than this are not fetched: the
/// longest interval allows three hours of freshness.
const PEER_LOOKBACK_SECS: u64 = 3 * event::MAX_INTERVAL;

/// Most trusted-instance events fetched per round for the peer median.
const PEER_FETCH_LIMIT: usize = 500;

/// The `z` tag, filterable as `#z`.
fn z_tag() -> SingleLetterTag {
    SingleLetterTag::from_char('z').expect("z is a single lowercase letter")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// The watchdog's keys for health monitoring.
#[derive(Clone, Default)]
pub struct WatchdogKeys {
    /// Signs the heartbeat. Each watchdog must have its own: heartbeats
    /// from one key share one addressable coordinate, so a live watchdog
    /// would mask a dead one.
    pub sign: Option<Keys>,
    /// Decrypts the health event's details. Watchdogs may share it, since
    /// the details are encrypted to one recipient. Defaults to `sign`.
    pub decrypt: Option<Keys>,
}

/// The state of health monitoring between two rounds.
pub struct Monitor<M> {
    config: MonitorConfig,
    mostro: PublicKey,
    keys: WatchdogKeys,
    peers: Vec<PublicKey>,
    /// Instances trusted for the block-height median, never Mostro itself.
    trusted_peers: Vec<PublicKey>,
    chat_id: i64,
    started_at: u64,
    messenger: M,
    store: HealthStore,
    tracker: Tracker,
    accepted: HashMap<PublicKey, Accepted>,
    info: Option<InfoSnapshot>,
    health: Option<HealthReport>,
    details: Option<HealthDetails>,
    peer_beats: HashMap<PublicKey, Envelope>,
    /// The current revision of each trusted peer, by persisted `seq`.
    trusted_reports: HashMap<PublicKey, HealthReport>,
    peer_heights: Vec<u64>,
    last_heartbeat: Option<LastHeartbeat>,
    /// The last content outcome logged, so a problem is logged once.
    content_logged: Option<String>,
    /// Recoveries Telegram didn't take, retried next round.
    undelivered: Vec<Notice>,
    /// The last relay state logged, so an outage is logged once.
    relays_logged: Option<bool>,
    /// Store writes that failed, retried in order every round, with the
    /// recovery each one holds back until it is stored.
    pending_writes: Vec<(Change, Option<Notice>)>,
}

/// What a round brings in from relays and explorers.
pub struct Round<'a> {
    pub now: u64,
    pub relays_reachable: Option<bool>,
    pub events: &'a [Event],
    pub explorer: Option<(u64, String)>,
}

impl<M: Messenger> Monitor<M> {
    /// A monitor that resumes what `store` kept before a restart.
    pub async fn new(
        config: MonitorConfig,
        mostro: PublicKey,
        keys: WatchdogKeys,
        chat_id: i64,
        started_at: u64,
        messenger: M,
        store: HealthStore,
    ) -> Result<Self, sqlx::Error> {
        let peers = config.peer_watchdogs().unwrap_or_default();
        let trusted_peers: Vec<PublicKey> = config
            .trusted_peers()
            .unwrap_or_default()
            .into_iter()
            .filter(|pk| *pk != mostro)
            .collect();
        let mut accepted = HashMap::new();
        for pubkey in std::iter::once(&mostro).chain(&peers).chain(&trusted_peers) {
            if let Some(last) = store.accepted(pubkey).await? {
                accepted.insert(*pubkey, last);
            }
        }
        let tracker = Tracker::resume(store.open_incidents().await?);
        let undelivered = store.pending_recoveries().await?;
        let last_heartbeat = store.last_heartbeat().await?;
        Ok(Self {
            config,
            mostro,
            keys,
            peers,
            trusted_peers,
            chat_id,
            started_at,
            messenger,
            store,
            tracker,
            accepted,
            info: None,
            health: None,
            details: None,
            peer_beats: HashMap::new(),
            trusted_reports: HashMap::new(),
            peer_heights: Vec::new(),
            last_heartbeat,
            content_logged: None,
            undelivered,
            relays_logged: None,
            pending_writes: Vec::new(),
        })
    }

    /// The filters of one round.
    pub fn filters(&self, now: u64) -> Vec<Filter> {
        let mut filters = Vec::new();
        if self.config.info_silence_threshold > 0 || self.config.alert_on_maintenance {
            filters.push(info_filter(self.mostro));
        }
        if self.config.mostro_health_enabled {
            filters.push(
                Filter::new()
                    .kind(HEALTH_KIND)
                    .author(self.mostro)
                    .identifier(self.mostro.to_hex()),
            );
            if !self.trusted_peers.is_empty() {
                filters.push(
                    Filter::new()
                        .kind(HEALTH_KIND)
                        .authors(self.trusted_peers.clone())
                        .custom_tag(z_tag(), event::Z_HEALTH)
                        .since(Timestamp::from(now.saturating_sub(PEER_LOOKBACK_SECS)))
                        .limit(PEER_FETCH_LIMIT),
                );
            }
        }
        if !self.peers.is_empty() {
            filters.push(
                Filter::new()
                    .kind(HEALTH_KIND)
                    .authors(self.peers.clone())
                    .custom_tag(z_tag(), event::Z_WATCHER),
            );
        }
        filters
    }

    /// The network whose explorers to ask this round: the instance's,
    /// from its last accepted health event.
    pub fn reference_network(&self) -> Option<&str> {
        self.health.as_ref()?.lnd_network.as_deref()
    }

    /// The next heartbeat to publish, when the watchdog publishes one.
    pub async fn next_heartbeat(&mut self, now_ms: u64) -> Option<Event> {
        if !self.config.heartbeat_enabled() {
            return None;
        }
        let keys = self.keys.sign.as_ref()?;
        let next = heartbeat::next(self.last_heartbeat, now_ms);
        if heartbeat::too_far_ahead(next, now_ms / 1000) {
            error!(
                created_at = next.created_at,
                "The clock went back by more than 300 s since the last heartbeat; readers \
                 will drop it as coming from the future until the clock catches up. \
                 Check this host's clock"
            );
        }
        let watching = self.config.heartbeat_watching_tag.then_some(self.mostro);
        match heartbeat::build(keys, next, self.config.check_interval, watching) {
            Ok(event) => {
                // Saved before it is sent: a seq or created_at that reached
                // a relay must never be reused.
                if let Err(e) = self.store.save_heartbeat(next).await {
                    error!(error = %e, "Failed to save the heartbeat state; not publishing it");
                    return None;
                }
                self.last_heartbeat = Some(next);
                Some(event)
            }
            Err(e) => {
                error!(error = %e, "Failed to sign the heartbeat");
                None
            }
        }
    }

    /// Takes in what the relays returned.
    pub async fn ingest(&mut self, events: &[Event], now: u64) {
        if let Some(info) = newest_info(events, self.mostro, now) {
            if self
                .info
                .is_none_or(|known| info.created_at > known.created_at)
            {
                self.info = Some(info);
            }
        }
        if self.config.mostro_health_enabled {
            self.ingest_health(events, now).await;
        }
        for peer in self.peers.clone() {
            let beats: Vec<_> = events
                .iter()
                .filter_map(|e| parse_watcher(e, Some(peer), now).ok())
                .collect();
            let last = self.accepted.get(&peer).copied();
            if let Some(beat) = newest(&beats, |b| &b.envelope, last).cloned() {
                self.accept(&beat.envelope).await;
                self.peer_beats.insert(peer, beat.envelope);
            }
        }
        // Trusted peers follow the same seq rules as Mostro: an older
        // revision a relay still serves must not lower the median.
        for peer in self.trusted_peers.clone() {
            let reports = height::peer_reports(events, peer, now);
            let last = self.accepted.get(&peer).copied();
            if let Some(report) = newest(&reports, |r| &r.envelope, last).cloned() {
                self.accept(&report.envelope).await;
                self.trusted_reports.insert(peer, report);
            }
        }
        self.peer_heights = match self.reference_network() {
            Some(network) => height::peer_heights(self.trusted_reports.values(), network, now),
            None => Vec::new(),
        };
    }

    async fn ingest_health(&mut self, events: &[Event], now: u64) {
        let mut reports = Vec::new();
        for event in events
            .iter()
            .filter(|e| e.pubkey == self.mostro && e.kind == HEALTH_KIND)
        {
            match parse_health(event, self.mostro, now) {
                Ok(report) => reports.push(report),
                Err(rejection) => {
                    warn!(event_id = %event.id, %rejection, "Ignoring Mostro health event")
                }
            }
        }
        let last = self.accepted.get(&self.mostro).copied();
        let Some(report) = newest(&reports, |r| &r.envelope, last).cloned() else {
            if !reports.is_empty() {
                debug!("Only older revisions of the health event were served");
            }
            return;
        };
        // Before the early return: a write that failed is retried here.
        self.accept(&report.envelope).await;
        if self.health.as_ref().map(|h| h.envelope.id) == Some(report.envelope.id) {
            return;
        }
        let content = content::read(&report, self.keys.decrypt.as_ref());
        self.log_content(&content);
        self.details = match content {
            Content::Details(details) => Some(*details),
            _ => None,
        };
        self.health = Some(report);
    }

    /// Records `envelope` as the newest revision from its publisher. Only
    /// once it is stored does the monitor remember it: a failed write is
    /// retried the next time the revision is seen.
    async fn accept(&mut self, envelope: &Envelope) {
        let accepted = Accepted {
            seq: envelope.seq,
            id: envelope.id,
        };
        if self.accepted.get(&envelope.pubkey) == Some(&accepted) {
            return;
        }
        match self.store.accept(&envelope.pubkey, accepted).await {
            Ok(()) => {
                self.accepted.insert(envelope.pubkey, accepted);
            }
            Err(e) => error!(error = %e, "Failed to persist the accepted seq; retried next round"),
        }
    }

    fn log_content(&mut self, content: &Content) {
        let state = match content {
            Content::Details(_) => "details",
            Content::Empty => "empty",
            Content::NotForUs => "not_for_us",
            Content::Unreadable => "unreadable",
            Content::UnknownVersion(_) => "unknown_version",
            Content::Inconsistent => "inconsistent",
        };
        if self.content_logged.as_deref() == Some(state) {
            return;
        }
        self.content_logged = Some(state.to_string());
        match content {
            Content::Details(_) => info!("Reading the health event's encrypted details"),
            Content::Empty => warn!(
                "The health event has no encrypted details; only its public tags are \
                 checked. Set the watchdog's pubkey in mostrod to get them"
            ),
            Content::NotForUs => warn!(
                "The health event's details are encrypted to another key (or this \
                 watchdog has no key); only its public tags are checked"
            ),
            Content::Unreadable => warn!(
                "The health event's details could not be decrypted or parsed; only its \
                 public tags are checked"
            ),
            Content::UnknownVersion(v) => warn!(
                version = ?v,
                "The health event's details use a version this watchdog doesn't know; \
                 only its public tags are checked"
            ),
            Content::Inconsistent => warn!(
                "The health event's details contradict its public tags (a mostrod bug); \
                 they are ignored"
            ),
        }
    }

    /// Logs when the watchdog loses or regains its relays. The logs are
    /// the default channel for the watchdog's own infrastructure.
    fn log_relays(&mut self, reachable: Option<bool>) {
        let Some(reachable) = reachable else {
            return;
        };
        if self.relays_logged == Some(reachable) {
            return;
        }
        let was_known = self.relays_logged.is_some();
        self.relays_logged = Some(reachable);
        if !reachable {
            warn!(
                "🔌 The watchdog can't round-trip an event through any relay; Mostro's \
                 state is unknown until a relay answers"
            );
        } else if was_known {
            info!("🔌 The watchdog reaches its relays again");
        }
    }

    /// Evaluates the round and tells Telegram what changed.
    pub async fn assess(&mut self, round: Round<'_>) {
        let now = round.now;
        self.log_relays(round.relays_reachable);
        if !round.events.is_empty() {
            self.ingest(round.events, now).await;
        }
        let reference = height::reference(round.explorer, &self.peer_heights);
        let snapshot = Snapshot {
            now,
            started_at: self.started_at,
            mostro: self.mostro,
            relays_reachable: round.relays_reachable,
            info: self.info,
            health: self.health.as_ref(),
            details: self.details.as_ref(),
            reference,
            peers: self
                .peers
                .iter()
                .map(|peer| (*peer, self.peer_beats.get(peer).cloned()))
                .collect(),
        };
        let readings = evaluate(&snapshot, &self.config);
        let policy = Policy {
            consecutive_samples: self.config.consecutive_samples,
            renotify_interval: self.config.renotify_interval,
        };
        let mut notices = std::mem::take(&mut self.undelivered);
        // Writes that failed last round go first, in order; a recovery held
        // behind one is released once it is stored.
        notices.extend(self.flush().await);
        for (subject, rule, reading) in readings {
            let (notice, change) = self.tracker.observe(subject, rule, reading, now, policy);
            match (change, notice) {
                // A recovery is told only once it is stored: told and then
                // lost, a restart would bring the incident back.
                (Some(change @ Change::Resolve { .. }), notice) => {
                    notices.extend(self.persist(change, notice).await);
                }
                (Some(change), notice) => {
                    notices.extend(self.persist(change, None).await);
                    notices.extend(notice);
                }
                (None, notice) => notices.extend(notice),
            }
        }
        self.deliver(notices, now).await;
    }

    /// Queues `change` behind any earlier write that failed and writes the
    /// queue in order. Returns the notices whose writes are now stored.
    async fn persist(&mut self, change: Change, hold: Option<Notice>) -> Vec<Notice> {
        self.pending_writes.push((change, hold));
        self.flush().await
    }

    /// Writes the queued changes in order, stopping at the first failure:
    /// the rest wait for the next round, so they never land out of order.
    async fn flush(&mut self) -> Vec<Notice> {
        let mut released = Vec::new();
        while let Some((change, _)) = self.pending_writes.first() {
            if let Err(e) = self.store.apply(change).await {
                error!(
                    error = %e,
                    queued = self.pending_writes.len(),
                    "Failed to persist a health incident; retried next round"
                );
                break;
            }
            let (_, hold) = self.pending_writes.remove(0);
            released.extend(hold);
        }
        released
    }

    async fn deliver(&mut self, notices: Vec<Notice>, now: u64) {
        if notices.is_empty() {
            return;
        }
        // One message for the whole evaluation: a section per subject and
        // kind of notice (new alerts, reminders, recoveries), so a round
        // rings the phones once.
        let mut groups: BTreeMap<(u8, Subject), Vec<usize>> = BTreeMap::new();
        for (index, notice) in notices.iter().enumerate() {
            let kind = match notice {
                Notice::Opened(_) => 0,
                Notice::Reminder(_) => 1,
                Notice::Resolved { .. } => 2,
            };
            groups
                .entry((kind, notice.incident().subject.clone()))
                .or_default()
                .push(index);
        }
        // In the order things happened: a recovery Telegram refused earlier
        // comes before the same rule opening again.
        let when = |index: &usize| transition_time(&notices[*index], now);
        let mut groups: Vec<((u8, Subject), Vec<usize>)> = groups.into_iter().collect();
        for (_, indices) in &mut groups {
            indices.sort_by_key(when);
        }
        groups.sort_by_key(|((kind, _), indices)| (indices.iter().map(when).min(), *kind));
        let mostro_name = self.health.as_ref().and_then(|h| h.name.clone());
        let order: Vec<Vec<usize>> = groups.iter().map(|(_, indices)| indices.clone()).collect();
        let sections: Vec<render::Section> = groups
            .iter()
            .map(|((kind, subject), indices)| {
                let label = render::subject_label(subject, mostro_name.as_deref());
                let group: Vec<&Notice> = indices.iter().map(|&i| &notices[i]).collect();
                match kind {
                    0 => render::opened(&label, &group),
                    1 => render::reminders(&label, &group, now),
                    _ => render::resolved(&label, &group),
                }
            })
            .collect();
        // Almost always one message. If a round doesn't fit and a later
        // message fails, the ones already sent count as delivered.
        let mut delivered: Vec<usize> = Vec::new();
        let mut failed = false;
        for message in render::pack(&sections) {
            let carried: Vec<usize> = message
                .items
                .iter()
                .map(|&(section, item)| order[section][item])
                .collect();
            if failed {
                continue;
            }
            match self.messenger.send(self.chat_id, &message.text).await {
                Ok(_) => delivered.extend(carried),
                Err(e) => {
                    let rules: Vec<&str> = carried
                        .iter()
                        .map(|&i| notices[i].incident().rule.as_str())
                        .collect();
                    error!(?rules, error = %e, "Failed to send a health notice");
                    failed = true;
                }
            }
        }
        for (index, notice) in notices.into_iter().enumerate() {
            if !delivered.contains(&index) {
                // Alerts and reminders are retried by the tracker on the
                // next bad reading; recoveries are kept here, and in the
                // store until delivered.
                if matches!(notice, Notice::Resolved { .. }) {
                    self.undelivered.push(notice);
                }
                continue;
            }
            let incident = notice.incident();
            info!(rule = incident.rule.as_str(), subject = %incident.subject.key(), "🩺 Health notice sent");
            let change = match &notice {
                Notice::Resolved { at, .. } => Some(Change::Delivered {
                    subject: incident.subject.clone(),
                    rule: incident.rule,
                    at: *at,
                }),
                _ => self
                    .tracker
                    .mark_notified(&incident.subject, incident.rule, now),
            };
            // A failed acknowledgement is retried with the other writes,
            // so the notice isn't sent again while the watchdog runs.
            if let Some(change) = change {
                // A recovery an earlier write held back may be released
                // here: it goes out next round.
                let released = self.persist(change, None).await;
                self.undelivered.extend(released);
            }
        }
    }
}

/// When the state change behind `notice` happened.
fn transition_time(notice: &Notice, now: u64) -> u64 {
    match notice {
        Notice::Opened(incident) => incident.opened_at,
        Notice::Reminder(_) => now,
        Notice::Resolved { at, .. } => *at,
    }
}

/// Publishes `heartbeat` and reads it back: whether any relay round-trips
/// it. Without a heartbeat, whether any relay is connected.
async fn probe(client: &Client, heartbeat: Option<&Event>) -> Option<bool> {
    let Some(heartbeat) = heartbeat else {
        let relays = client.relays().await;
        return Some(
            relays
                .values()
                .any(|relay| relay.status() == RelayStatus::Connected),
        );
    };
    match client.send_event(heartbeat).await {
        Ok(output) if output.success.is_empty() => {
            warn!(failed = ?output.failed, "No relay accepted the heartbeat");
            return Some(false);
        }
        Ok(_) => {}
        Err(e) => {
            warn!(error = %e, "Failed to publish the heartbeat");
            return Some(false);
        }
    }
    let answers = match client
        .stream_events(Filter::new().id(heartbeat.id))
        .timeout(PROBE_TIMEOUT)
        .await
    {
        Ok(answers) => answers,
        Err(e) => {
            warn!(error = %e, "Failed to read the heartbeat back");
            return Some(false);
        }
    };
    let mut answers = answers;
    while let Some((relay, answer)) = answers.next().await {
        if matches!(answer, Ok(ref event) if event.id == heartbeat.id) {
            debug!(%relay, "Heartbeat round-tripped");
            return Some(true);
        }
    }
    warn!("No relay returned the heartbeat it was sent");
    Some(false)
}

/// What the background task needs.
struct Wiring {
    client: Client,
    bot: Bot,
    chat_id: i64,
    config: MonitorConfig,
    mostro: PublicKey,
    keys: WatchdogKeys,
    pool: sqlx::SqlitePool,
    http: reqwest::Client,
}

/// Starts Mostro health monitoring in the background when anything is
/// configured to be watched. Returns a one-line summary for the startup
/// message. Fails startup only when the watchdog's key is set but invalid.
pub fn launch(
    config: &MonitorConfig,
    client: &Client,
    bot: &Bot,
    chat_id: i64,
    mostro: PublicKey,
    pool: sqlx::SqlitePool,
) -> Result<String, Box<dyn std::error::Error>> {
    if !config.is_active() {
        info!("Mostro health monitoring off");
        return Ok("disabled".into());
    }
    let env = |name: &str| std::env::var(name).ok();
    let sign = crate::config::read_optional_key(&config.private_key_env, env)?;
    let decrypt = crate::config::read_optional_key(&config.decrypt_key_env, env)?;
    let keys = WatchdogKeys {
        decrypt: decrypt.or_else(|| sign.clone()),
        sign,
    };
    match &keys.sign {
        Some(sign) => info!(
            "🩺 Mostro health monitoring on. Heartbeat signed by {} (hex)",
            sign.public_key().to_hex()
        ),
        None => warn!(
            "🩺 Mostro health monitoring on without a watchdog key ({} unset): no \
             heartbeat is published",
            config.private_key_env
        ),
    }
    match &keys.decrypt {
        Some(decrypt) => info!(
            "Health details are read when mostrod encrypts them to {} (hex); set it as \
             the health recipient in mostrod",
            decrypt.public_key().to_hex()
        ),
        None => warn!("No key to decrypt the health details: only the public tags are read"),
    }
    let http = reqwest::Client::builder()
        .user_agent(format!("mostro-watchdog/{}", crate::version::VERSION))
        .build()?;
    let summary = summary(config, keys.sign.is_some());
    tokio::spawn(run(Wiring {
        client: client.clone(),
        bot: bot.clone(),
        chat_id: config.alerts_chat_id.unwrap_or(chat_id),
        config: config.clone(),
        mostro,
        keys,
        pool,
        http,
    }));
    Ok(summary)
}

/// What is watched, for the startup message.
fn summary(config: &MonitorConfig, has_key: bool) -> String {
    let mut parts = Vec::new();
    if config.info_silence_threshold > 0 {
        parts.push(format!(
            "info event within {} s",
            config.info_silence_threshold
        ));
    }
    if config.alert_on_maintenance {
        parts.push("maintenance".to_string());
    }
    if config.mostro_health_enabled {
        parts.push("health event".to_string());
    }
    if config.heartbeat_enabled() && has_key {
        parts.push("heartbeat".to_string());
    }
    if !config.peer_watchdogs.is_empty() {
        parts.push(format!("{} peer watchdog(s)", config.peer_watchdogs.len()));
    }
    parts.join(", ")
}

/// One round every `check_interval`, forever. A failure to open the store
/// stops monitoring with an error; everything else is logged and retried.
async fn run(w: Wiring) {
    let store = match HealthStore::new(w.pool).await {
        Ok(store) => store,
        Err(e) => {
            error!(error = %e, "Health monitoring stopped: its store could not be opened");
            return;
        }
    };
    let interval = Duration::from_secs(w.config.check_interval);
    let sources = w.config.block_height_sources.clone();
    let started_at = now_ms() / 1000;
    let mut monitor = match Monitor::new(
        w.config, w.mostro, w.keys, w.chat_id, started_at, w.bot, store,
    )
    .await
    {
        Ok(monitor) => monitor,
        Err(e) => {
            error!(error = %e, "Health monitoring stopped: its state could not be read");
            return;
        }
    };
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        ticker.tick().await;
        let now_ms = now_ms();
        let heartbeat = monitor.next_heartbeat(now_ms).await;
        let relays_reachable = probe(&w.client, heartbeat.as_ref()).await;
        let now = now_ms / 1000;
        let events: Vec<Event> = match w
            .client
            .fetch_events(monitor.filters(now))
            .timeout(FETCH_TIMEOUT)
            .await
        {
            Ok(events) => events.into_iter().collect(),
            Err(e) => {
                warn!(error = %e, "Failed to fetch health events");
                Vec::new()
            }
        };
        monitor.ingest(&events, now).await;
        let explorer = match monitor
            .reference_network()
            .and_then(|network| sources.get(network))
        {
            Some(urls) if !urls.is_empty() => height::explorer_tip(&w.http, urls).await,
            _ => None,
        };
        monitor
            .assess(Round {
                now,
                relays_reachable,
                events: &[],
                explorer,
            })
            .await;
    }
}

#[cfg(test)]
mod tests;
