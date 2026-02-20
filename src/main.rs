use nostr_sdk::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use teloxide::prelude::*;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

mod config;

use config::Config;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Health monitor to track system status and send periodic heartbeats
#[derive(Debug, Clone)]
struct HealthMonitor {
    /// Last time we received a dispute event
    last_event_time: Arc<RwLock<Option<SystemTime>>>,
    /// Last time we sent a heartbeat  
    last_heartbeat: Arc<RwLock<Option<SystemTime>>>,
    /// Start time of the application
    start_time: SystemTime,
    /// Number of events processed
    events_processed: Arc<RwLock<u64>>,
    /// Health status
    is_healthy: Arc<RwLock<bool>>,
}

impl HealthMonitor {
    fn new() -> Self {
        Self {
            last_event_time: Arc::new(RwLock::new(None)),
            last_heartbeat: Arc::new(RwLock::new(None)),
            start_time: SystemTime::now(),
            events_processed: Arc::new(RwLock::new(0)),
            is_healthy: Arc::new(RwLock::new(true)),
        }
    }

    /// Record that we received an event
    async fn record_event(&self) {
        *self.last_event_time.write().await = Some(SystemTime::now());
        *self.events_processed.write().await += 1;
    }

    /// Record that we sent a heartbeat
    async fn record_heartbeat(&self) {
        *self.last_heartbeat.write().await = Some(SystemTime::now());
    }

    /// Check if we should be concerned about lack of events
    async fn should_alert_no_events(&self, threshold_seconds: u64) -> bool {
        if threshold_seconds == 0 {
            return false; // Disabled
        }

        let last_event = *self.last_event_time.read().await;
        match last_event {
            None => {
                // No events yet - check if we've been running long enough to be concerned
                let uptime = self.start_time.elapsed().unwrap_or(Duration::ZERO);
                uptime.as_secs() > threshold_seconds
            }
            Some(last) => {
                let elapsed = last.elapsed().unwrap_or(Duration::MAX);
                elapsed.as_secs() > threshold_seconds
            }
        }
    }

    /// Get health status as JSON
    async fn get_status_json(&self) -> String {
        let last_event = *self.last_event_time.read().await;
        let last_heartbeat = *self.last_heartbeat.read().await;
        let events_count = *self.events_processed.read().await;
        let is_healthy = *self.is_healthy.read().await;

        let uptime_secs = self
            .start_time
            .elapsed()
            .unwrap_or(Duration::ZERO)
            .as_secs();

        let last_event_ts = last_event
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs());

        let last_heartbeat_ts = last_heartbeat
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs());

        format!(
            r#"{{"status":"{}","uptime_seconds":{},"events_processed":{},"last_event_timestamp":{},"last_heartbeat_timestamp":{},"version":"{}"}}"#,
            if is_healthy { "healthy" } else { "unhealthy" },
            uptime_secs,
            events_count,
            last_event_ts
                .map(|t| t.to_string())
                .unwrap_or_else(|| "null".to_string()),
            last_heartbeat_ts
                .map(|t| t.to_string())
                .unwrap_or_else(|| "null".to_string()),
            VERSION
        )
    }
}

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

/// Start health monitoring background tasks
async fn start_health_tasks(
    health_monitor: Arc<HealthMonitor>,
    bot: Bot,
    chat_id: i64,
    health_config: &config::HealthConfig,
    client: Client,
    relays: &[String],
) {
    // Heartbeat task
    if health_config.heartbeat_enabled {
        let health_monitor_hb = health_monitor.clone();
        let bot_hb = bot.clone();
        let heartbeat_interval = health_config.heartbeat_interval;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(heartbeat_interval));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                let uptime = health_monitor_hb
                    .start_time
                    .elapsed()
                    .unwrap_or(Duration::ZERO)
                    .as_secs();

                let events_count = *health_monitor_hb.events_processed.read().await;

                let heartbeat_msg = format!(
                    "💓 *Health Check*\n\n\
                     ✅ System: Online\n\
                     ⏰ Uptime: {} hours {} minutes\n\
                     📊 Events processed: {}\n\
                     🔔 Status: Monitoring active",
                    uptime / 3600,
                    (uptime % 3600) / 60,
                    events_count
                );

                if let Err(e) = bot_hb
                    .send_message(ChatId(chat_id), &heartbeat_msg)
                    .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                    .await
                {
                    error!("Failed to send heartbeat: {}", e);
                } else {
                    health_monitor_hb.record_heartbeat().await;
                    info!(
                        "💓 Heartbeat sent (uptime: {}h {}m, events: {})",
                        uptime / 3600,
                        (uptime % 3600) / 60,
                        events_count
                    );
                }
            }
        });
    }

    // Event silence monitoring task
    if health_config.event_alert_threshold > 0 {
        let health_monitor_es = health_monitor.clone();
        let bot_es = bot.clone();
        let threshold = health_config.event_alert_threshold;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(threshold / 2)); // Check twice as often as threshold
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let mut last_alert = SystemTime::UNIX_EPOCH;

            loop {
                interval.tick().await;

                if health_monitor_es.should_alert_no_events(threshold).await {
                    // Avoid spam - only alert once every threshold period
                    let now = SystemTime::now();
                    if now
                        .duration_since(last_alert)
                        .unwrap_or(Duration::MAX)
                        .as_secs()
                        >= threshold
                    {
                        let uptime = health_monitor_es
                            .start_time
                            .elapsed()
                            .unwrap_or(Duration::ZERO)
                            .as_secs();

                        let alert_msg = format!(
                            "⚠️ *Event Silence Alert*\n\n\
                             🔕 No dispute events received for {} hours\n\
                             ⏰ System uptime: {} hours {} minutes\n\
                             🔍 Please check:\n\
                             • Mostro daemon status\n\
                             • Nostr relay connections\n\
                             • Network connectivity",
                            threshold / 3600,
                            uptime / 3600,
                            (uptime % 3600) / 60
                        );

                        if let Err(e) = bot_es
                            .send_message(ChatId(chat_id), &alert_msg)
                            .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                            .await
                        {
                            error!("Failed to send event silence alert: {}", e);
                        } else {
                            warn!(
                                "⚠️ Event silence alert sent ({}h threshold)",
                                threshold / 3600
                            );
                            last_alert = now;
                        }
                    }
                }
            }
        });
    }

    // Relay connectivity check task
    if health_config.check_relays {
        let client_rc = client.clone();
        let bot_rc = bot.clone();
        let relays_rc = relays.to_vec();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300)); // Check every 5 minutes
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                interval.tick().await;

                let relay_stats = client_rc.relay_pool().stats().await;
                let mut failed_relays = Vec::new();

                for relay_url in &relays_rc {
                    if let Some(stat) = relay_stats.get(relay_url) {
                        if stat.status() != nostr_sdk::relay::RelayStatus::Connected {
                            failed_relays.push(relay_url.clone());
                        }
                    } else {
                        failed_relays.push(relay_url.clone());
                    }
                }

                if !failed_relays.is_empty() {
                    let alert_msg = format!(
                        "🔌 *Relay Connection Alert*\n\n\
                         ⚠️ Disconnected relays: {}\n\
                         ✅ Connected relays: {}\n\
                         🔄 Attempting reconnection\\.\\.\\.",
                        failed_relays.len(),
                        relays_rc.len() - failed_relays.len()
                    );

                    if let Err(e) = bot_rc
                        .send_message(ChatId(chat_id), &alert_msg)
                        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
                        .await
                    {
                        error!("Failed to send relay alert: {}", e);
                    } else {
                        warn!(
                            "🔌 Relay connectivity alert sent ({} failed)",
                            failed_relays.len()
                        );
                    }

                    // Attempt to reconnect failed relays
                    for relay_url in &failed_relays {
                        if let Err(e) = client_rc.add_relay(relay_url).await {
                            error!("Failed to reconnect to relay {}: {}", relay_url, e);
                        }
                    }
                    client_rc.connect().await;
                }
            }
        });
    }

    // HTTP health endpoint task
    if health_config.enable_http_endpoint {
        let health_monitor_http = health_monitor.clone();
        let http_port = health_config.http_port;

        tokio::spawn(async move {
            if let Err(e) = start_health_server(health_monitor_http, http_port).await {
                error!("Health HTTP server failed: {}", e);
            }
        });
    }
}

/// Start HTTP health status endpoint
async fn start_health_server(
    health_monitor: Arc<HealthMonitor>,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    use hyper::body::Body;
    use hyper::service::service_fn;
    use hyper::{Request, Response, StatusCode};
    use hyper_util::rt::TokioIo;
    use hyper_util::server::conn::auto::Builder;
    use tokio::net::TcpListener;

    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await?;
    info!(
        "🌐 Health HTTP endpoint listening on http://{}/health",
        addr
    );

    loop {
        let (stream, _) = listener.accept().await?;
        let health_monitor = health_monitor.clone();

        tokio::spawn(async move {
            let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                let health_monitor = health_monitor.clone();
                async move {
                    match req.uri().path() {
                        "/health" => {
                            let status_json = health_monitor.get_status_json().await;
                            Ok::<Response<Body>, hyper::Error>(
                                Response::builder()
                                    .status(StatusCode::OK)
                                    .header("Content-Type", "application/json")
                                    .body(Body::from(status_json))?,
                            )
                        }
                        _ => Ok(Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(Body::from("Not Found"))?),
                    }
                }
            });

            if let Err(err) = Builder::new(hyper_util::rt::TokioExecutor::new())
                .serve_connection(TokioIo::new(stream), service)
                .await
            {
                error!("Error serving HTTP connection: {:?}", err);
            }
        });
    }
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

    // Initialize health monitor
    let health_monitor = Arc::new(HealthMonitor::new());
    let health_config = config.health.unwrap_or_default();

    // Start health check background tasks
    start_health_tasks(
        health_monitor.clone(),
        bot.clone(),
        config.telegram.chat_id,
        &health_config,
        client.clone(),
        &config.nostr.relays,
    )
    .await;

    // Send startup notification
    let startup_msg = format!(
        "🐕 *mostro\\-watchdog* is now online and monitoring for disputes\\.\n\n\
         📊 Health monitoring: {}\n\
         ⏰ Heartbeat interval: {} seconds\n\
         🔔 Event silence alert: {} seconds",
        if health_config.heartbeat_enabled {
            "enabled"
        } else {
            "disabled"
        },
        health_config.heartbeat_interval,
        if health_config.event_alert_threshold > 0 {
            health_config.event_alert_threshold.to_string()
        } else {
            "disabled".to_string()
        }
    );

    if let Err(e) = bot
        .send_message(ChatId(config.telegram.chat_id), &startup_msg)
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
            let health_monitor = health_monitor.clone();

            async move {
                if let RelayPoolNotification::Event { event, .. } = notification {
                    if event.kind == Kind::Custom(38386) {
                        health_monitor.record_event().await;
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

    #[tokio::test]
    async fn test_health_monitor_creation() {
        let health_monitor = HealthMonitor::new();

        // Initial state should be healthy with no events
        assert!(*health_monitor.is_healthy.read().await);
        assert_eq!(*health_monitor.events_processed.read().await, 0);
        assert!(health_monitor.last_event_time.read().await.is_none());
        assert!(health_monitor.last_heartbeat.read().await.is_none());

        // Start time should be recent
        let uptime = health_monitor
            .start_time
            .elapsed()
            .unwrap_or(Duration::ZERO);
        assert!(uptime.as_secs() < 10); // Should be created within last 10 seconds
    }

    #[tokio::test]
    async fn test_health_monitor_event_recording() {
        let health_monitor = HealthMonitor::new();

        // Record an event
        health_monitor.record_event().await;

        // Check that event was recorded
        assert_eq!(*health_monitor.events_processed.read().await, 1);
        assert!(health_monitor.last_event_time.read().await.is_some());

        // Record another event
        health_monitor.record_event().await;
        assert_eq!(*health_monitor.events_processed.read().await, 2);
    }

    #[tokio::test]
    async fn test_health_monitor_heartbeat_recording() {
        let health_monitor = HealthMonitor::new();

        // Initially no heartbeat
        assert!(health_monitor.last_heartbeat.read().await.is_none());

        // Record a heartbeat
        health_monitor.record_heartbeat().await;

        // Check that heartbeat was recorded
        assert!(health_monitor.last_heartbeat.read().await.is_some());
    }

    #[tokio::test]
    async fn test_should_alert_no_events() {
        let health_monitor = HealthMonitor::new();

        // With threshold 0 (disabled), should never alert
        assert!(!health_monitor.should_alert_no_events(0).await);

        // With threshold 10 and no events, should not alert immediately (just started)
        assert!(!health_monitor.should_alert_no_events(10).await);

        // Simulate system running for a while by manually setting start time
        let old_start = SystemTime::now() - Duration::from_secs(20);
        let health_monitor_old = HealthMonitor {
            last_event_time: Arc::new(RwLock::new(None)),
            last_heartbeat: Arc::new(RwLock::new(None)),
            start_time: old_start,
            events_processed: Arc::new(RwLock::new(0)),
            is_healthy: Arc::new(RwLock::new(true)),
        };

        // Now with no events and system running for 20 seconds, should alert with 10s threshold
        assert!(health_monitor_old.should_alert_no_events(10).await);

        // But if we record an event recently, should not alert
        health_monitor_old.record_event().await;
        assert!(!health_monitor_old.should_alert_no_events(10).await);
    }

    #[tokio::test]
    async fn test_health_monitor_status_json() {
        let health_monitor = HealthMonitor::new();

        // Get initial status
        let status_json = health_monitor.get_status_json().await;

        // Should be valid JSON with expected fields
        assert!(status_json.contains("\"status\":\"healthy\""));
        assert!(status_json.contains("\"events_processed\":0"));
        assert!(status_json.contains("\"version\":"));
        assert!(status_json.contains("\"uptime_seconds\":"));

        // Record some events and check updated status
        health_monitor.record_event().await;
        health_monitor.record_event().await;
        health_monitor.record_heartbeat().await;

        let updated_status = health_monitor.get_status_json().await;
        assert!(updated_status.contains("\"events_processed\":2"));
        assert!(updated_status.contains("\"last_event_timestamp\":"));
        assert!(updated_status.contains("\"last_heartbeat_timestamp\":"));
    }

    #[test]
    fn test_health_config_defaults() {
        let config = config::HealthConfig::default();

        assert!(config.heartbeat_enabled);
        assert_eq!(config.heartbeat_interval, 3600); // 1 hour
        assert!(config.check_relays);
        assert_eq!(config.relay_timeout, 30);
        assert_eq!(config.event_alert_threshold, 7200); // 2 hours
        assert!(!config.enable_http_endpoint); // Disabled by default
        assert_eq!(config.http_port, 8080);
    }
}
