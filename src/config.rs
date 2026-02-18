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

        Ok(config)
    }
}
