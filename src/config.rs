use nostr_sdk::prelude::{Keys, PublicKey};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub mostro: MostroConfig,
    pub nostr: NostrConfig,
    pub telegram: TelegramConfig,
    pub alerts: Option<AlertsConfig>,
    pub health: Option<HealthConfig>,
    /// Serbero alerts; when absent they are off.
    pub serbero: Option<SerberoConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlertsConfig {
    /// Enable alerts for new disputes (status: initiated)
    #[serde(default = "default_true")]
    pub initiated: bool,
    /// Enable alerts when dispute is taken (status: in-progress)
    #[serde(default = "default_true")]
    pub in_progress: bool,
    /// Enable alerts when dispute is resolved with seller refund
    #[serde(default = "default_true")]
    pub seller_refunded: bool,
    /// Enable alerts when dispute is settled (payment to buyer)
    #[serde(default = "default_true")]
    pub settled: bool,
    /// Enable alerts when dispute is released
    #[serde(default = "default_true")]
    pub released: bool,
    /// Enable alerts for unknown/other status changes
    #[serde(default = "default_true")]
    pub other: bool,
    /// Send a new message when Serbero hands a dispute off to a human solver
    /// or cannot start mediating it
    #[serde(default = "default_true")]
    pub serbero_handoff: bool,
    /// Show Serbero's progress on the dispute's message (edits, no
    /// notification)
    #[serde(default = "default_true")]
    pub serbero_progress: bool,
}

fn default_true() -> bool {
    true
}

impl Default for AlertsConfig {
    fn default() -> Self {
        Self {
            initiated: true,
            in_progress: true,
            seller_refunded: true,
            settled: true,
            released: true,
            other: true,
            serbero_handoff: true,
            serbero_progress: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct HealthConfig {
    /// Enable periodic heartbeat notifications (disabled by default to avoid flooding the chat)
    #[serde(default = "default_false")]
    pub heartbeat_enabled: bool,
    /// Heartbeat interval in seconds (default: 3600 = 1 hour)
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval: u64,
    /// Check relay connections periodically
    #[serde(default = "default_true")]
    pub check_relays: bool,
    /// Relay connection timeout in seconds (default: 30)
    #[serde(default = "default_connection_timeout")]
    pub relay_timeout: u64,
    /// Alert if no events received for this many seconds (default: 7200 = 2 hours)
    #[serde(default = "default_event_alert_threshold")]
    pub event_alert_threshold: u64,
    /// Enable optional health status endpoint
    #[serde(default = "default_false")]
    pub enable_http_endpoint: bool,
    /// HTTP endpoint port (default: 8080)
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    /// HTTP endpoint bind address (default: 127.0.0.1)
    /// Set to "0.0.0.0" for Docker or external access
    #[serde(default = "default_http_bind")]
    pub http_bind: String,
}

fn default_false() -> bool {
    false
}

fn default_heartbeat_interval() -> u64 {
    3600 // 1 hour
}

fn default_connection_timeout() -> u64 {
    30 // 30 seconds
}

fn default_event_alert_threshold() -> u64 {
    7200 // 2 hours
}

fn default_http_port() -> u16 {
    8080
}

fn default_http_bind() -> String {
    "127.0.0.1".to_string()
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            heartbeat_enabled: false,
            heartbeat_interval: default_heartbeat_interval(),
            check_relays: true,
            relay_timeout: default_connection_timeout(),
            event_alert_threshold: default_event_alert_threshold(),
            enable_http_endpoint: false,
            http_port: default_http_port(),
            http_bind: default_http_bind(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct MostroConfig {
    /// Mostro daemon's Nostr public key (hex or npub format)
    pub pubkey: String,
}

#[derive(Debug, Deserialize)]
pub struct NostrConfig {
    /// List of Nostr relay URLs used as bootstrap to discover Mostro's NIP-65 relay list
    pub relays: Vec<String>,
    /// How often (in seconds) to re-fetch the NIP-65 relay list (default: 7200 = 2 hours)
    #[serde(default = "default_nip65_refresh_interval")]
    pub nip65_refresh_interval: u64,
}

fn default_nip65_refresh_interval() -> u64 {
    7200 // 2 hours
}

/// Environment variable that holds the watchdog's own Nostr secret key when
/// `serbero.private_key_env` is not set.
pub const DEFAULT_PRIVATE_KEY_ENV: &str = "WATCHDOG_NOSTR_PRIVATE_KEY";

fn default_private_key_env() -> String {
    DEFAULT_PRIVATE_KEY_ENV.to_string()
}

/// The `[serbero]` section: relay what Serbero, Mostro's dispute assistant,
/// reports about each dispute. Serbero writes to the watchdog's own Nostr
/// key once that key is listed in Serbero's `[[observers]]`.
#[derive(Debug, Clone, Deserialize)]
// A misspelled key, or the secret itself pasted in (`private_key = ...`),
// must fail instead of being ignored.
#[serde(deny_unknown_fields)]
pub struct SerberoConfig {
    /// Name of the environment variable that holds the watchdog's Nostr
    /// secret key (nsec or hex). The key itself never goes in this file.
    #[serde(default = "default_private_key_env")]
    pub private_key_env: String,
    /// Serbero's public key (npub or hex). When omitted, it is read from the
    /// Mostro node's info event (kind 38385, `serbero` tag).
    pub pubkey: Option<String>,
}

/// Why the `[serbero]` section cannot be used. The messages name the
/// environment variable, never its value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SerberoConfigError {
    #[error("serbero.private_key_env cannot be empty")]
    EmptyKeyVariable,
    #[error(
        "serbero.private_key_env must name an environment variable, but it holds a \
         Nostr secret key; move the key into the environment"
    )]
    KeyAsVariable,
    #[error(
        "[serbero] is configured, but the environment variable {0} with the watchdog's \
         Nostr secret key (nsec or hex) is not set"
    )]
    MissingKey(String),
    #[error(
        "the environment variable {0} does not hold a valid Nostr secret key (expected nsec or hex)"
    )]
    InvalidKey(String),
    #[error("serbero.pubkey is not a valid Nostr public key (expected npub or hex)")]
    InvalidPubkey,
}

/// The `[serbero]` section resolved at startup.
#[derive(Debug, Clone)]
pub struct SerberoSettings {
    /// The watchdog's own keys: Serbero encrypts its messages to them.
    pub keys: Keys,
    /// Serbero's key when configured; otherwise it is discovered.
    pub pubkey: Option<PublicKey>,
}

impl SerberoConfig {
    /// Checks what can be checked without the environment.
    pub fn validate(&self) -> Result<(), SerberoConfigError> {
        let variable = self.private_key_env.trim();
        if variable.is_empty() {
            return Err(SerberoConfigError::EmptyKeyVariable);
        }
        // A key here would be printed as the name of a missing variable.
        if Keys::parse(variable).is_ok() {
            return Err(SerberoConfigError::KeyAsVariable);
        }
        self.pubkey().map(|_| ())
    }

    /// Serbero's configured key, if any.
    pub fn pubkey(&self) -> Result<Option<PublicKey>, SerberoConfigError> {
        self.pubkey
            .as_deref()
            .map(|key| PublicKey::parse(key.trim()).map_err(|_| SerberoConfigError::InvalidPubkey))
            .transpose()
    }

    /// Reads the watchdog's secret key from the environment through `env`
    /// (a lookup by variable name) and parses the configured keys.
    pub fn resolve(
        &self,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<SerberoSettings, SerberoConfigError> {
        self.validate()?;
        let variable = self.private_key_env.trim();
        let secret =
            env(variable).ok_or_else(|| SerberoConfigError::MissingKey(variable.into()))?;
        // The parse error is dropped on purpose: it could quote the value.
        let keys = Keys::parse(secret.trim())
            .map_err(|_| SerberoConfigError::InvalidKey(variable.into()))?;
        Ok(SerberoSettings {
            keys,
            pubkey: self.pubkey()?,
        })
    }
}

#[derive(Debug, Deserialize)]
pub struct TelegramConfig {
    /// Telegram bot token from @BotFather
    pub bot_token: String,
    /// Telegram chat ID where alerts will be sent (group or channel)
    pub chat_id: i64,
}

/// A TOML error as a sentence with its line. Never the `toml` error itself:
/// its `Debug` output, which `main` prints, carries the whole file, bot token
/// included.
fn toml_error(content: &str, error: &toml::de::Error) -> String {
    let line = error
        .span()
        .and_then(|span| content.get(..span.start))
        .map(|before| before.matches('\n').count() + 1);
    match line {
        Some(line) => format!("invalid config at line {line}: {}", error.message()),
        None => format!("invalid config: {}", error.message()),
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        if !path.exists() {
            let mut msg = format!(
                "Config file not found: {}\n\n\
                 Searched in:\n\
                 \x20   1. ./config.toml (current directory)\n\
                 \x20   2. ~/.config/mostro-watchdog/config.toml\n\n\
                 To fix this, either:\n\
                 \x20   • Run from the directory containing config.toml\n\
                 \x20   • Specify the path: mostro-watchdog --config /path/to/config.toml\n\
                 \x20   • Copy config to: ~/.config/mostro-watchdog/config.toml\n\n\
                 See config.example.toml for reference.",
                path.display()
            );

            // Extra hint if HOME config dir doesn't exist
            if let Some(home) = std::env::var_os("HOME") {
                let xdg_dir = std::path::PathBuf::from(home).join(".config/mostro-watchdog");
                if !xdg_dir.exists() {
                    msg.push_str(&format!(
                        "\n\nHint: mkdir -p {} && cp config.example.toml {}/config.toml",
                        xdg_dir.display(),
                        xdg_dir.display()
                    ));
                }
            }

            return Err(msg.into());
        }

        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content).map_err(|e| toml_error(&content, &e))?;

        // Validate
        if config.nostr.relays.is_empty() {
            return Err("At least one Nostr relay must be configured".into());
        }

        if config.telegram.bot_token.is_empty() {
            return Err("Telegram bot_token cannot be empty".into());
        }

        if config.mostro.pubkey.is_empty() {
            return Err("Mostro pubkey cannot be empty".into());
        }

        if config.nostr.nip65_refresh_interval == 0 {
            return Err("nip65_refresh_interval must be greater than 0".into());
        }

        if let Some(ref serbero) = config.serbero {
            // As a string, like the errors above: `main` prints it with `Debug`.
            serbero.validate().map_err(|e| e.to_string())?;
        }

        if let Some(ref health) = config.health {
            if health.heartbeat_enabled && health.heartbeat_interval == 0 {
                return Err("heartbeat_interval must be greater than 0".into());
            }
            if health.check_relays && health.relay_timeout == 0 {
                return Err(
                    "relay_timeout must be greater than 0 when check_relays is enabled".into(),
                );
            }
        }

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::ToBech32;
    use std::io::Write;

    const BASE: &str = r#"
[mostro]
pubkey = "npub1..."

[nostr]
relays = ["wss://relay.mostro.network"]

[telegram]
bot_token = "123:abc"
chat_id = -1001
"#;

    fn load(extra: &str) -> Result<Config, Box<dyn std::error::Error>> {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        write!(file, "{BASE}{extra}").unwrap();
        Config::load(file.path())
    }

    fn serbero(private_key_env: &str, pubkey: Option<&str>) -> SerberoConfig {
        SerberoConfig {
            private_key_env: private_key_env.into(),
            pubkey: pubkey.map(Into::into),
        }
    }

    #[test]
    fn the_serbero_section_is_optional() {
        let config = load("").unwrap();

        assert!(config.serbero.is_none());
    }

    #[test]
    fn an_empty_serbero_section_reads_the_default_key_variable() {
        let config = load("\n[serbero]\n").unwrap();

        let serbero = config.serbero.expect("section present");
        assert_eq!(serbero.private_key_env, DEFAULT_PRIVATE_KEY_ENV);
        assert_eq!(serbero.pubkey, None);
    }

    #[test]
    fn serbero_alert_toggles_default_to_true() {
        let config = load("\n[alerts]\ninitiated = false\n").unwrap();

        let alerts = config.alerts.expect("section present");
        assert!(alerts.serbero_handoff);
        assert!(alerts.serbero_progress);
        assert!(AlertsConfig::default().serbero_handoff);
        assert!(AlertsConfig::default().serbero_progress);
    }

    #[test]
    fn serbero_alert_toggles_can_be_turned_off() {
        let config =
            load("\n[alerts]\nserbero_handoff = false\nserbero_progress = false\n").unwrap();

        let alerts = config.alerts.expect("section present");
        assert!(!alerts.serbero_handoff);
        assert!(!alerts.serbero_progress);
    }

    #[test]
    fn the_watchdog_key_is_read_from_the_named_variable_as_nsec_or_hex() {
        let keys = Keys::generate();
        let nsec = keys.secret_key().to_bech32().unwrap();
        let hex = keys.secret_key().to_secret_hex();

        for secret in [
            nsec,
            hex,
            format!("  {}\n", keys.secret_key().to_secret_hex()),
        ] {
            let settings = serbero("MY_KEY", None)
                .resolve(|name| (name == "MY_KEY").then(|| secret.clone()))
                .unwrap();

            assert_eq!(settings.keys.public_key(), keys.public_key());
            assert_eq!(settings.pubkey, None);
        }
    }

    #[test]
    fn a_missing_key_variable_is_an_error_that_names_it() {
        let result = serbero("MY_KEY", None).resolve(|_| None);

        let err = result.unwrap_err();
        assert_eq!(err, SerberoConfigError::MissingKey("MY_KEY".into()));
        assert!(err.to_string().contains("MY_KEY"));
    }

    #[test]
    fn an_invalid_key_is_an_error_that_does_not_echo_the_value() {
        let result = serbero("MY_KEY", None).resolve(|_| Some("nsec1notakey".into()));

        let err = result.unwrap_err();
        assert_eq!(err, SerberoConfigError::InvalidKey("MY_KEY".into()));
        assert!(!err.to_string().contains("nsec1notakey"));
    }

    #[test]
    fn an_empty_key_is_an_error() {
        let result = serbero("MY_KEY", None).resolve(|_| Some("   ".into()));

        assert_eq!(
            result.unwrap_err(),
            SerberoConfigError::InvalidKey("MY_KEY".into())
        );
    }

    #[test]
    fn the_serbero_pubkey_accepts_npub_and_hex() {
        let serbero_key = Keys::generate().public_key();
        let watchdog = Keys::generate();
        let secret = watchdog.secret_key().to_secret_hex();

        for configured in [serbero_key.to_bech32().unwrap(), serbero_key.to_hex()] {
            let settings = serbero("MY_KEY", Some(&configured))
                .resolve(|_| Some(secret.clone()))
                .unwrap();

            assert_eq!(settings.pubkey, Some(serbero_key));
        }
    }

    #[test]
    fn an_invalid_serbero_pubkey_fails_at_load() {
        let err = load("\n[serbero]\npubkey = \"npub1nope\"\n").unwrap_err();

        assert!(err.to_string().contains("serbero.pubkey"), "{err}");
        assert_eq!(
            serbero("MY_KEY", Some("")).validate(),
            Err(SerberoConfigError::InvalidPubkey)
        );
    }

    #[test]
    fn a_serbero_error_at_load_prints_as_a_sentence() {
        // `main` prints the error it returns with `Debug`.
        let err = load("\n[serbero]\npubkey = \"npub1nope\"\n").unwrap_err();

        assert!(
            format!("{err:?}").contains("serbero.pubkey is not a valid Nostr public key"),
            "{err:?}"
        );
    }

    #[test]
    fn an_empty_key_variable_name_fails_at_load() {
        let err = load("\n[serbero]\nprivate_key_env = \"\"\n").unwrap_err();

        assert!(err.to_string().contains("private_key_env"), "{err}");
    }

    #[test]
    fn the_example_config_loads_with_serbero_alerts_off() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("config.example.toml");

        let config = Config::load(&path).unwrap();

        let alerts = config.alerts.expect("[alerts] in the example");
        assert!(alerts.serbero_handoff);
        assert!(alerts.serbero_progress);
        assert!(config.serbero.is_none(), "Serbero alerts are opt-in");
    }

    #[test]
    fn a_key_pasted_into_the_serbero_section_is_rejected_without_echoing_it() {
        let err = load("\n[serbero]\nprivate_key = \"nsec1pastedsecretvalue\"\n").unwrap_err();

        // `main` prints the error it returns with `Debug`.
        let printed = format!("{err:?}");
        assert!(printed.contains("unknown field `private_key`"), "{printed}");
        assert!(printed.contains("line 13"), "{printed}");
        assert!(!printed.contains("nsec1pastedsecretvalue"), "{printed}");
    }

    #[test]
    fn a_key_pasted_as_the_variable_name_is_rejected_without_echoing_it() {
        let secret = Keys::generate().secret_key().to_secret_hex();

        let err = load(&format!("\n[serbero]\nprivate_key_env = \"{secret}\"\n")).unwrap_err();

        let printed = format!("{err:?}");
        assert!(printed.contains("private_key_env"), "{printed}");
        assert!(!printed.contains(&secret), "{printed}");
    }

    #[test]
    fn loading_the_config_never_reads_the_secret() {
        // The variable is resolved separately at startup, so a config with
        // `[serbero]` loads even where the variable is unset (CI, tests).
        let config = load("\n[serbero]\nprivate_key_env = \"SURELY_UNSET_VARIABLE_42\"\n");

        assert!(config.is_ok());
    }
}
