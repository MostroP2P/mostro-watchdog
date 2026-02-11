use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub mostro: MostroConfig,
    pub nostr: NostrConfig,
    pub telegram: TelegramConfig,
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
            return Err(format!(
                "Config file not found: {}\nRun with: mostro-watchdog <config.toml>\nSee config.example.toml for reference.",
                path.display()
            )
            .into());
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

        Ok(config)
    }
}
