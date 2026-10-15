//! The `[health]` keys of Mostro health monitoring: info event freshness
//! (phase 0), the health event (phase 1, kind 38389), the watchdog's own
//! heartbeat and peer watchdogs. They are flattened into `[health]`, next
//! to the older keys of the watchdog's own process monitoring.

use std::collections::HashMap;

use nostr_sdk::prelude::PublicKey;
use serde::Deserialize;

use crate::config::DEFAULT_PRIVATE_KEY_ENV;

/// Shortest and longest `check_interval`: the range the health event spec
/// allows for the `interval` tag, which the watchdog's heartbeat carries.
pub const MIN_CHECK_INTERVAL_SECS: u64 = 30;
pub const MAX_CHECK_INTERVAL_SECS: u64 = 3600;

/// Default `info_silence_threshold`: three times an info interval of one
/// hour, a common mostrod setting. With mostrod's default of 300 s, 900 is
/// the tighter choice.
pub const DEFAULT_INFO_SILENCE_THRESHOLD_SECS: u64 = 10_800;

/// Explorers asked for the chain tip, per `lnd_network`, when
/// `block_height_sources` is unset. A network with no entry (regtest,
/// simnet) has no independent reference, so its cross-check is skipped.
pub const DEFAULT_BLOCK_HEIGHT_SOURCES: [(&str, &[&str]); 4] = [
    (
        "mainnet",
        &[
            "https://mempool.space/api/blocks/tip/height",
            "https://blockstream.info/api/blocks/tip/height",
        ],
    ),
    (
        "testnet",
        &[
            "https://mempool.space/testnet/api/blocks/tip/height",
            "https://blockstream.info/testnet/api/blocks/tip/height",
        ],
    ),
    (
        "testnet4",
        &["https://mempool.space/testnet4/api/blocks/tip/height"],
    ),
    (
        "signet",
        &["https://mempool.space/signet/api/blocks/tip/height"],
    ),
];

fn default_block_height_sources() -> HashMap<String, Vec<String>> {
    DEFAULT_BLOCK_HEIGHT_SOURCES
        .iter()
        .map(|(network, urls)| {
            (
                network.to_string(),
                urls.iter().map(|u| u.to_string()).collect(),
            )
        })
        .collect()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct MonitorConfig {
    /// Phase 0: alert when Mostro's info event (kind 38385) is older than
    /// this many seconds. Set it to three times the instance's
    /// `publish_mostro_info_interval`: the event doesn't say its interval.
    /// 0 turns the check off.
    pub info_silence_threshold: u64,
    /// Phase 0: alert when Mostro enters maintenance mode, and say when it
    /// leaves it.
    pub alert_on_maintenance: bool,
    /// Phase 1: read Mostro's health event (kind 38389). Off by default
    /// until the operator runs a mostrod that publishes it: with no event
    /// at all, Mostro would be reported as silent.
    pub mostro_health_enabled: bool,
    /// Seconds between two evaluations, and the `interval` of the
    /// watchdog's own heartbeat.
    pub check_interval: u64,
    /// Bad readings in a row before an alert fires, and good readings in a
    /// row before it is resolved.
    pub consecutive_samples: u32,
    /// Seconds between reminders of a critical alert that is still active.
    /// 0 never reminds. Warnings never remind.
    pub renotify_interval: u64,
    /// Explorer endpoints that answer the chain tip height as plain text,
    /// per `lnd_network`.
    pub block_height_sources: HashMap<String, Vec<String>>,
    /// Other Mostro instances (npub or hex) whose block height the watchdog
    /// trusts: the median of their fresh health events is a reference, when
    /// at least three are fresh. Empty: explorers only. Never every
    /// discovered instance, since anyone can publish a validly signed event
    /// with a made-up height.
    pub trusted_peers: Vec<String>,
    /// Publish the watchdog's own heartbeat (kind 38389, `z` = `watcher`).
    /// It is also the relay round-trip probe. Needs the watchdog's key.
    pub publish_heartbeat: bool,
    /// Put a `watching` tag with Mostro's pubkey in the heartbeat. It links
    /// the watchdog to the instance in public.
    pub heartbeat_watching_tag: bool,
    /// Other watchdogs (npub or hex) whose heartbeat this one watches.
    pub peer_watchdogs: Vec<String>,
    /// Telegram chat for health alerts. Defaults to `[telegram].chat_id`.
    pub alerts_chat_id: Option<i64>,
    /// Environment variable with the watchdog's own Nostr secret key: it
    /// decrypts the health event's content and signs the heartbeat. When the
    /// variable is unset, only the public tags are read and no heartbeat is
    /// published.
    pub private_key_env: String,
    /// Environment variable with a key that only decrypts the health
    /// event's details. Two watchdogs may share it, while each signs its
    /// heartbeat with its own `private_key_env` key. When the variable is
    /// unset, the `private_key_env` key decrypts.
    pub decrypt_key_env: String,
    pub thresholds: Thresholds,
}

/// Default `decrypt_key_env`.
pub const DEFAULT_DECRYPT_KEY_ENV: &str = "WATCHDOG_HEALTH_DECRYPT_KEY";

impl Default for MonitorConfig {
    fn default() -> Self {
        Self {
            info_silence_threshold: DEFAULT_INFO_SILENCE_THRESHOLD_SECS,
            alert_on_maintenance: true,
            mostro_health_enabled: false,
            check_interval: 60,
            consecutive_samples: 3,
            renotify_interval: 3600,
            block_height_sources: default_block_height_sources(),
            trusted_peers: Vec::new(),
            publish_heartbeat: true,
            heartbeat_watching_tag: true,
            peer_watchdogs: Vec::new(),
            alerts_chat_id: None,
            private_key_env: DEFAULT_PRIVATE_KEY_ENV.to_string(),
            decrypt_key_env: DEFAULT_DECRYPT_KEY_ENV.to_string(),
            thresholds: Thresholds::default(),
        }
    }
}

/// When a reading counts as bad. Defaults follow the table in
/// mostro-watchdog#46.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Thresholds {
    /// Blocks behind the reference height that count as lagging.
    pub block_lag_blocks: u64,
    /// Seconds the lag must last.
    pub block_lag_for_secs: u64,
    /// Seconds `lnd_synced_to_chain` must stay false.
    pub unsynced_for_secs: u64,
    /// No successful payout for this long while an order has been waiting
    /// for its payout for as long.
    pub no_payout_success_secs: u64,
    /// Share of failed payout attempts, in the event's window, that counts
    /// as failing (more than this).
    pub payout_failure_rate: f64,
    /// Fewest attempts in the window before the failure rate counts.
    pub payout_failure_min_attempts: u64,
    /// Blocks before the nearest pending HTLC expires.
    pub htlc_expiry_blocks: i64,
    /// Age of the oldest settled-but-unpaid order that warns.
    pub settled_unpaid_warn_secs: u64,
    /// Seconds a channel must stay inactive before it warns.
    pub inactive_channels_warn_secs: u64,
    /// Seconds the daemon must report `degraded` before it warns.
    pub degraded_for_secs: u64,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            block_lag_blocks: 3,
            block_lag_for_secs: 900,
            unsynced_for_secs: 600,
            no_payout_success_secs: 7200,
            payout_failure_rate: 0.5,
            payout_failure_min_attempts: 3,
            htlc_expiry_blocks: 72,
            settled_unpaid_warn_secs: 900,
            inactive_channels_warn_secs: 1800,
            degraded_for_secs: 600,
        }
    }
}

impl MonitorConfig {
    /// Whether anything about the Mostro instance is watched at all.
    pub fn is_active(&self) -> bool {
        self.info_silence_threshold > 0
            || self.alert_on_maintenance
            || self.mostro_health_enabled
            || !self.peer_watchdogs.is_empty()
    }

    /// The configured peer watchdogs, parsed.
    pub fn peer_watchdogs(&self) -> Result<Vec<PublicKey>, String> {
        parse_keys("peer_watchdogs", &self.peer_watchdogs)
    }

    /// The instances trusted for the block-height median, parsed.
    pub fn trusted_peers(&self) -> Result<Vec<PublicKey>, String> {
        parse_keys("trusted_peers", &self.trusted_peers)
    }

    /// Checks the values that can be wrong on their own.
    pub fn validate(&self) -> Result<(), String> {
        if !(MIN_CHECK_INTERVAL_SECS..=MAX_CHECK_INTERVAL_SECS).contains(&self.check_interval) {
            return Err(format!(
                "health.check_interval must be between {MIN_CHECK_INTERVAL_SECS} and \
                 {MAX_CHECK_INTERVAL_SECS} seconds"
            ));
        }
        if self.consecutive_samples == 0 {
            return Err("health.consecutive_samples must be at least 1".into());
        }
        let rate = self.thresholds.payout_failure_rate;
        if !(rate > 0.0 && rate < 1.0) {
            return Err("health.thresholds.payout_failure_rate must be between 0 and 1".into());
        }
        if self.thresholds.block_lag_blocks == 0 {
            return Err("health.thresholds.block_lag_blocks must be at least 1".into());
        }
        if self.thresholds.payout_failure_min_attempts == 0 {
            return Err("health.thresholds.payout_failure_min_attempts must be at least 1".into());
        }
        if let Some(source) = self
            .block_height_sources
            .values()
            .flatten()
            .find(|url| !(url.starts_with("https://") || url.starts_with("http://")))
        {
            return Err(format!(
                "health.block_height_sources: {source:?} is not an http(s) URL"
            ));
        }
        if self.private_key_env.trim().is_empty() {
            return Err("health.private_key_env cannot be empty".into());
        }
        if self.decrypt_key_env.trim().is_empty() {
            return Err("health.decrypt_key_env cannot be empty".into());
        }
        self.trusted_peers()?;
        self.peer_watchdogs().map(|_| ())
    }
}

fn parse_keys(field: &str, keys: &[String]) -> Result<Vec<PublicKey>, String> {
    keys.iter()
        .map(|key| {
            PublicKey::parse(key.trim())
                .map_err(|_| format!("health.{field}: {key:?} is not a valid pubkey"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml: &str) -> MonitorConfig {
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn phase_zero_is_on_and_phase_one_is_opt_in_by_default() {
        let config = MonitorConfig::default();

        assert_eq!(config.info_silence_threshold, 10_800);
        assert!(config.alert_on_maintenance);
        assert!(!config.mostro_health_enabled);
        assert_eq!(config.check_interval, 60);
        assert_eq!(config.consecutive_samples, 3);
        assert_eq!(config.renotify_interval, 3600);
        assert_eq!(config.block_height_sources["mainnet"].len(), 2);
        assert!(!config.block_height_sources.contains_key("regtest"));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn thresholds_default_to_the_issue_table() {
        let t = Thresholds::default();

        assert_eq!(t.block_lag_blocks, 3);
        assert_eq!(t.block_lag_for_secs, 900);
        assert_eq!(t.unsynced_for_secs, 600);
        assert_eq!(t.no_payout_success_secs, 7200);
        assert_eq!(t.payout_failure_rate, 0.5);
        assert_eq!(t.payout_failure_min_attempts, 3);
        assert_eq!(t.htlc_expiry_blocks, 72);
        assert_eq!(t.settled_unpaid_warn_secs, 900);
        assert_eq!(t.inactive_channels_warn_secs, 1800);
        assert_eq!(t.degraded_for_secs, 600);
    }

    #[test]
    fn a_partial_thresholds_table_keeps_the_other_defaults() {
        let config = parse("mostro_health_enabled = true\n[thresholds]\nblock_lag_blocks = 6\n");

        assert!(config.mostro_health_enabled);
        assert_eq!(config.thresholds.block_lag_blocks, 6);
        assert_eq!(config.thresholds.unsynced_for_secs, 600);
    }

    #[test]
    fn a_check_interval_outside_the_spec_range_is_rejected() {
        for interval in [0, 29, 3601] {
            let config = MonitorConfig {
                check_interval: interval,
                ..MonitorConfig::default()
            };
            assert!(config.validate().is_err(), "{interval}");
        }
        for interval in [30, 3600] {
            let config = MonitorConfig {
                check_interval: interval,
                ..MonitorConfig::default()
            };
            assert!(config.validate().is_ok(), "{interval}");
        }
    }

    #[test]
    fn block_height_sources_are_configured_per_network() {
        let config =
            parse("[block_height_sources]\nsignet = [\"https://example.org/signet/tip\"]\n");

        assert_eq!(
            config.block_height_sources["signet"],
            vec!["https://example.org/signet/tip".to_string()]
        );
        // A table given replaces the defaults: only what is listed is asked.
        assert!(!config.block_height_sources.contains_key("mainnet"));
    }

    #[test]
    fn zero_consecutive_samples_is_rejected() {
        let config = MonitorConfig {
            consecutive_samples: 0,
            ..MonitorConfig::default()
        };

        assert!(config
            .validate()
            .unwrap_err()
            .contains("consecutive_samples"));
    }

    #[test]
    fn a_failure_rate_outside_zero_and_one_is_rejected() {
        for rate in [0.0, 1.0, 1.5, -0.1] {
            let mut config = MonitorConfig::default();
            config.thresholds.payout_failure_rate = rate;
            assert!(config.validate().is_err(), "{rate}");
        }
    }

    #[test]
    fn a_block_height_source_that_is_not_http_is_rejected() {
        let config = MonitorConfig {
            block_height_sources: HashMap::from([(
                "mainnet".to_string(),
                vec!["mempool.space/api/blocks/tip/height".to_string()],
            )]),
            ..MonitorConfig::default()
        };

        assert!(config
            .validate()
            .unwrap_err()
            .contains("block_height_sources"));
    }

    #[test]
    fn peer_watchdogs_accept_npub_and_hex_and_reject_garbage() {
        let keys = nostr_sdk::prelude::Keys::generate();
        let npub = nostr_sdk::prelude::ToBech32::to_bech32(&keys.public_key()).unwrap();
        let config = MonitorConfig {
            peer_watchdogs: vec![npub, keys.public_key().to_hex()],
            ..MonitorConfig::default()
        };

        assert_eq!(
            config.peer_watchdogs().unwrap(),
            vec![keys.public_key(), keys.public_key()]
        );

        let bad = MonitorConfig {
            peer_watchdogs: vec!["not-a-key".into()],
            ..MonitorConfig::default()
        };
        assert!(bad.validate().unwrap_err().contains("peer_watchdogs"));
    }

    #[test]
    fn the_peer_median_is_off_unless_peers_are_trusted_explicitly() {
        let keys = nostr_sdk::prelude::Keys::generate();

        assert!(MonitorConfig::default().trusted_peers().unwrap().is_empty());
        let config = parse(&format!(
            "trusted_peers = [\"{}\"]\n",
            keys.public_key().to_hex()
        ));
        assert_eq!(config.trusted_peers().unwrap(), vec![keys.public_key()]);
        let bad = parse("trusted_peers = [\"nope\"]\n");
        assert!(bad.validate().unwrap_err().contains("trusted_peers"));
    }

    #[test]
    fn nothing_is_watched_when_every_check_is_off() {
        let config = MonitorConfig {
            info_silence_threshold: 0,
            alert_on_maintenance: false,
            ..MonitorConfig::default()
        };

        assert!(!config.is_active());
        assert!(MonitorConfig::default().is_active());
    }
}
