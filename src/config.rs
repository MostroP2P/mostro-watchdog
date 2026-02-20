use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub mostro: MostroConfig,
    pub nostr: NostrConfig,
    pub telegram: TelegramConfig,
    pub alerts: Option<AlertsConfig>,
    pub health: Option<HealthConfig>,
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
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct HealthConfig {
    /// Enable periodic heartbeat notifications
    #[serde(default = "default_true")]
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

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            heartbeat_enabled: true,
            heartbeat_interval: default_heartbeat_interval(),
            check_relays: true,
            relay_timeout: default_connection_timeout(),
            event_alert_threshold: default_event_alert_threshold(),
            enable_http_endpoint: false,
            http_port: default_http_port(),
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
    /// List of Nostr relay URLs to connect to
    pub relays: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramConfig {
    /// Telegram bot token from @BotFather
    pub bot_token: String,
    /// Telegram chat ID where alerts will be sent (group or channel)
    pub chat_id: i64,
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
        let config: Config = toml::from_str(&content)?;

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

        if let Some(ref health) = config.health {
            if health.heartbeat_enabled && health.heartbeat_interval == 0 {
                return Err("heartbeat_interval must be greater than 0".into());
            }
            if health.relay_timeout == 0 {
                return Err("relay_timeout must be greater than 0".into());
            }
        }

        Ok(config)
    }
}
