//! The rules: what counts as a bad reading of the instance, from its info
//! event, its health event and the watchdog's own observations. Pure
//! functions of a [`Snapshot`], so they are tested with any clock.
//!
//! A rule reads `Bad`, `Good` or `Unknown`. `Unknown` is no reading at all:
//! the tracker neither opens nor resolves anything on it. A missing or
//! stale health event makes the content rules unknown, never good.

use nostr_sdk::prelude::PublicKey;

use super::content::HealthDetails;
use super::event::{CheckState, Envelope, EscrowMode, HealthReport, Status};
use super::info::InfoSnapshot;
use super::settings::MonitorConfig;

/// How urgent an alert is. Critical alerts are repeated while active;
/// warnings are sent once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Critical,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "warning" => Some(Self::Warning),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

/// One condition the watchdog alerts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RuleId {
    RelaysUnreachable,
    InfoSilent,
    HealthSilent,
    StatusDown,
    LndUnsynced,
    BlockLag,
    NoPayoutSuccess,
    PayoutFailureRate,
    HtlcExpiry,
    StatusDegraded,
    SettledUnpaid,
    InactiveChannels,
    Maintenance,
    PeerWatchdogSilent,
}

impl RuleId {
    pub const ALL: [RuleId; 14] = [
        Self::RelaysUnreachable,
        Self::InfoSilent,
        Self::HealthSilent,
        Self::StatusDown,
        Self::LndUnsynced,
        Self::BlockLag,
        Self::NoPayoutSuccess,
        Self::PayoutFailureRate,
        Self::HtlcExpiry,
        Self::StatusDegraded,
        Self::SettledUnpaid,
        Self::InactiveChannels,
        Self::Maintenance,
        Self::PeerWatchdogSilent,
    ];

    /// The stored name. Never change one: open incidents are keyed by it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RelaysUnreachable => "relays_unreachable",
            Self::InfoSilent => "info_silent",
            Self::HealthSilent => "health_silent",
            Self::StatusDown => "status_down",
            Self::LndUnsynced => "lnd_unsynced",
            Self::BlockLag => "block_lag",
            Self::NoPayoutSuccess => "no_payout_success",
            Self::PayoutFailureRate => "payout_failure_rate",
            Self::HtlcExpiry => "htlc_expiry",
            Self::StatusDegraded => "status_degraded",
            Self::SettledUnpaid => "settled_unpaid",
            Self::InactiveChannels => "inactive_channels",
            Self::Maintenance => "maintenance",
            Self::PeerWatchdogSilent => "peer_watchdog_silent",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|rule| rule.as_str() == value)
    }

    pub fn severity(self) -> Severity {
        match self {
            Self::StatusDegraded
            | Self::SettledUnpaid
            | Self::InactiveChannels
            | Self::Maintenance
            | Self::PeerWatchdogSilent => Severity::Warning,
            _ => Severity::Critical,
        }
    }

    /// The headline of the alert.
    pub fn title(self) -> &'static str {
        match self {
            Self::RelaysUnreachable => "Relays unreachable from the watchdog",
            Self::InfoSilent => "Mostro info event silent",
            Self::HealthSilent => "Mostro is silent",
            Self::StatusDown => "Mostro reports itself down",
            Self::LndUnsynced => "LND not synced to chain",
            Self::BlockLag => "LND behind the chain",
            Self::NoPayoutSuccess => "Buyer payouts stuck",
            Self::PayoutFailureRate => "Buyer payouts failing",
            Self::HtlcExpiry => "HTLC close to expiry",
            Self::StatusDegraded => "Mostro reports itself degraded",
            Self::SettledUnpaid => "Orders waiting for their payout",
            Self::InactiveChannels => "Inactive Lightning channels",
            Self::Maintenance => "Mostro in maintenance mode",
            Self::PeerWatchdogSilent => "Peer watchdog silent",
        }
    }
}

/// What a rule made of the snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reading {
    Bad {
        detail: String,
        /// When the condition began, if the data says so. Otherwise the
        /// tracker uses the first bad reading it saw.
        since: Option<u64>,
        /// How long the condition must last before it alerts.
        min_duration: u64,
    },
    Good {
        /// What recovery looks like, for the "resolved" message.
        detail: Option<String>,
    },
    Unknown,
}

impl Reading {
    fn bad(detail: impl Into<String>) -> Self {
        Self::Bad {
            detail: detail.into(),
            since: None,
            min_duration: 0,
        }
    }

    fn good(detail: impl Into<String>) -> Self {
        Self::Good {
            detail: Some(detail.into()),
        }
    }

    const GOOD: Self = Self::Good { detail: None };
}

/// Who an alert is about: Mostro, a peer watchdog, or the watchdog's own
/// relays. Incidents are keyed by subject and rule.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Subject {
    Mostro(PublicKey),
    PeerWatchdog(PublicKey),
    Relays,
}

impl Subject {
    /// The stored key.
    pub fn key(&self) -> String {
        match self {
            Self::Mostro(pk) => pk.to_hex(),
            Self::PeerWatchdog(pk) => format!("watchdog:{}", pk.to_hex()),
            Self::Relays => "relays".into(),
        }
    }

    pub fn parse(key: &str) -> Option<Self> {
        if key == "relays" {
            return Some(Self::Relays);
        }
        if let Some(hex) = key.strip_prefix("watchdog:") {
            return PublicKey::from_hex(hex).ok().map(Self::PeerWatchdog);
        }
        PublicKey::from_hex(key).ok().map(Self::Mostro)
    }
}

/// An independent block height to compare LND with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    pub height: u64,
    /// Where it came from, for the message (e.g. `mempool.space`).
    pub source: String,
}

/// Everything the rules read, gathered once per evaluation.
#[derive(Debug, Clone)]
pub struct Snapshot<'a> {
    pub now: u64,
    /// When the watchdog started: nothing seen yet is unknown before a
    /// grace period has passed, silent after.
    pub started_at: u64,
    pub mostro: PublicKey,
    /// Whether the watchdog could round-trip an event through any relay.
    /// `None` when it couldn't tell.
    pub relays_reachable: Option<bool>,
    /// The newest valid info event.
    pub info: Option<InfoSnapshot>,
    /// The last accepted health event, possibly stale.
    pub health: Option<&'a HealthReport>,
    /// Its decrypted content, when there is one the watchdog can trust.
    pub details: Option<&'a HealthDetails>,
    pub reference: Option<Reference>,
    /// Each peer watchdog and its last accepted heartbeat.
    pub peers: Vec<(PublicKey, Option<Envelope>)>,
}

impl Snapshot<'_> {
    /// Seconds since the watchdog started.
    fn uptime(&self) -> u64 {
        self.now.saturating_sub(self.started_at)
    }

    fn relays_down(&self) -> bool {
        self.relays_reachable == Some(false)
    }

    /// The health event when it is fresh enough to read.
    fn fresh_health(&self) -> Option<&HealthReport> {
        self.health.filter(|h| h.envelope.is_fresh(self.now))
    }

    /// The content of a fresh health event.
    fn fresh_details(&self) -> Option<&HealthDetails> {
        self.fresh_health().and(self.details)
    }
}

/// Every reading of `snapshot` under `config`.
pub fn evaluate(snapshot: &Snapshot, config: &MonitorConfig) -> Vec<(Subject, RuleId, Reading)> {
    let mostro = Subject::Mostro(snapshot.mostro);
    let mut readings = vec![(
        Subject::Relays,
        RuleId::RelaysUnreachable,
        relays_unreachable(snapshot),
    )];
    if config.info_silence_threshold > 0 {
        readings.push((
            mostro.clone(),
            RuleId::InfoSilent,
            info_silent(snapshot, config.info_silence_threshold),
        ));
    }
    if config.alert_on_maintenance {
        readings.push((mostro.clone(), RuleId::Maintenance, maintenance(snapshot)));
    }
    if config.mostro_health_enabled {
        let t = &config.thresholds;
        let grace = 3 * config.check_interval;
        let health_rules = [
            (RuleId::HealthSilent, health_silent(snapshot, grace)),
            (RuleId::StatusDown, status_down(snapshot)),
            (
                RuleId::StatusDegraded,
                status_degraded(snapshot, t.degraded_for_secs),
            ),
            (
                RuleId::LndUnsynced,
                lnd_unsynced(snapshot, t.unsynced_for_secs),
            ),
            (
                RuleId::BlockLag,
                block_lag(snapshot, t.block_lag_blocks, t.block_lag_for_secs),
            ),
            (
                RuleId::NoPayoutSuccess,
                no_payout_success(snapshot, t.no_payout_success_secs),
            ),
            (
                RuleId::PayoutFailureRate,
                payout_failure_rate(
                    snapshot,
                    t.payout_failure_rate,
                    t.payout_failure_min_attempts,
                ),
            ),
            (
                RuleId::HtlcExpiry,
                htlc_expiry(snapshot, t.htlc_expiry_blocks),
            ),
            (
                RuleId::SettledUnpaid,
                settled_unpaid(snapshot, t.settled_unpaid_warn_secs),
            ),
            (
                RuleId::InactiveChannels,
                inactive_channels(snapshot, t.inactive_channels_warn_secs),
            ),
        ];
        readings.extend(
            health_rules
                .into_iter()
                .map(|(rule, reading)| (mostro.clone(), rule, reading)),
        );
    }
    let grace = 3 * config.check_interval;
    readings.extend(snapshot.peers.iter().map(|(peer, beat)| {
        (
            Subject::PeerWatchdog(*peer),
            RuleId::PeerWatchdogSilent,
            peer_silent(snapshot, beat.as_ref(), grace),
        )
    }));
    readings
}

/// `"2 h 5 min"`, `"7 min"`, `"40 s"`.
pub fn human_duration(secs: u64) -> String {
    let (days, hours, minutes) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60);
    match (days, hours, minutes) {
        (0, 0, 0) => format!("{secs} s"),
        (0, 0, m) => format!("{m} min"),
        (0, h, m) => format!("{h} h {m} min"),
        (d, h, _) => format!("{d} d {h} h"),
    }
}

fn relays_unreachable(s: &Snapshot) -> Reading {
    match s.relays_reachable {
        Some(false) => Reading::bad(
            "The watchdog could not read its own event back from any relay. \
             Mostro's state is unknown until a relay answers.",
        ),
        Some(true) => Reading::good("The watchdog reaches its relays again."),
        None => Reading::Unknown,
    }
}

fn info_silent(s: &Snapshot, threshold: u64) -> Reading {
    if s.relays_down() {
        return Reading::Unknown;
    }
    match &s.info {
        Some(info) => {
            let age = s.now.saturating_sub(info.created_at);
            if age > threshold {
                Reading::Bad {
                    detail: format!(
                        "No info event (kind 38385) for {}. mostrod may be down.",
                        human_duration(age)
                    ),
                    since: Some(info.created_at),
                    min_duration: 0,
                }
            } else {
                Reading::good("mostrod publishes its info event again.")
            }
        }
        None if s.uptime() > threshold => Reading::bad(format!(
            "No info event (kind 38385) seen since the watchdog started {} ago.",
            human_duration(s.uptime())
        )),
        None => Reading::Unknown,
    }
}

fn maintenance(s: &Snapshot) -> Reading {
    // The health event is newer and more frequent; the info event is the
    // fallback that works with any mostrod.
    let (enabled, reason) = match (s.fresh_health(), &s.info) {
        (Some(health), _) => (
            health.maintenance_mode,
            s.fresh_details()
                .and_then(|d| d.maintenance.as_ref())
                .and_then(|m| m.reason.clone()),
        ),
        (None, Some(info)) => (info.maintenance_mode, None),
        (None, None) => return Reading::Unknown,
    };
    if !enabled {
        return Reading::good("Mostro accepts new orders again.");
    }
    let why = match reason.as_deref() {
        Some("lnd_unsynced") => " mostrod turned it on by itself because LND is not synced.".into(),
        Some(reason) => format!(" Reason: {reason}."),
        None => String::new(),
    };
    Reading::bad(format!(
        "New orders and takes are rejected; open trades can finish.{why}"
    ))
}

fn health_silent(s: &Snapshot, grace: u64) -> Reading {
    if s.relays_down() {
        return Reading::Unknown;
    }
    match s.health {
        Some(health) if health.envelope.is_fresh(s.now) => Reading::good(format!(
            "Mostro publishes its health event again.{}",
            recent_outage(s)
        )),
        Some(health) => Reading::Bad {
            detail: format!(
                "Last health event {} ago (interval {} s).",
                human_duration(s.now.saturating_sub(health.envelope.created_at)),
                health.envelope.interval
            ),
            since: Some(health.envelope.created_at),
            min_duration: 0,
        },
        None if s.uptime() > grace => Reading::bad(
            "No health event (kind 38389) from Mostro since the watchdog started. \
             Is mostrod publishing it?",
        ),
        None => Reading::Unknown,
    }
}

/// How long a relay outage Mostro reports stays worth mentioning.
const RECENT_OUTAGE_SECS: u64 = 3600;

/// " It reports its relays were unreachable for 12 min." when Mostro's
/// payload says its relays were all down recently: the likely reason it
/// went silent. Empty otherwise.
fn recent_outage(s: &Snapshot) -> String {
    let Some(relays) = s.fresh_details().and_then(|d| d.relays.as_ref()) else {
        return String::new();
    };
    match (relays.last_outage_started_at, relays.last_outage_ended_at) {
        (Some(start), Some(end))
            if end >= start && s.now.saturating_sub(end) <= RECENT_OUTAGE_SECS =>
        {
            format!(
                " It reports its relays were unreachable for {}.",
                human_duration(end - start)
            )
        }
        _ => String::new(),
    }
}

/// The checks that are not ok, as `lnd_sync: fail`, with the earliest
/// `since` among them.
fn failing_checks(health: &HealthReport) -> (String, Option<u64>) {
    let failing: Vec<_> = health
        .checks
        .iter()
        .filter(|c| c.state != CheckState::Ok)
        .collect();
    let list = failing
        .iter()
        .map(|c| format!("{}: {}", c.name.as_str(), c.state.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    (list, failing.iter().map(|c| c.since).min())
}

fn status_down(s: &Snapshot) -> Reading {
    let Some(health) = s.fresh_health() else {
        return Reading::Unknown;
    };
    if health.status != Status::Down {
        return Reading::good(format!("Mostro reports {}.", health.status.as_str()));
    }
    let (list, since) = failing_checks(health);
    Reading::Bad {
        detail: format!("Failing checks: {list}."),
        since,
        min_duration: 0,
    }
}

fn status_degraded(s: &Snapshot, min_duration: u64) -> Reading {
    let Some(health) = s.fresh_health() else {
        return Reading::Unknown;
    };
    match health.status {
        // Down has its own alert; this one stays as it is meanwhile.
        Status::Down => Reading::Unknown,
        Status::Ok => Reading::good("Mostro reports ok."),
        Status::Degraded => {
            let (list, since) = failing_checks(health);
            // Degraded only because of maintenance: that has its own alert.
            if list.is_empty() {
                return Reading::GOOD;
            }
            Reading::Bad {
                detail: format!("Checks not ok: {list}."),
                since,
                min_duration,
            }
        }
    }
}

/// When LND's current run of `synced_to_chain = false` began, from the
/// payload or else the `lnd_sync` check. `None` while LND reports synced.
fn unsynced_since(s: &Snapshot, health: &HealthReport) -> Option<u64> {
    if health.lnd_synced_to_chain != Some(false) {
        return None;
    }
    s.fresh_details()
        .and_then(|d| d.lnd.as_ref())
        .and_then(|lnd| lnd.unsynced_since)
        .or_else(|| {
            health
                .check(super::event::CheckName::LndSync)
                .filter(|c| matches!(c.state, CheckState::Warn | CheckState::Fail))
                .map(|c| c.since)
        })
}

fn lnd_unsynced(s: &Snapshot, min_duration: u64) -> Reading {
    let Some(health) = s.fresh_health() else {
        return Reading::Unknown;
    };
    match health.lnd_synced_to_chain {
        Some(true) => Reading::good(format!(
            "LND is synced at height {}.",
            health
                .lnd_block_height
                .map_or("unknown".into(), |h| h.to_string())
        )),
        Some(false) => {
            let since = unsynced_since(s, health);
            let at = health
                .lnd_block_height
                .map_or(String::new(), |h| format!(" at height {h}"));
            Reading::Bad {
                detail: format!("LND reports synced_to_chain = false{at}."),
                since,
                min_duration,
            }
        }
        // No GetInfo: the `lnd_sync` check is unknown, and status is down.
        None => Reading::Unknown,
    }
}

fn block_lag(s: &Snapshot, min_lag: u64, min_duration: u64) -> Reading {
    let Some(health) = s.fresh_health() else {
        return Reading::Unknown;
    };
    if health.escrow_mode != EscrowMode::Lightning {
        return Reading::Unknown;
    }
    let (Some(height), Some(reference)) = (health.lnd_block_height, s.reference.as_ref()) else {
        return Reading::Unknown;
    };
    let lag = reference.height.saturating_sub(height);
    if lag >= min_lag {
        Reading::Bad {
            detail: format!(
                "LND is {lag} blocks behind ({height} vs {} from {}).",
                reference.height, reference.source
            ),
            // When LND admits it is not synced, the lag dates from then.
            // When it claims to be synced, only the watchdog's own
            // observation counts.
            since: unsynced_since(s, health),
            min_duration,
        }
    } else {
        Reading::good(format!(
            "LND at {height}, reference {} from {}.",
            reference.height, reference.source
        ))
    }
}

fn no_payout_success(s: &Snapshot, threshold: u64) -> Reading {
    let Some(payouts) = s.fresh_details().and_then(|d| d.payouts.as_ref()) else {
        return Reading::Unknown;
    };
    let Some(waiting) = payouts.settled_unpaid_orders else {
        return Reading::Unknown;
    };
    let oldest = payouts.oldest_settled_unpaid_order_age_secs.unwrap_or(0);
    let since_success = payouts
        .last_payout_success_at
        .map(|at| s.now.saturating_sub(at));
    let stuck =
        waiting > 0 && oldest >= threshold && since_success.is_none_or(|age| age >= threshold);
    if !stuck {
        return Reading::good("Buyer payouts succeed again.");
    }
    let last = since_success.map_or("never".into(), |age| format!("{} ago", human_duration(age)));
    Reading::Bad {
        detail: format!(
            "Last successful payout: {last}. {waiting} order(s) settled but unpaid \
             (oldest {}).",
            human_duration(oldest)
        ),
        since: Some(s.now.saturating_sub(oldest)),
        min_duration: 0,
    }
}

fn payout_failure_rate(s: &Snapshot, max_rate: f64, min_attempts: u64) -> Reading {
    let Some(payouts) = s.fresh_details().and_then(|d| d.payouts.as_ref()) else {
        return Reading::Unknown;
    };
    let (Some(succeeded), Some(failed)) = (payouts.payouts_succeeded_1h, payouts.payouts_failed_1h)
    else {
        return Reading::Unknown;
    };
    let attempts = succeeded + failed;
    if attempts < min_attempts || (failed as f64) <= max_rate * attempts as f64 {
        return Reading::good("Most buyer payouts succeed again.");
    }
    let reasons = payouts
        .failure_reasons_1h
        .iter()
        .filter(|(_, count)| **count > 0)
        .map(|(reason, count)| format!("{reason} {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let window = human_duration(payouts.window_secs.unwrap_or(3600));
    let reasons = if reasons.is_empty() {
        String::new()
    } else {
        format!(" Reasons: {reasons}.")
    };
    Reading::bad(format!(
        "{failed} of {attempts} buyer payouts failed in the last {window}.{reasons}"
    ))
}

fn htlc_expiry(s: &Snapshot, min_blocks: i64) -> Reading {
    let Some(lnd) = s.fresh_details().and_then(|d| d.lnd.as_ref()) else {
        return Reading::Unknown;
    };
    if lnd.reachable != Some(true) {
        return Reading::Unknown;
    }
    match lnd.nearest_htlc_expiry_blocks {
        Some(blocks) if blocks < min_blocks => Reading::bad(format!(
            "The nearest pending HTLC expires in {blocks} blocks ({} pending).",
            lnd.pending_htlcs.map_or("?".into(), |n| n.to_string())
        )),
        _ => Reading::good("No pending HTLC is close to its expiry."),
    }
}

fn settled_unpaid(s: &Snapshot, threshold: u64) -> Reading {
    let Some(payouts) = s.fresh_details().and_then(|d| d.payouts.as_ref()) else {
        return Reading::Unknown;
    };
    match payouts.oldest_settled_unpaid_order_age_secs {
        Some(age) if age >= threshold => Reading::Bad {
            detail: format!(
                "{} order(s) settled but unpaid; the oldest for {}.",
                payouts.settled_unpaid_orders.unwrap_or(1),
                human_duration(age)
            ),
            since: Some(s.now.saturating_sub(age)),
            min_duration: 0,
        },
        _ => Reading::good("No order waits long for its payout."),
    }
}

fn inactive_channels(s: &Snapshot, min_duration: u64) -> Reading {
    let Some(lnd) = s.fresh_details().and_then(|d| d.lnd.as_ref()) else {
        return Reading::Unknown;
    };
    match lnd.channels_inactive {
        Some(0) => Reading::good("Every channel is active again."),
        Some(n) => Reading::Bad {
            detail: format!(
                "{n} channel(s) inactive, {} active.",
                lnd.channels_active.map_or("?".into(), |a| a.to_string())
            ),
            since: None,
            min_duration,
        },
        None => Reading::Unknown,
    }
}

fn peer_silent(s: &Snapshot, beat: Option<&Envelope>, grace: u64) -> Reading {
    if s.relays_down() {
        return Reading::Unknown;
    }
    match beat {
        Some(beat) if beat.is_fresh(s.now) => Reading::good("Its heartbeat is back."),
        Some(beat) => Reading::Bad {
            detail: format!(
                "Last heartbeat {} ago.",
                human_duration(s.now.saturating_sub(beat.created_at))
            ),
            since: Some(beat.created_at),
            min_duration: 0,
        },
        None if s.uptime() > grace => {
            Reading::bad("No heartbeat seen since this watchdog started.")
        }
        None => Reading::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::content::tests::payload;
    use crate::health::event::parse_health;
    use crate::health::event::tests::{health_tags, set_tag, sign, NOW};
    use nostr_sdk::prelude::Keys;

    fn config() -> MonitorConfig {
        MonitorConfig {
            mostro_health_enabled: true,
            ..MonitorConfig::default()
        }
    }

    fn report_with(mostro: &Keys, edit: impl FnOnce(&mut Vec<Vec<String>>)) -> HealthReport {
        let mut tags = health_tags(mostro, NOW, 1);
        edit(&mut tags);
        parse_health(&sign(mostro, tags, "", NOW), mostro.public_key(), NOW).unwrap()
    }

    fn details(edit: impl FnOnce(&mut serde_json::Value)) -> HealthDetails {
        let mut body = payload();
        edit(&mut body);
        serde_json::from_value(body).unwrap()
    }

    fn snapshot<'a>(
        mostro: &Keys,
        health: Option<&'a HealthReport>,
        details: Option<&'a HealthDetails>,
    ) -> Snapshot<'a> {
        Snapshot {
            now: NOW,
            started_at: NOW - 3600,
            mostro: mostro.public_key(),
            relays_reachable: Some(true),
            info: Some(InfoSnapshot {
                created_at: NOW - 60,
                maintenance_mode: false,
            }),
            health,
            details,
            reference: Some(Reference {
                height: 971_485,
                source: "mempool.space".into(),
            }),
            peers: Vec::new(),
        }
    }

    fn reading(snapshot: &Snapshot, rule: RuleId) -> Reading {
        evaluate(snapshot, &config())
            .into_iter()
            .find(|(_, r, _)| *r == rule)
            .map(|(_, _, reading)| reading)
            .unwrap_or_else(|| panic!("no reading for {rule:?}"))
    }

    fn is_bad(reading: &Reading) -> bool {
        matches!(reading, Reading::Bad { .. })
    }

    /// The 2026-10-14 incident: LND stuck 136 blocks behind, unsynced for
    /// hours, payouts failing.
    fn incident(mostro: &Keys) -> (HealthReport, HealthDetails) {
        let report = report_with(mostro, |tags| {
            set_tag(tags, "status", &["down"]);
            set_tag(tags, "lnd_block_height", &["971349"]);
            set_tag(tags, "lnd_synced_to_chain", &["false"]);
            tags.retain(|t| t[0] != "check" || t[1] != "lnd_sync");
            tags.push(vec![
                "check".into(),
                "lnd_sync".into(),
                "fail".into(),
                "1791952200".into(),
            ]);
        });
        let details = details(|body| {
            body["lnd"]["block_height"] = 971_349.into();
            body["lnd"]["synced_to_chain"] = false.into();
            body["lnd"]["unsynced_since"] = 1_791_951_600u64.into();
            body["payouts"]["payouts_succeeded_1h"] = 0.into();
            body["payouts"]["payouts_failed_1h"] = 37.into();
            body["payouts"]["failure_reasons_1h"] =
                serde_json::json!({"incorrect_payment_details": 31, "no_route": 6});
            body["payouts"]["last_payout_success_at"] = 1_791_950_000u64.into();
            body["payouts"]["settled_unpaid_orders"] = 4.into();
            body["payouts"]["oldest_settled_unpaid_order_age_secs"] = 61_200.into();
        });
        (report, details)
    }

    #[test]
    fn the_incident_trips_every_critical_rule_it_should() {
        let mostro = Keys::generate();
        let (report, details) = incident(&mostro);
        let s = snapshot(&mostro, Some(&report), Some(&details));

        for rule in [
            RuleId::StatusDown,
            RuleId::LndUnsynced,
            RuleId::BlockLag,
            RuleId::NoPayoutSuccess,
            RuleId::PayoutFailureRate,
            RuleId::SettledUnpaid,
        ] {
            assert!(is_bad(&reading(&s, rule)), "{rule:?}");
        }
        let Reading::Bad { detail, .. } = reading(&s, RuleId::BlockLag) else {
            unreachable!()
        };
        assert_eq!(
            detail,
            "LND is 136 blocks behind (971349 vs 971485 from mempool.space)."
        );
        assert_eq!(
            reading(&s, RuleId::HealthSilent),
            Reading::good("Mostro publishes its health event again.")
        );
    }

    #[test]
    fn a_healthy_report_reads_good_everywhere() {
        let mostro = Keys::generate();
        let report = report_with(&mostro, |_| {});
        let details = details(|body| body["lnd"]["channels_inactive"] = 0.into());
        let s = snapshot(&mostro, Some(&report), Some(&details));

        for (_, rule, reading) in evaluate(&s, &config()) {
            assert!(
                matches!(reading, Reading::Good { .. }),
                "{rule:?}: {reading:?}"
            );
        }
    }

    #[test]
    fn the_unsynced_rule_takes_its_start_from_the_payload() {
        let mostro = Keys::generate();
        let (report, details) = incident(&mostro);
        let s = snapshot(&mostro, Some(&report), Some(&details));

        let Reading::Bad {
            since,
            min_duration,
            ..
        } = reading(&s, RuleId::LndUnsynced)
        else {
            panic!("expected bad");
        };
        assert_eq!(since, Some(1_791_951_600));
        assert_eq!(min_duration, 600);

        // Without the payload, the `lnd_sync` check's `since` stands in.
        let s = snapshot(&mostro, Some(&report), None);
        let Reading::Bad { since, .. } = reading(&s, RuleId::LndUnsynced) else {
            panic!("expected bad");
        };
        assert_eq!(since, Some(1_791_952_200));
    }

    #[test]
    fn content_rules_are_unknown_without_a_payload() {
        let mostro = Keys::generate();
        let (report, _) = incident(&mostro);
        let s = snapshot(&mostro, Some(&report), None);

        for rule in [
            RuleId::NoPayoutSuccess,
            RuleId::PayoutFailureRate,
            RuleId::HtlcExpiry,
            RuleId::SettledUnpaid,
            RuleId::InactiveChannels,
        ] {
            assert_eq!(reading(&s, rule), Reading::Unknown, "{rule:?}");
        }
        // The public tags still carry the essentials.
        assert!(is_bad(&reading(&s, RuleId::StatusDown)));
        assert!(is_bad(&reading(&s, RuleId::BlockLag)));
    }

    #[test]
    fn a_stale_event_is_silence_and_its_data_is_unknown_not_good() {
        let mostro = Keys::generate();
        let (report, details) = incident(&mostro);
        let mut s = snapshot(&mostro, Some(&report), Some(&details));
        s.now = NOW + 180;

        assert!(is_bad(&reading(&s, RuleId::HealthSilent)));
        for rule in [
            RuleId::StatusDown,
            RuleId::BlockLag,
            RuleId::LndUnsynced,
            RuleId::NoPayoutSuccess,
        ] {
            assert_eq!(reading(&s, rule), Reading::Unknown, "{rule:?}");
        }
    }

    #[test]
    fn no_health_event_is_silence_only_after_a_grace_period() {
        let mostro = Keys::generate();
        let mut s = snapshot(&mostro, None, None);

        assert!(is_bad(&reading(&s, RuleId::HealthSilent)));

        s.started_at = NOW - 60;
        assert_eq!(reading(&s, RuleId::HealthSilent), Reading::Unknown);
    }

    #[test]
    fn unreachable_relays_make_silence_unknown_and_alert_on_their_own() {
        let mostro = Keys::generate();
        let mut s = snapshot(&mostro, None, None);
        s.relays_reachable = Some(false);
        s.info = None;

        assert_eq!(reading(&s, RuleId::HealthSilent), Reading::Unknown);
        assert_eq!(reading(&s, RuleId::InfoSilent), Reading::Unknown);
        assert!(is_bad(&reading(&s, RuleId::RelaysUnreachable)));
    }

    #[test]
    fn a_lag_below_the_threshold_or_without_a_reference_does_not_alert() {
        let mostro = Keys::generate();
        let report = report_with(&mostro, |tags| {
            set_tag(tags, "lnd_block_height", &["971483"])
        });
        let mut s = snapshot(&mostro, Some(&report), None);

        assert!(matches!(
            reading(&s, RuleId::BlockLag),
            Reading::Good { .. }
        ));

        s.reference = None;
        assert_eq!(reading(&s, RuleId::BlockLag), Reading::Unknown);
    }

    #[test]
    fn a_lag_while_lnd_admits_being_unsynced_dates_from_then() {
        let mostro = Keys::generate();
        let (report, details) = incident(&mostro);
        let s = snapshot(&mostro, Some(&report), Some(&details));

        let Reading::Bad { since, .. } = reading(&s, RuleId::BlockLag) else {
            panic!("expected bad");
        };
        assert_eq!(since, Some(1_791_951_600));

        // LND claiming to be synced while behind: only the watchdog's own
        // observation dates it.
        let lying = report_with(&mostro, |tags| {
            set_tag(tags, "lnd_block_height", &["971349"])
        });
        let s = snapshot(&mostro, Some(&lying), None);
        let Reading::Bad { since, .. } = reading(&s, RuleId::BlockLag) else {
            panic!("expected bad");
        };
        assert_eq!(since, None);
    }

    #[test]
    fn coming_back_from_silence_mentions_a_recent_relay_outage() {
        let mostro = Keys::generate();
        let report = report_with(&mostro, |_| {});
        let details = details(|body| {
            body["relays"]["last_outage_started_at"] = (NOW - 1500).into();
            body["relays"]["last_outage_ended_at"] = (NOW - 300).into();
        });
        let s = snapshot(&mostro, Some(&report), Some(&details));

        let Reading::Good {
            detail: Some(detail),
        } = reading(&s, RuleId::HealthSilent)
        else {
            panic!("expected good");
        };
        assert!(detail.ends_with("unreachable for 20 min."), "{detail}");
    }

    #[test]
    fn the_failure_rate_needs_enough_attempts() {
        let mostro = Keys::generate();
        let report = report_with(&mostro, |_| {});
        let two_failures = details(|body| {
            body["payouts"]["payouts_succeeded_1h"] = 0.into();
            body["payouts"]["payouts_failed_1h"] = 2.into();
        });
        let half = details(|body| {
            body["payouts"]["payouts_succeeded_1h"] = 2.into();
            body["payouts"]["payouts_failed_1h"] = 2.into();
        });

        for d in [&two_failures, &half] {
            let s = snapshot(&mostro, Some(&report), Some(d));
            assert!(matches!(
                reading(&s, RuleId::PayoutFailureRate),
                Reading::Good { .. }
            ));
        }
    }

    #[test]
    fn no_successful_payout_needs_an_order_that_waited_as_long() {
        let mostro = Keys::generate();
        let report = report_with(&mostro, |_| {});
        // No success for 3 h, but the only waiting order settled a minute ago.
        let fresh_order = details(|body| {
            body["payouts"]["last_payout_success_at"] = (NOW - 3 * 3600).into();
            body["payouts"]["settled_unpaid_orders"] = 1.into();
            body["payouts"]["oldest_settled_unpaid_order_age_secs"] = 60.into();
        });
        let s = snapshot(&mostro, Some(&report), Some(&fresh_order));

        assert!(matches!(
            reading(&s, RuleId::NoPayoutSuccess),
            Reading::Good { .. }
        ));
    }

    #[test]
    fn maintenance_reads_the_health_event_or_falls_back_to_the_info_event() {
        let mostro = Keys::generate();
        let report = report_with(&mostro, |tags| {
            set_tag(tags, "maintenance_mode", &["true"]);
            set_tag(tags, "status", &["degraded"]);
        });
        let auto = details(|body| {
            body["maintenance"] = serde_json::json!({"enabled": true, "reason": "lnd_unsynced"});
        });
        let s = snapshot(&mostro, Some(&report), Some(&auto));

        let Reading::Bad { detail, .. } = reading(&s, RuleId::Maintenance) else {
            panic!("expected bad");
        };
        assert!(detail.contains("by itself"), "{detail}");
        // Degraded only because of maintenance: no second alert.
        assert!(matches!(
            reading(&s, RuleId::StatusDegraded),
            Reading::Good { .. }
        ));

        let mut s = snapshot(&mostro, None, None);
        s.info = Some(InfoSnapshot {
            created_at: NOW - 60,
            maintenance_mode: true,
        });
        assert!(is_bad(&reading(&s, RuleId::Maintenance)));
    }

    #[test]
    fn phase_zero_works_without_the_health_event() {
        let mostro = Keys::generate();
        let mut s = snapshot(&mostro, None, None);
        s.info = Some(InfoSnapshot {
            created_at: NOW - 901,
            maintenance_mode: false,
        });
        let config = MonitorConfig {
            info_silence_threshold: 900,
            ..MonitorConfig::default()
        };

        let readings = evaluate(&s, &config);

        assert!(readings
            .iter()
            .all(|(_, rule, _)| *rule != RuleId::HealthSilent));
        let info = readings
            .iter()
            .find(|(_, rule, _)| *rule == RuleId::InfoSilent)
            .unwrap();
        assert!(is_bad(&info.2));
    }

    #[test]
    fn a_peer_watchdog_is_silent_once_its_heartbeat_is_stale() {
        let mostro = Keys::generate();
        let peer = Keys::generate().public_key();
        let mut s = snapshot(&mostro, None, None);
        let beat = Envelope {
            id: nostr_sdk::prelude::EventId::from_byte_array([0; 32]),
            pubkey: peer,
            created_at: NOW - 200,
            interval: 60,
            seq: 1,
            expiration: NOW - 20,
        };
        s.peers = vec![(peer, Some(beat))];

        let readings = evaluate(&s, &config());
        let (subject, _, reading) = readings
            .iter()
            .find(|(_, rule, _)| *rule == RuleId::PeerWatchdogSilent)
            .unwrap();

        assert_eq!(subject, &Subject::PeerWatchdog(peer));
        assert!(is_bad(reading));
    }

    #[test]
    fn subjects_and_rules_round_trip_through_their_stored_names() {
        let pk = Keys::generate().public_key();
        for subject in [
            Subject::Mostro(pk),
            Subject::PeerWatchdog(pk),
            Subject::Relays,
        ] {
            assert_eq!(Subject::parse(&subject.key()), Some(subject));
        }
        for rule in RuleId::ALL {
            assert_eq!(RuleId::parse(rule.as_str()), Some(rule));
        }
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(human_duration(40), "40 s");
        assert_eq!(human_duration(420), "7 min");
        assert_eq!(human_duration(74_460), "20 h 41 min");
        assert_eq!(human_duration(2 * 86_400 + 3 * 3600), "2 d 3 h");
    }
}
