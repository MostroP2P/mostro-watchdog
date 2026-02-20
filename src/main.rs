use nostr_sdk::prelude::*;
use std::path::PathBuf;
use teloxide::prelude::*;
use tracing::{error, info, warn};

mod config;

use config::Config;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Parse command-line arguments for config path.
///
/// Supported forms:
///   mostro-watchdog                          → config.toml (cwd)
///   mostro-watchdog /path/to/config.toml     → positional arg
///   mostro-watchdog --config /path/to/config  → named flag
///   mostro-watchdog -c /path/to/config        → short flag
///   mostro-watchdog --help | -h              → print usage
///   mostro-watchdog --version | -V           → print version
fn parse_config_path() -> PathBuf {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() {
        return default_config_path();
    }

    match args[0].as_str() {
        "--help" | "-h" => {
            print_usage();
            std::process::exit(0);
        }
        "--version" | "-V" => {
            println!("mostro-watchdog {VERSION}");
            std::process::exit(0);
        }
        "--config" | "-c" => {
            if let Some(path) = args.get(1) {
                PathBuf::from(path)
            } else {
                eprintln!("Error: --config requires a path argument\n");
                print_usage();
                std::process::exit(1);
            }
        }
        arg if arg.starts_with('-') => {
            eprintln!("Error: unknown option '{arg}'\n");
            print_usage();
            std::process::exit(1);
        }
        path => PathBuf::from(path),
    }
}

/// Resolve the default config path with fallback:
/// 1. ./config.toml (current directory)
/// 2. ~/.config/mostro-watchdog/config.toml
fn default_config_path() -> PathBuf {
    let local = PathBuf::from("config.toml");
    if local.exists() {
        return local;
    }

    if let Some(home) = std::env::var_os("HOME") {
        let xdg = PathBuf::from(home).join(".config/mostro-watchdog/config.toml");
        if xdg.exists() {
            return xdg;
        }
    }

    // Return local path anyway — Config::load will produce a helpful error
    local
}

fn print_usage() {
    println!(
        "🐕 mostro-watchdog {VERSION} — Dispute notification bot for Mostro admins\n\n\
         USAGE:\n\
         \x20   mostro-watchdog [OPTIONS] [CONFIG_PATH]\n\n\
         ARGS:\n\
         \x20   [CONFIG_PATH]  Path to config.toml (default: ./config.toml)\n\n\
         OPTIONS:\n\
         \x20   -c, --config <PATH>  Path to config file\n\
         \x20   -h, --help           Print this help message\n\
         \x20   -V, --version        Print version\n\n\
         CONFIG SEARCH ORDER:\n\
         \x20   1. ./config.toml (current directory)\n\
         \x20   2. ~/.config/mostro-watchdog/config.toml\n\n\
         EXAMPLES:\n\
         \x20   mostro-watchdog\n\
         \x20   mostro-watchdog /etc/mostro-watchdog/config.toml\n\
         \x20   mostro-watchdog --config ~/my-config.toml\n\
         \x20   RUST_LOG=debug mostro-watchdog"
    );
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("mostro_watchdog=info".parse()?),
        )
        .init();

    let config_path = parse_config_path();

    let config = Config::load(&config_path)?;

    info!("🐕 mostro-watchdog starting...");
    info!("Monitoring Mostro pubkey: {}", config.mostro.pubkey);
    info!(
        "Sending alerts to Telegram chat: {}",
        config.telegram.chat_id
    );

    // Initialize Telegram bot
    let bot = Bot::new(&config.telegram.bot_token);

    // Verify Telegram bot connection
    match bot.get_me().await {
        Ok(me) => info!("Telegram bot connected: @{}", me.username()),
        Err(e) => {
            error!("Failed to connect Telegram bot: {}", e);
            return Err(e.into());
        }
    }

    // Initialize Nostr client
    let client = Client::default();

    for relay in &config.nostr.relays {
        info!("Adding relay: {}", relay);
        client.add_relay(relay).await?;
    }

    client.connect().await;
    info!("Connected to {} relay(s)", config.nostr.relays.len());

    // Subscribe to dispute events (kind 38386) from the configured Mostro pubkey
    let mostro_pubkey = PublicKey::from_bech32(&config.mostro.pubkey)
        .or_else(|_| PublicKey::from_hex(&config.mostro.pubkey))?;

    let dispute_filter = Filter::new()
        .kind(Kind::Custom(38386))
        .author(mostro_pubkey)
        .since(Timestamp::now());

    client.subscribe(vec![dispute_filter], None).await?;

    info!("🔍 Subscribed to dispute events. Watching...");

    // Send startup notification
    let startup_msg = "🐕 *mostro\\-watchdog* is now online and monitoring for disputes\\.";
    if let Err(e) = bot
        .send_message(ChatId(config.telegram.chat_id), startup_msg)
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .await
    {
        warn!("Failed to send startup message: {}", e);
    }

    // Process events
    let alerts_config = config.alerts.unwrap_or_default();
    client
        .handle_notifications(|notification| {
            let bot = bot.clone();
            let chat_id = config.telegram.chat_id;
            let alerts_config = alerts_config.clone();

            async move {
                if let RelayPoolNotification::Event { event, .. } = notification {
                    if event.kind == Kind::Custom(38386) {
                        handle_dispute_event(&bot, chat_id, &event, &alerts_config).await;
                    }
                }
                Ok(false) // Keep listening
            }
        })
        .await?;

    Ok(())
}

async fn handle_dispute_event(
    bot: &Bot,
    chat_id: i64,
    event: &Event,
    alerts_config: &config::AlertsConfig,
) {
    let mut dispute_id = String::from("unknown");
    let mut status = String::from("unknown");
    let mut initiator = String::from("unknown");

    for tag in event.tags.iter() {
        let tag_vec: Vec<String> = tag.as_slice().iter().map(|s| s.to_string()).collect();
        if tag_vec.len() >= 2 {
            match tag_vec[0].as_str() {
                "d" => dispute_id = tag_vec[1].clone(),
                "s" => status = tag_vec[1].clone(),
                "initiator" => initiator = tag_vec[1].clone(),
                _ => {}
            }
        }
    }

    info!(
        "Dispute event received: id={}, status={}, initiator={}",
        dispute_id, status, initiator
    );

    // Check if this alert type is enabled
    let alert_enabled = match status.as_str() {
        "initiated" => alerts_config.initiated,
        "in-progress" => alerts_config.in_progress,
        "seller-refunded" => alerts_config.seller_refunded,
        "settled" => alerts_config.settled,
        "released" => alerts_config.released,
        _ => alerts_config.other,
    };

    if !alert_enabled {
        info!(
            "Alert for status '{}' is disabled, skipping notification",
            status
        );
        return;
    }

    // Generate appropriate message based on status
    let message = match status.as_str() {
        "initiated" => {
            format!(
                "🚨 *NEW DISPUTE*\n\n\
                 📋 *Dispute ID:* `{}`\n\
                 👤 *Initiated by:* {}\n\
                 ⏰ *Time:* {}\n\n\
                 ⚡ Please take this dispute in Mostrix or your admin client\\.",
                escape_markdown_code(&dispute_id),
                escape_markdown(&initiator),
                escape_markdown(&chrono_timestamp(event.created_at.as_u64())),
            )
        }
        "in-progress" => {
            format!(
                "🔄 *DISPUTE IN PROGRESS*\n\n\
                 📋 *Dispute ID:* `{}`\n\
                 👨‍⚖️ *Status:* Taken by solver\n\
                 ⏰ *Time:* {}\n\n\
                 ℹ️ Dispute is now being handled\\.",
                escape_markdown_code(&dispute_id),
                escape_markdown(&chrono_timestamp(event.created_at.as_u64())),
            )
        }
        "seller-refunded" => {
            format!(
                "💰 *DISPUTE RESOLVED*\n\n\
                 📋 *Dispute ID:* `{}`\n\
                 ✅ *Resolution:* Seller refunded\n\
                 ⏰ *Time:* {}\n\n\
                 ✔️ Dispute closed: funds returned to seller\\.",
                escape_markdown_code(&dispute_id),
                escape_markdown(&chrono_timestamp(event.created_at.as_u64())),
            )
        }
        "settled" => {
            format!(
                "✅ *DISPUTE RESOLVED*\n\n\
                 📋 *Dispute ID:* `{}`\n\
                 💸 *Resolution:* Payment to buyer\n\
                 ⏰ *Time:* {}\n\n\
                 ✔️ Dispute closed: buyer receives payment\\.",
                escape_markdown_code(&dispute_id),
                escape_markdown(&chrono_timestamp(event.created_at.as_u64())),
            )
        }
        "released" => {
            format!(
                "🔓 *DISPUTE RESOLVED*\n\n\
                 📋 *Dispute ID:* `{}`\n\
                 🤝 *Resolution:* Released by seller\n\
                 ⏰ *Time:* {}\n\n\
                 ✔️ Dispute closed: trade completed\\.",
                escape_markdown_code(&dispute_id),
                escape_markdown(&chrono_timestamp(event.created_at.as_u64())),
            )
        }
        _ => {
            format!(
                "📡 *DISPUTE STATUS UPDATE*\n\n\
                 📋 *Dispute ID:* `{}`\n\
                 📊 *Status:* {}\n\
                 ⏰ *Time:* {}\n\n\
                 ℹ️ Status changed\\.",
                escape_markdown_code(&dispute_id),
                escape_markdown(&status),
                escape_markdown(&chrono_timestamp(event.created_at.as_u64())),
            )
        }
    };

    if let Err(e) = bot
        .send_message(ChatId(chat_id), &message)
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .await
    {
        error!("Failed to send Telegram alert: {}", e);
    } else {
        info!(
            "✅ Telegram alert sent for dispute {} (status: {})",
            dispute_id, status
        );
    }
}

fn chrono_timestamp(unix: u64) -> String {
    let secs = unix as i64;
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    // Simple days-since-epoch to Y-M-D (good enough for 2020-2099)
    let mut y = 1970i64;
    let mut remaining = days;
    loop {
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            366
        } else {
            365
        };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0usize;
    for md in &month_days {
        if remaining < *md {
            break;
        }
        remaining -= md;
        m += 1;
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        y,
        m + 1,
        remaining + 1,
        hours,
        minutes,
        seconds
    )
}

fn escape_markdown(text: &str) -> String {
    let special_chars = [
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
    ];
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if special_chars.contains(&c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// Escape text for use inside MarkdownV2 code spans.
/// Only escapes backticks and backslashes since code spans protect against other formatting.
fn escape_markdown_code(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if c == '`' || c == '\\' {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::AlertsConfig;

    #[test]
    fn test_escape_markdown() {
        // Test all special characters
        assert_eq!(escape_markdown("_italic_"), "\\_italic\\_");
        assert_eq!(escape_markdown("*bold*"), "\\*bold\\*");
        assert_eq!(escape_markdown("[link]"), "\\[link\\]");
        assert_eq!(escape_markdown("(paren)"), "\\(paren\\)");
        assert_eq!(escape_markdown("~strike~"), "\\~strike\\~");
        assert_eq!(escape_markdown("`code`"), "\\`code\\`");
        assert_eq!(escape_markdown(">quote"), "\\>quote");
        assert_eq!(escape_markdown("#header"), "\\#header");
        assert_eq!(escape_markdown("+plus"), "\\+plus");
        assert_eq!(escape_markdown("-minus"), "\\-minus");
        assert_eq!(escape_markdown("=equals"), "\\=equals");
        assert_eq!(escape_markdown("|pipe|"), "\\|pipe\\|");
        assert_eq!(escape_markdown("{brace}"), "\\{brace\\}");
        assert_eq!(escape_markdown(".dot"), "\\.dot");
        assert_eq!(escape_markdown("!exclaim"), "\\!exclaim");

        // Test complex case with special characters from CodeRabbit example
        assert_eq!(
            escape_markdown("test_123-abc*def"),
            "test\\_123\\-abc\\*def"
        );

        // Test empty and normal text
        assert_eq!(escape_markdown(""), "");
        assert_eq!(escape_markdown("normal text"), "normal text");
    }

    #[test]
    fn test_escape_markdown_code() {
        // Only backticks and backslashes should be escaped in code spans
        assert_eq!(
            escape_markdown_code("test`with`backticks"),
            "test\\`with\\`backticks"
        );
        assert_eq!(
            escape_markdown_code("test\\with\\backslashes"),
            "test\\\\with\\\\backslashes"
        );
        assert_eq!(escape_markdown_code("test`and\\both"), "test\\`and\\\\both");

        // Other markdown characters should NOT be escaped in code spans
        assert_eq!(escape_markdown_code("test_123-abc*def"), "test_123-abc*def");
        assert_eq!(
            escape_markdown_code("*bold* _italic_ [link]"),
            "*bold* _italic_ [link]"
        );

        // Test empty and normal text
        assert_eq!(escape_markdown_code(""), "");
        assert_eq!(escape_markdown_code("normal text"), "normal text");
    }

    #[test]
    fn test_chrono_timestamp() {
        // Test known Unix timestamp: 1609459200 = 2021-01-01 00:00:00 UTC
        assert_eq!(chrono_timestamp(1609459200), "2021-01-01 00:00:00 UTC");

        // Test another known timestamp: 1640995200 = 2022-01-01 00:00:00 UTC
        assert_eq!(chrono_timestamp(1640995200), "2022-01-01 00:00:00 UTC");

        // Test with time: 1609459200 + 3661 = 2021-01-01 01:01:01 UTC
        assert_eq!(chrono_timestamp(1609462861), "2021-01-01 01:01:01 UTC");

        // Test leap year: 1582934400 = 2020-02-29 00:00:00 UTC (leap year)
        assert_eq!(chrono_timestamp(1582934400), "2020-02-29 00:00:00 UTC");
    }

    #[test]
    fn test_alerts_config_defaults() {
        let config = AlertsConfig::default();
        assert!(config.initiated);
        assert!(config.in_progress);
        assert!(config.seller_refunded);
        assert!(config.settled);
        assert!(config.released);
        assert!(config.other);
    }

    #[test]
    fn test_alert_gating_logic() {
        let mut config = AlertsConfig::default();

        // Test all enabled (default)
        assert!(should_send_alert("initiated", &config));
        assert!(should_send_alert("in-progress", &config));
        assert!(should_send_alert("seller-refunded", &config));
        assert!(should_send_alert("settled", &config));
        assert!(should_send_alert("released", &config));
        assert!(should_send_alert("unknown-status", &config)); // maps to other

        // Test specific disabling
        config.initiated = false;
        assert!(!should_send_alert("initiated", &config));
        assert!(should_send_alert("in-progress", &config)); // still enabled

        config.other = false;
        assert!(!should_send_alert("unknown-status", &config)); // maps to other
        assert!(should_send_alert("settled", &config)); // still enabled
    }

    /// Helper function to test alert gating logic
    /// This mirrors the logic in handle_dispute_event
    fn should_send_alert(status: &str, alerts_config: &AlertsConfig) -> bool {
        match status {
            "initiated" => alerts_config.initiated,
            "in-progress" => alerts_config.in_progress,
            "seller-refunded" => alerts_config.seller_refunded,
            "settled" => alerts_config.settled,
            "released" => alerts_config.released,
            _ => alerts_config.other,
        }
    }

    #[test]
    fn test_edge_cases() {
        // Test unknown status mapping
        let config = AlertsConfig::default();
        assert!(should_send_alert("", &config)); // empty status maps to other
        assert!(should_send_alert("invalid-status", &config)); // unknown status maps to other

        // Test malformed events (simulated with empty strings)
        assert_eq!(escape_markdown_code(""), "");
        assert_eq!(chrono_timestamp(0), "1970-01-01 00:00:00 UTC"); // Unix epoch

        // Test boundary conditions - backslash is NOT in escape_markdown special chars
        assert_eq!(escape_markdown("\\"), "\\"); // backslash not escaped by escape_markdown
        assert_eq!(escape_markdown_code("\\"), "\\\\"); // but IS escaped by escape_markdown_code
        assert_eq!(escape_markdown_code("`"), "\\`");
    }
}
