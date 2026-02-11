use nostr_sdk::prelude::*;
use std::path::PathBuf;
use teloxide::prelude::*;
use tracing::{error, info, warn};

mod config;

use config::Config;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("mostro_watchdog=info".parse()?),
        )
        .init();

    let config_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("config.toml"));

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
    client
        .handle_notifications(|notification| {
            let bot = bot.clone();
            let chat_id = config.telegram.chat_id;

            async move {
                if let RelayPoolNotification::Event { event, .. } = notification {
                    if event.kind == Kind::Custom(38386) {
                        handle_dispute_event(&bot, chat_id, &event).await;
                    }
                }
                Ok(false) // Keep listening
            }
        })
        .await?;

    Ok(())
}

async fn handle_dispute_event(bot: &Bot, chat_id: i64, event: &Event) {
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

    // Only notify on new disputes (status: initiated)
    if status != "initiated" {
        info!("Skipping non-initiation event (status: {})", status);
        return;
    }

    let message = format!(
        "🚨 *NEW DISPUTE*\n\n\
         📋 *Dispute ID:* `{}`\n\
         👤 *Initiated by:* {}\n\
         ⏰ *Time:* {}\n\n\
         ⚡ Please take this dispute in Mostrix or your admin client\\.",
        escape_markdown(&dispute_id),
        escape_markdown(&initiator),
        escape_markdown(&chrono_timestamp(event.created_at.as_u64())),
    );

    if let Err(e) = bot
        .send_message(ChatId(chat_id), &message)
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .await
    {
        error!("Failed to send Telegram alert: {}", e);
    } else {
        info!("✅ Telegram alert sent for dispute {}", dispute_id);
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
        let days_in_year = if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 366 } else { 365 };
        if remaining < days_in_year { break; }
        remaining -= days_in_year;
        y += 1;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0usize;
    for md in &month_days {
        if remaining < *md { break; }
        remaining -= md;
        m += 1;
    }
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC", y, m + 1, remaining + 1, hours, minutes, seconds)
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
