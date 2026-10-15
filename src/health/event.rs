//! Reading health events (kind 38389) as the protocol's "What a reader
//! owes" requires: verify the signature and every required tag before
//! trusting anything, take the newest valid revision by `seq`, and treat a
//! missing, expired or stale event as unknown, never as ok.
//!
//! The same kind carries two shapes: an instance's report (`z` = `health`)
//! and a watcher's heartbeat (`z` = `watcher`).

use std::collections::HashSet;

use nostr_sdk::prelude::*;

/// Health events and watcher heartbeats, kind 38389.
pub const HEALTH_KIND: Kind = Kind::Custom(38389);

/// `z` of an instance's health event.
pub const Z_HEALTH: &str = "health";

/// `z` of a watcher's heartbeat.
pub const Z_WATCHER: &str = "watcher";

/// Allowed range of the `interval` tag, in seconds.
pub const MIN_INTERVAL: u64 = 30;
pub const MAX_INTERVAL: u64 = 3600;

/// Largest accepted `seq`: 2^53 − 1.
pub const MAX_SEQ: u64 = 9_007_199_254_740_991;

/// An event dated further than this into the future is ignored.
pub const MAX_FUTURE_SKEW_SECS: u64 = 300;

/// The daemon's self-assessed state (`status` tag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Degraded,
    Down,
}

impl Status {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "ok" => Some(Self::Ok),
            "degraded" => Some(Self::Degraded),
            "down" => Some(Self::Down),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Down => "down",
        }
    }
}

/// The checks of version 1 of the event. Readers ignore other names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CheckName {
    LndSync,
    Payouts,
    Relays,
    Database,
}

impl CheckName {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "lnd_sync" => Some(Self::LndSync),
            "payouts" => Some(Self::Payouts),
            "relays" => Some(Self::Relays),
            "database" => Some(Self::Database),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LndSync => "lnd_sync",
            Self::Payouts => "payouts",
            Self::Relays => "relays",
            Self::Database => "database",
        }
    }
}

/// The state of a check. A state this version does not know is `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Ok,
    Warn,
    Fail,
    Unknown,
}

impl CheckState {
    fn parse(value: &str) -> Self {
        match value {
            "ok" => Self::Ok,
            "warn" => Self::Warn,
            "fail" => Self::Fail,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::Unknown => "unknown",
        }
    }
}

/// One `check` tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Check {
    pub name: CheckName,
    pub state: CheckState,
    /// Unix seconds the check entered `state`.
    pub since: u64,
}

/// `escrow_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscrowMode {
    Lightning,
    Cashu,
}

/// The fields every kind-38389 event carries, health event or heartbeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub id: EventId,
    pub pubkey: PublicKey,
    pub created_at: u64,
    pub interval: u64,
    pub seq: u64,
    pub expiration: u64,
}

impl Envelope {
    /// Whether the event can still be trusted at `now`: before its
    /// `expiration` and no older than three intervals.
    pub fn is_fresh(&self, now: u64) -> bool {
        now < self.expiration && now.saturating_sub(self.created_at) <= 3 * self.interval
    }
}

/// A verified instance health event (`z` = `health`).
#[derive(Debug, Clone, PartialEq)]
pub struct HealthReport {
    pub envelope: Envelope,
    /// The instance name from the `y` tag, when given.
    pub name: Option<String>,
    pub status: Status,
    pub checks: Vec<Check>,
    pub escrow_mode: EscrowMode,
    pub lnd_block_height: Option<u64>,
    pub lnd_synced_to_chain: Option<bool>,
    /// The chain LND is on (`mainnet`, `testnet`, `testnet4`, `signet`,
    /// `regtest` or `simnet`). Heights only compare within one network.
    pub lnd_network: Option<String>,
    pub maintenance_mode: bool,
    pub mostro_version: String,
    /// The pubkey the content is encrypted to, when there is content.
    pub recipient: Option<PublicKey>,
    /// The encrypted content, empty when there is none.
    pub content: String,
}

impl HealthReport {
    pub fn check(&self, name: CheckName) -> Option<Check> {
        self.checks.iter().copied().find(|c| c.name == name)
    }
}

/// A verified watcher heartbeat (`z` = `watcher`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatcherBeat {
    pub envelope: Envelope,
    pub software: String,
    pub watching: Vec<PublicKey>,
}

/// Why an event was ignored. Logged, never forwarded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Rejection {
    #[error("invalid signature")]
    BadSignature,
    #[error("not kind 38389")]
    WrongKind,
    #[error("author is not the expected pubkey")]
    WrongAuthor,
    #[error("`d` is not the author's pubkey")]
    WrongAddress,
    #[error("`z` is not {0:?}")]
    WrongZ(&'static str),
    #[error("dated more than {MAX_FUTURE_SKEW_SECS} s in the future")]
    FromTheFuture,
    #[error("missing or malformed `{0}` tag")]
    Malformed(&'static str),
    #[error("`expiration` is not created_at + 3 × interval")]
    WrongExpiration,
}

/// Reads the single tag named `name`: `Ok(None)` when absent, an error
/// when it appears more than once.
fn single_tag<'a>(event: &'a Event, name: &'static str) -> Result<Option<&'a [String]>, Rejection> {
    let mut found = event
        .tags
        .iter()
        .map(|tag| tag.as_slice())
        .filter(|values| values.first().map(String::as_str) == Some(name));
    let first = found.next();
    if found.next().is_some() {
        return Err(Rejection::Malformed(name));
    }
    Ok(first)
}

/// The value of the single tag `name`, which must be present.
fn required_value<'a>(event: &'a Event, name: &'static str) -> Result<&'a str, Rejection> {
    single_tag(event, name)?
        .and_then(|values| values.get(1))
        .map(String::as_str)
        .ok_or(Rejection::Malformed(name))
}

/// A base-10 integer with no sign, no leading zeros and no fraction.
fn parse_integer(value: &str) -> Option<u64> {
    let canonical = !value.is_empty()
        && value.bytes().all(|b| b.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'));
    canonical.then(|| value.parse().ok()).flatten()
}

fn required_integer(event: &Event, name: &'static str) -> Result<u64, Rejection> {
    parse_integer(required_value(event, name)?).ok_or(Rejection::Malformed(name))
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Checks what both shapes share and returns the envelope.
fn verify_envelope(
    event: &Event,
    expected_author: Option<PublicKey>,
    z: &'static str,
    now: u64,
) -> Result<Envelope, Rejection> {
    if event.kind != HEALTH_KIND {
        return Err(Rejection::WrongKind);
    }
    if let Some(author) = expected_author {
        if event.pubkey != author {
            return Err(Rejection::WrongAuthor);
        }
    }
    if event.verify().is_err() {
        return Err(Rejection::BadSignature);
    }
    if required_value(event, "d")? != event.pubkey.to_hex() {
        return Err(Rejection::WrongAddress);
    }
    if required_value(event, "z")? != z {
        return Err(Rejection::WrongZ(z));
    }
    let created_at = event.created_at.as_secs();
    if created_at > now.saturating_add(MAX_FUTURE_SKEW_SECS) {
        return Err(Rejection::FromTheFuture);
    }
    let interval = required_integer(event, "interval")?;
    if !(MIN_INTERVAL..=MAX_INTERVAL).contains(&interval) {
        return Err(Rejection::Malformed("interval"));
    }
    let seq = required_integer(event, "seq")?;
    if !(1..=MAX_SEQ).contains(&seq) {
        return Err(Rejection::Malformed("seq"));
    }
    let expiration = required_integer(event, "expiration")?;
    if Some(expiration) != created_at.checked_add(3 * interval) {
        return Err(Rejection::WrongExpiration);
    }
    Ok(Envelope {
        id: event.id,
        pubkey: event.pubkey,
        created_at,
        interval,
        seq,
        expiration,
    })
}

/// The `check` tags. Unknown names are skipped; any name twice, or a
/// malformed tag, rejects the event.
fn parse_checks(event: &Event, created_at: u64) -> Result<Vec<Check>, Rejection> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut checks = Vec::new();
    for values in event.tags.iter().map(|tag| tag.as_slice()) {
        let [kind, name, state, since, ..] = values else {
            if values.first().map(String::as_str) == Some("check") {
                return Err(Rejection::Malformed("check"));
            }
            continue;
        };
        if kind != "check" {
            continue;
        }
        let since = parse_integer(since).ok_or(Rejection::Malformed("check"))?;
        if since > created_at || !seen.insert(name.as_str()) {
            return Err(Rejection::Malformed("check"));
        }
        let Some(name) = CheckName::parse(name) else {
            continue;
        };
        checks.push(Check {
            name,
            state: CheckState::parse(state),
            since,
        });
    }
    if checks.is_empty() {
        return Err(Rejection::Malformed("check"));
    }
    Ok(checks)
}

/// Verifies an instance health event from `author` and reads its tags.
pub fn parse_health(event: &Event, author: PublicKey, now: u64) -> Result<HealthReport, Rejection> {
    parse_health_from(event, Some(author), now)
}

/// Like [`parse_health`], for an instance discovered by kind and `z`.
pub fn parse_any_health(event: &Event, now: u64) -> Result<HealthReport, Rejection> {
    parse_health_from(event, None, now)
}

fn parse_health_from(
    event: &Event,
    author: Option<PublicKey>,
    now: u64,
) -> Result<HealthReport, Rejection> {
    let envelope = verify_envelope(event, author, Z_HEALTH, now)?;
    let y = single_tag(event, "y")?.ok_or(Rejection::Malformed("y"))?;
    if y.get(1).map(String::as_str) != Some("mostro") {
        return Err(Rejection::Malformed("y"));
    }
    let name = y.get(2).filter(|n| !n.is_empty()).cloned();
    let status =
        Status::parse(required_value(event, "status")?).ok_or(Rejection::Malformed("status"))?;
    let checks = parse_checks(event, envelope.created_at)?;
    let escrow_mode = match required_value(event, "escrow_mode")? {
        "lightning" => EscrowMode::Lightning,
        "cashu" => EscrowMode::Cashu,
        _ => return Err(Rejection::Malformed("escrow_mode")),
    };
    let lnd_block_height = single_tag(event, "lnd_block_height")?
        .map(|values| {
            values
                .get(1)
                .and_then(|v| parse_integer(v))
                .ok_or(Rejection::Malformed("lnd_block_height"))
        })
        .transpose()?;
    let lnd_synced_to_chain = single_tag(event, "lnd_synced_to_chain")?
        .map(|values| {
            values
                .get(1)
                .and_then(|v| parse_bool(v))
                .ok_or(Rejection::Malformed("lnd_synced_to_chain"))
        })
        .transpose()?;
    let lnd_network = single_tag(event, "lnd_network")?
        .map(|values| {
            values
                .get(1)
                .filter(|v| !v.is_empty())
                .cloned()
                .ok_or(Rejection::Malformed("lnd_network"))
        })
        .transpose()?;
    let maintenance_mode = parse_bool(required_value(event, "maintenance_mode")?)
        .ok_or(Rejection::Malformed("maintenance_mode"))?;
    let mostro_version = required_value(event, "mostro_version")?.to_string();
    let recipient = single_tag(event, "p")?
        .map(|values| {
            values
                .get(1)
                .and_then(|v| PublicKey::from_hex(v).ok())
                .ok_or(Rejection::Malformed("p"))
        })
        .transpose()?;
    // `p` is present if and only if there is content.
    if recipient.is_some() == event.content.is_empty() {
        return Err(Rejection::Malformed("p"));
    }
    Ok(HealthReport {
        envelope,
        name,
        status,
        checks,
        escrow_mode,
        lnd_block_height,
        lnd_synced_to_chain,
        lnd_network,
        maintenance_mode,
        mostro_version,
        recipient,
        content: event.content.clone(),
    })
}

/// Verifies a watcher heartbeat, from `author` when given.
pub fn parse_watcher(
    event: &Event,
    author: Option<PublicKey>,
    now: u64,
) -> Result<WatcherBeat, Rejection> {
    let envelope = verify_envelope(event, author, Z_WATCHER, now)?;
    let software = required_value(event, "y")?.to_string();
    let mut watching: Vec<PublicKey> = Vec::new();
    for values in event
        .tags
        .iter()
        .map(|tag| tag.as_slice())
        .filter(|values| values.first().map(String::as_str) == Some("watching"))
    {
        let pubkey = values
            .get(1)
            .and_then(|v| PublicKey::from_hex(v).ok())
            .ok_or(Rejection::Malformed("watching"))?;
        // One tag per instance: the same pubkey twice is a repeated tag.
        if watching.contains(&pubkey) {
            return Err(Rejection::Malformed("watching"));
        }
        watching.push(pubkey);
    }
    Ok(WatcherBeat {
        envelope,
        software,
        watching,
    })
}

/// The last revision accepted from one publisher. A new revision is
/// accepted only with a greater `seq`; the accepted one, seen again, is
/// still the current one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accepted {
    pub seq: u64,
    pub id: EventId,
}

/// Among `candidates` (already verified), the revision to use: the one with
/// the highest `seq`, provided it is newer than `last` or is `last` itself.
/// `None` when every candidate is older (a replay) or there are none.
pub fn newest<'a, T>(
    candidates: impl IntoIterator<Item = &'a T>,
    envelope: impl Fn(&T) -> &Envelope,
    last: Option<Accepted>,
) -> Option<&'a T>
where
    T: 'a,
{
    let best = candidates
        .into_iter()
        .max_by_key(|candidate| envelope(candidate).seq)?;
    let env = envelope(best);
    match last {
        Some(last) if env.seq < last.seq => None,
        Some(last) if env.seq == last.seq && env.id != last.id => None,
        _ => Some(best),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub const NOW: u64 = 1_792_000_000;

    /// The spec's example, with tags overridable by name.
    pub fn health_tags(mostro: &Keys, created_at: u64, seq: u64) -> Vec<Vec<String>> {
        let pk = mostro.public_key().to_hex();
        let tags: Vec<Vec<&str>> = vec![
            vec!["d", &pk],
            vec!["y", "mostro", "Test Mostro"],
            vec!["z", "health"],
            vec!["status", "ok"],
            vec!["check", "lnd_sync", "ok", "1791900000"],
            vec!["check", "payouts", "ok", "1791900000"],
            vec!["check", "relays", "ok", "1791900000"],
            vec!["check", "database", "ok", "1791900000"],
            vec!["escrow_mode", "lightning"],
            vec!["lnd_block_height", "971485"],
            vec!["lnd_synced_to_chain", "true"],
            vec!["lnd_network", "mainnet"],
            vec!["maintenance_mode", "false"],
            vec!["mostro_version", "0.19.3"],
            vec!["interval", "60"],
        ];
        let mut tags: Vec<Vec<String>> = tags
            .into_iter()
            .map(|t| t.into_iter().map(String::from).collect())
            .collect();
        tags.push(vec!["seq".into(), seq.to_string()]);
        tags.push(vec!["expiration".into(), (created_at + 180).to_string()]);
        tags
    }

    /// Sets tag `name` to `values` (or removes it when `values` is empty).
    pub fn set_tag(tags: &mut Vec<Vec<String>>, name: &str, values: &[&str]) {
        tags.retain(|t| t[0] != name);
        if !values.is_empty() {
            let mut tag = vec![name.to_string()];
            tag.extend(values.iter().map(|v| v.to_string()));
            tags.push(tag);
        }
    }

    pub fn sign(keys: &Keys, tags: Vec<Vec<String>>, content: &str, created_at: u64) -> Event {
        EventBuilder::new(HEALTH_KIND, content)
            .tags(tags.into_iter().map(|t| Tag::parse(t).unwrap()))
            .custom_created_at(Timestamp::from(created_at))
            .finalize(keys)
            .unwrap()
    }

    pub fn health_event(mostro: &Keys, created_at: u64, seq: u64) -> Event {
        sign(mostro, health_tags(mostro, created_at, seq), "", created_at)
    }

    fn with(tag: &str, values: &[&str]) -> (Keys, Event) {
        let mostro = Keys::generate();
        let mut tags = health_tags(&mostro, NOW, 7);
        set_tag(&mut tags, tag, values);
        let event = sign(&mostro, tags, "", NOW);
        (mostro, event)
    }

    #[test]
    fn the_spec_example_parses() {
        let mostro = Keys::generate();

        let report =
            parse_health(&health_event(&mostro, NOW, 7), mostro.public_key(), NOW).unwrap();

        assert_eq!(report.status, Status::Ok);
        assert_eq!(report.name.as_deref(), Some("Test Mostro"));
        assert_eq!(report.checks.len(), 4);
        assert_eq!(report.escrow_mode, EscrowMode::Lightning);
        assert_eq!(report.lnd_block_height, Some(971_485));
        assert_eq!(report.lnd_synced_to_chain, Some(true));
        assert_eq!(report.lnd_network.as_deref(), Some("mainnet"));
        assert!(!report.maintenance_mode);
        assert_eq!(report.envelope.interval, 60);
        assert_eq!(report.envelope.seq, 7);
        assert_eq!(report.envelope.expiration, NOW + 180);
        assert_eq!(report.recipient, None);
    }

    #[test]
    fn an_event_from_another_key_is_ignored() {
        let mostro = Keys::generate();
        let impostor = Keys::generate();
        // Well-formed for the impostor, but not the instance we watch.
        let event = health_event(&impostor, NOW, 7);

        assert_eq!(
            parse_health(&event, mostro.public_key(), NOW),
            Err(Rejection::WrongAuthor)
        );
    }

    #[test]
    fn a_tampered_event_fails_the_signature_check() {
        let mostro = Keys::generate();
        let event = health_event(&mostro, NOW, 7);
        let mut json: serde_json::Value = serde_json::from_str(&event.as_json()).unwrap();
        json["content"] = serde_json::Value::String("tampered".into());
        let tampered = Event::from_json(json.to_string()).unwrap();

        assert_eq!(
            parse_health(&tampered, mostro.public_key(), NOW),
            Err(Rejection::BadSignature)
        );
    }

    #[test]
    fn a_d_tag_other_than_the_author_is_rejected() {
        let (mostro, event) = with("d", &["health"]);

        assert_eq!(
            parse_health(&event, mostro.public_key(), NOW),
            Err(Rejection::WrongAddress)
        );
    }

    #[test]
    fn a_watcher_heartbeat_is_not_taken_for_an_instance() {
        let (mostro, event) = with("z", &["watcher"]);

        assert_eq!(
            parse_health(&event, mostro.public_key(), NOW),
            Err(Rejection::WrongZ("health"))
        );
    }

    #[test]
    fn an_event_more_than_five_minutes_in_the_future_is_ignored() {
        let mostro = Keys::generate();
        let future = health_event(&mostro, NOW + 301, 7);
        let skewed = health_event(&mostro, NOW + 300, 7);

        assert_eq!(
            parse_health(&future, mostro.public_key(), NOW),
            Err(Rejection::FromTheFuture)
        );
        assert!(parse_health(&skewed, mostro.public_key(), NOW).is_ok());
    }

    #[test]
    fn an_interval_outside_thirty_to_thirty_six_hundred_is_rejected() {
        for interval in ["29", "3601", "0"] {
            let mostro = Keys::generate();
            let mut tags = health_tags(&mostro, NOW, 7);
            set_tag(&mut tags, "interval", &[interval]);
            let expiration = (NOW + 3 * interval.parse::<u64>().unwrap()).to_string();
            set_tag(&mut tags, "expiration", &[&expiration]);
            let event = sign(&mostro, tags, "", NOW);

            assert_eq!(
                parse_health(&event, mostro.public_key(), NOW),
                Err(Rejection::Malformed("interval")),
                "{interval}"
            );
        }
    }

    #[test]
    fn an_expiration_other_than_three_intervals_is_rejected() {
        let (mostro, event) = with("expiration", &[&(NOW + 181).to_string()]);

        assert_eq!(
            parse_health(&event, mostro.public_key(), NOW),
            Err(Rejection::WrongExpiration)
        );
    }

    #[test]
    fn integers_with_a_sign_or_leading_zeros_are_malformed() {
        for seq in ["+7", "07", "-7", "7.0", "", "0"] {
            let (mostro, event) = with("seq", &[seq]);

            assert_eq!(
                parse_health(&event, mostro.public_key(), NOW),
                Err(Rejection::Malformed("seq")),
                "{seq:?}"
            );
        }
    }

    #[test]
    fn a_missing_required_tag_rejects_the_event() {
        for tag in [
            "status",
            "escrow_mode",
            "maintenance_mode",
            "mostro_version",
            "y",
        ] {
            let (mostro, event) = with(tag, &[]);

            assert!(
                parse_health(&event, mostro.public_key(), NOW).is_err(),
                "{tag}"
            );
        }
    }

    #[test]
    fn unknown_check_names_are_skipped_and_unknown_states_are_unknown() {
        let mostro = Keys::generate();
        let mut tags = health_tags(&mostro, NOW, 7);
        tags.retain(|t| t[0] != "check");
        tags.push(vec![
            "check".into(),
            "lnd_sync".into(),
            "bogus".into(),
            "1791900000".into(),
        ]);
        tags.push(vec![
            "check".into(),
            "quantum".into(),
            "fail".into(),
            "1791900000".into(),
        ]);
        let event = sign(&mostro, tags, "", NOW);

        let report = parse_health(&event, mostro.public_key(), NOW).unwrap();

        assert_eq!(report.checks.len(), 1);
        assert_eq!(
            report.check(CheckName::LndSync).unwrap().state,
            CheckState::Unknown
        );
    }

    #[test]
    fn a_check_reported_twice_or_dated_after_the_event_is_malformed() {
        let mostro = Keys::generate();
        let mut twice = health_tags(&mostro, NOW, 7);
        twice.push(vec![
            "check".into(),
            "relays".into(),
            "fail".into(),
            "1791900000".into(),
        ]);
        let mut late = health_tags(&mostro, NOW, 7);
        set_tag(
            &mut late,
            "check",
            &["relays", "ok", &(NOW + 1).to_string()],
        );

        for tags in [twice, late] {
            let event = sign(&mostro, tags, "", NOW);
            assert_eq!(
                parse_health(&event, mostro.public_key(), NOW),
                Err(Rejection::Malformed("check"))
            );
        }
    }

    #[test]
    fn any_repeated_tag_rejects_the_event() {
        let mostro = Keys::generate();
        let repeated = [
            "d",
            "y",
            "z",
            "status",
            "escrow_mode",
            "lnd_block_height",
            "lnd_synced_to_chain",
            "lnd_network",
            "maintenance_mode",
            "mostro_version",
            "interval",
            "seq",
            "expiration",
        ];
        for name in repeated {
            let mut tags = health_tags(&mostro, NOW, 7);
            let copy = tags.iter().find(|t| t[0] == name).unwrap().clone();
            // Same value twice: still ambiguous for readers that pick one.
            tags.push(copy);
            let event = sign(&mostro, tags, "", NOW);

            assert!(
                parse_health(&event, mostro.public_key(), NOW).is_err(),
                "repeated {name}"
            );
        }

        let watchdog = Keys::generate().public_key().to_hex();
        let mut tags = health_tags(&mostro, NOW, 7);
        tags.push(vec!["p".into(), watchdog.clone()]);
        tags.push(vec!["p".into(), watchdog]);
        let event = sign(&mostro, tags, "abc", NOW);
        assert_eq!(
            parse_health(&event, mostro.public_key(), NOW),
            Err(Rejection::Malformed("p"))
        );
    }

    #[test]
    fn an_unknown_check_name_repeated_also_rejects_the_event() {
        let mostro = Keys::generate();
        let mut tags = health_tags(&mostro, NOW, 7);
        for _ in 0..2 {
            tags.push(vec![
                "check".into(),
                "quantum".into(),
                "ok".into(),
                "1791900000".into(),
            ]);
        }

        assert_eq!(
            parse_health(&sign(&mostro, tags, "", NOW), mostro.public_key(), NOW),
            Err(Rejection::Malformed("check"))
        );
    }

    #[test]
    fn a_p_tag_without_content_or_content_without_p_is_malformed() {
        let mostro = Keys::generate();
        let watchdog = Keys::generate().public_key().to_hex();
        let mut tags = health_tags(&mostro, NOW, 7);
        set_tag(&mut tags, "p", &[&watchdog]);

        let p_without_content = sign(&mostro, tags.clone(), "", NOW);
        let content_without_p = sign(&mostro, health_tags(&mostro, NOW, 7), "abc", NOW);
        let both = sign(&mostro, tags, "abc", NOW);

        assert_eq!(
            parse_health(&p_without_content, mostro.public_key(), NOW),
            Err(Rejection::Malformed("p"))
        );
        assert_eq!(
            parse_health(&content_without_p, mostro.public_key(), NOW),
            Err(Rejection::Malformed("p"))
        );
        assert!(parse_health(&both, mostro.public_key(), NOW).is_ok());
    }

    #[test]
    fn freshness_ends_at_expiration_or_three_intervals() {
        let mostro = Keys::generate();
        let report =
            parse_health(&health_event(&mostro, NOW, 7), mostro.public_key(), NOW).unwrap();

        assert!(report.envelope.is_fresh(NOW));
        assert!(report.envelope.is_fresh(NOW + 179));
        assert!(!report.envelope.is_fresh(NOW + 180));
    }

    #[test]
    fn the_highest_seq_wins_and_older_revisions_are_replays() {
        let mostro = Keys::generate();
        let parse = |seq, at| {
            parse_health(&health_event(&mostro, at, seq), mostro.public_key(), NOW).unwrap()
        };
        let reports = [parse(5, NOW - 120), parse(7, NOW), parse(6, NOW - 60)];

        let best = newest(&reports, |r| &r.envelope, None).unwrap();
        assert_eq!(best.envelope.seq, 7);

        let accepted = Accepted {
            seq: 7,
            id: best.envelope.id,
        };
        // The accepted revision seen again is still the current one.
        assert_eq!(
            newest(&reports, |r| &r.envelope, Some(accepted)).map(|r| r.envelope.seq),
            Some(7)
        );
        // Only older revisions left: a relay serving a replay.
        assert!(newest(&reports[..1], |r| &r.envelope, Some(accepted)).is_none());
        // Same seq, different event: not greater, so ignored.
        let other = Accepted {
            seq: 7,
            id: reports[0].envelope.id,
        };
        assert!(newest(&reports[1..2], |r| &r.envelope, Some(other)).is_none());
    }

    #[test]
    fn a_watcher_heartbeat_parses_with_its_watching_tags() {
        let watcher = Keys::generate();
        let mostro = Keys::generate().public_key();
        let pk = watcher.public_key().to_hex();
        let tags = vec![
            vec!["d".to_string(), pk],
            vec!["y".into(), "mostro-watchdog".into()],
            vec!["z".into(), "watcher".into()],
            vec!["watching".into(), mostro.to_hex()],
            vec!["interval".into(), "60".into()],
            vec!["seq".into(), "88213".into()],
            vec!["expiration".into(), (NOW + 180).to_string()],
        ];
        let event = sign(&watcher, tags, "", NOW);

        let beat = parse_watcher(&event, Some(watcher.public_key()), NOW).unwrap();

        assert_eq!(beat.software, "mostro-watchdog");
        assert_eq!(beat.watching, vec![mostro]);
        assert_eq!(beat.envelope.seq, 88213);
        assert_eq!(
            parse_health(&event, watcher.public_key(), NOW),
            Err(Rejection::WrongZ("health"))
        );
    }

    fn watcher_tags(watcher: &Keys) -> Vec<Vec<String>> {
        vec![
            vec!["d".to_string(), watcher.public_key().to_hex()],
            vec!["y".into(), "mostro-watchdog".into()],
            vec!["z".into(), "watcher".into()],
            vec!["interval".into(), "60".into()],
            vec!["seq".into(), "5".into()],
            vec!["expiration".into(), (NOW + 180).to_string()],
        ]
    }

    #[test]
    fn a_heartbeat_needs_no_instance_tags_and_may_watch_several_instances() {
        let watcher = Keys::generate();
        let mut tags = watcher_tags(&watcher);
        let (a, b) = (Keys::generate().public_key(), Keys::generate().public_key());
        tags.push(vec!["watching".into(), a.to_hex()]);
        tags.push(vec!["watching".into(), b.to_hex()]);

        let beat = parse_watcher(&sign(&watcher, tags, "", NOW), None, NOW).unwrap();

        assert_eq!(beat.watching, vec![a, b]);
    }

    #[test]
    fn a_heartbeat_repeating_a_tag_is_rejected() {
        let watcher = Keys::generate();
        for name in ["d", "y", "z", "interval", "seq", "expiration"] {
            let mut tags = watcher_tags(&watcher);
            let copy = tags.iter().find(|t| t[0] == name).unwrap().clone();
            tags.push(copy);

            assert!(
                parse_watcher(&sign(&watcher, tags, "", NOW), None, NOW).is_err(),
                "repeated {name}"
            );
        }
    }

    #[test]
    fn a_heartbeat_watching_the_same_instance_twice_or_a_bad_key_is_rejected() {
        let watcher = Keys::generate();
        let mostro = Keys::generate().public_key().to_hex();
        let mut twice = watcher_tags(&watcher);
        twice.push(vec!["watching".into(), mostro.clone()]);
        twice.push(vec!["watching".into(), mostro]);
        let mut garbage = watcher_tags(&watcher);
        garbage.push(vec!["watching".into(), "not-a-key".into()]);

        for tags in [twice, garbage] {
            assert_eq!(
                parse_watcher(&sign(&watcher, tags, "", NOW), None, NOW),
                Err(Rejection::Malformed("watching"))
            );
        }
    }
}
