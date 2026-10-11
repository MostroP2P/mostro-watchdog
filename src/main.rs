use nostr_sdk::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use teloxide::prelude::*;
use teloxide::types::Chat;
use tokio::sync::{mpsc, Notify, RwLock};
use tracing::{debug, error, info, warn};

mod config;
mod db;
mod disputes;
mod serbero;
mod solver;
mod timeline;
mod version;

use config::{Config, SerberoSettings};
use db::DisputeMessageStore;
use disputes::{handle_dispute_event, AlertMode, AlertSettings};
use serbero::alerts::SerberoAlerts;
use serbero::telegram::Messenger;
use timeline::{Names, Nudge};
use version::{version_message, VERSION};

/// Shared state for the currently active relay list (discovered via NIP-65 or bootstrap fallback)
type ActiveRelays = Arc<RwLock<Vec<String>>>;

/// Mostro's dispute events, kind 38386.
const DISPUTE_KIND: Kind = Kind::Custom(38386);

/// The id of the live dispute subscription. The event loop posts to the
/// channel only what arrives on it: nostr-sdk notifies the events of every
/// subscription, including the fetches of the solver catch-up, which bring
/// statuses that are months old.
const LIVE_DISPUTES: &str = "disputes-live";

/// Mostro's dispute events from `since` on.
fn live_dispute_filter(mostro_pubkey: PublicKey, since: Timestamp) -> Filter {
    Filter::new()
        .kind(DISPUTE_KIND)
        .author(mostro_pubkey)
        .since(since)
}

fn is_live_dispute_subscription(subscription_id: &SubscriptionId) -> bool {
    subscription_id.as_str() == LIVE_DISPUTES
}

/// Fetch NIP-65 (kind 10002) relay list metadata from a pubkey via the connected relays.
/// Returns the list of relay URLs if a kind 10002 event is found, or None.
async fn fetch_nip65_relays(client: &Client, pubkey: PublicKey) -> Option<Vec<String>> {
    let filter = Filter::new().kind(Kind::RelayList).author(pubkey).limit(1);

    let timeout = Duration::from_secs(15);
    let events = client.fetch_events(filter).timeout(timeout).await.ok()?;

    // Get the most recent event by created_at
    let event = events.into_iter().max_by_key(|e| e.created_at)?;

    let relays: Vec<String> = event
        .tags
        .iter()
        .filter_map(|tag| {
            let values: Vec<String> = tag.as_slice().iter().map(|s| s.to_string()).collect();
            if values.first().map(|s| s.as_str()) == Some("r") && values.len() >= 2 {
                Some(values[1].clone())
            } else {
                None
            }
        })
        .collect();

    if relays.is_empty() {
        None
    } else {
        Some(relays)
    }
}

/// Swap the client's relays: remove all current relays, add new ones, connect, and re-subscribe.
/// Returns Ok(()) on success.
async fn swap_relays(
    client: &Client,
    new_relays: &[String],
    mostro_pubkey: PublicKey,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let swap_time = Timestamp::now();

    client.remove_all_relays().force().await?;

    for relay in new_relays {
        client.add_relay(relay).await?;
    }

    client.connect().await;

    client
        .subscribe(live_dispute_filter(mostro_pubkey, swap_time))
        .with_id(SubscriptionId::new(LIVE_DISPUTES))
        .await?;

    Ok(())
}

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

        serde_json::json!({
            "status": if is_healthy { "healthy" } else { "unhealthy" },
            "uptime_seconds": uptime_secs,
            "events_processed": events_count,
            "last_event_timestamp": last_event_ts,
            "last_heartbeat_timestamp": last_heartbeat_ts,
            "version": VERSION
        })
        .to_string()
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

/// Start the NIP-65 relay discovery background task.
/// On first run, fetches the kind 10002 event and swaps relays if found.
/// Then re-fetches periodically every `refresh_interval` seconds.
/// Each of `relays_changed` is notified after each swap: subscriptions
/// other than the dispute one are re-sent by their owners, one `Notify` each.
fn start_nip65_task(
    client: Client,
    mostro_pubkey: PublicKey,
    bootstrap_relays: Vec<String>,
    active_relays: ActiveRelays,
    refresh_interval: u64,
    relays_changed: Vec<Arc<Notify>>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(refresh_interval));
        // Run immediately on first tick
        loop {
            interval.tick().await;

            info!("🔍 Fetching NIP-65 relay list for Mostro pubkey...");

            match fetch_nip65_relays(&client, mostro_pubkey).await {
                Some(discovered) => {
                    let current = active_relays.read().await.clone();

                    let mut sorted_current = current.clone();
                    sorted_current.sort();
                    let mut sorted_discovered = discovered.clone();
                    sorted_discovered.sort();

                    if sorted_current == sorted_discovered {
                        info!("NIP-65 relay list unchanged ({} relays)", discovered.len());
                        continue;
                    }

                    info!(
                        "NIP-65 relay list changed: {} -> {} relays",
                        current.len(),
                        discovered.len()
                    );

                    match swap_relays(&client, &discovered, mostro_pubkey).await {
                        Ok(()) => {
                            *active_relays.write().await = discovered.clone();

                            // Logged only, not sent to Telegram: the channel is reserved
                            // for dispute events, so relay changes stay out of it.
                            info!("✅ Switched to NIP-65 discovered relays: {:?}", discovered);
                        }
                        Err(e) => {
                            error!("Failed to swap to NIP-65 relays: {}", e);
                        }
                    }
                    // Even a failed swap may have removed relays, and their
                    // subscriptions with them.
                    for owner in &relays_changed {
                        owner.notify_one();
                    }
                }
                None => {
                    let current = active_relays.read().await.clone();
                    if current == bootstrap_relays {
                        warn!(
                            "No NIP-65 relay list found for Mostro pubkey. Using bootstrap relays as fallback."
                        );
                    } else {
                        info!(
                            "No NIP-65 relay list found. Keeping current discovered relays ({} relays).",
                            current.len()
                        );
                    }
                }
            }
        }
    });
}

/// Start health monitoring background tasks
fn start_health_tasks(
    health_monitor: Arc<HealthMonitor>,
    bot: Bot,
    chat_id: i64,
    health_config: &config::HealthConfig,
    client: Client,
    active_relays: ActiveRelays,
) {
    // Heartbeat task
    if health_config.heartbeat_enabled {
        let health_monitor_hb = health_monitor.clone();
        let bot_hb = bot.clone();
        let heartbeat_interval = health_config.heartbeat_interval;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(heartbeat_interval));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval.tick().await; // skip the immediate first tick

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
                    escape_markdown(&(uptime / 3600).to_string()),
                    escape_markdown(&((uptime % 3600) / 60).to_string()),
                    escape_markdown(&events_count.to_string())
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
            let check_period = std::cmp::max(threshold / 2, 1);
            let mut interval = tokio::time::interval(Duration::from_secs(check_period));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval.tick().await; // skip the immediate first tick

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
                            escape_markdown(&(threshold / 3600).to_string()),
                            escape_markdown(&(uptime / 3600).to_string()),
                            escape_markdown(&((uptime % 3600) / 60).to_string())
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
        let active_relays_rc = active_relays.clone();
        // Derive relay check cadence from relay_timeout (check every 10x the timeout, min 10s)
        let relay_timeout = health_config.relay_timeout;

        tokio::spawn(async move {
            let check_secs = std::cmp::max(relay_timeout, 1) * 10;
            let mut interval = tokio::time::interval(Duration::from_secs(check_secs));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval.tick().await; // skip first immediate tick to allow connections to establish

            loop {
                interval.tick().await;

                let relays = active_relays_rc.read().await.clone();
                let mut failed_relays = Vec::new();

                for relay_url_str in &relays {
                    match client_rc.relay(relay_url_str).await {
                        Ok(Some(relay)) => {
                            if relay.status() != RelayStatus::Connected {
                                failed_relays.push(relay_url_str.clone());
                            }
                        }
                        // Unknown to the pool or an invalid URL: treat as disconnected.
                        Ok(None) | Err(_) => {
                            failed_relays.push(relay_url_str.clone());
                        }
                    }
                }

                if !failed_relays.is_empty() {
                    // Relay connectivity issues are logged only: the Telegram channel is
                    // reserved for dispute events, so infrastructure noise stays out of it.
                    warn!(
                        "🔌 {} relay(s) disconnected: {} ({} connected). Attempting reconnection...",
                        failed_relays.len(),
                        failed_relays.join(", "),
                        relays.len() - failed_relays.len()
                    );

                    // Attempt to reconnect all failed/terminated relays
                    client_rc.connect().await;
                }
            }
        });
    }

    // HTTP health endpoint task
    if health_config.enable_http_endpoint {
        let health_monitor_http = health_monitor.clone();
        let http_port = health_config.http_port;
        let http_bind = health_config.http_bind.clone();

        tokio::spawn(async move {
            if let Err(e) = start_health_server(health_monitor_http, &http_bind, http_port).await {
                error!("Health HTTP server failed: {}", e);
            }
        });
    }
}

/// Start HTTP health status endpoint
async fn start_health_server(
    health_monitor: Arc<HealthMonitor>,
    bind: &str,
    port: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    use http_body_util::Full;
    use hyper::body::Bytes;
    use hyper::service::service_fn;
    use hyper::{Request, Response, StatusCode};
    use hyper_util::rt::TokioIo;
    use hyper_util::server::conn::auto::Builder;
    use std::convert::Infallible;
    use tokio::net::TcpListener;

    let addr = format!("{}:{}", bind, port);
    let listener = TcpListener::bind(&addr).await?;
    info!(
        "🌐 Health HTTP endpoint listening on http://{}/health",
        addr
    );

    loop {
        let (stream, _) = match listener.accept().await {
            Ok(conn) => conn,
            Err(e) => {
                error!("Failed to accept HTTP connection: {}", e);
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let health_monitor = health_monitor.clone();

        tokio::spawn(async move {
            let service = service_fn(move |req: Request<hyper::body::Incoming>| {
                let health_monitor = health_monitor.clone();
                async move {
                    match req.uri().path() {
                        "/health" => {
                            let status_json = health_monitor.get_status_json().await;
                            Ok::<Response<Full<Bytes>>, Infallible>(
                                Response::builder()
                                    .status(StatusCode::OK)
                                    .header("Content-Type", "application/json")
                                    .body(Full::from(Bytes::from(status_json)))
                                    .expect("valid response"),
                            )
                        }
                        _ => Ok(Response::builder()
                            .status(StatusCode::NOT_FOUND)
                            .body(Full::from(Bytes::from("Not Found")))
                            .expect("valid response")),
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

/// Commands the bot answers in Telegram.
#[derive(teloxide::macros::BotCommands, Clone)]
#[command(
    rename_rule = "lowercase",
    description = "mostro-watchdog supports these commands:"
)]
enum Command {
    #[command(description = "show the running version and commit")]
    Version,
    #[command(description = "link your Mostro solver key to get dispute chat notifications")]
    Link,
    #[command(description = "unlink your solver keys from this chat")]
    Unlink,
    #[command(description = "show the solver keys linked to this chat")]
    Status,
}

/// Whether a command sent in this chat may be answered.
///
/// Only private chats qualify. The dispute channel is reserved for dispute
/// events, so an answer sent to a group or channel would be exactly the kind of
/// non-dispute noise the bot must not produce — and since the bot is only ever a
/// member of the admin's own chats, replying in private also keeps the answer
/// with the person who asked.
fn is_answerable_chat(chat: &Chat) -> bool {
    chat.is_private()
}

/// Answer a bot command. `solver` is `None` without `[solver_notifications]`.
async fn answer_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    solver: Option<&solver::commands::SolverCommands>,
) -> ResponseResult<()> {
    if !is_answerable_chat(&msg.chat) {
        info!(
            "Ignoring command in non-private chat {}: replies are private-only",
            msg.chat.id
        );
        return Ok(());
    }

    let chat_id = msg.chat.id.0;
    let text = match (cmd, solver) {
        (Command::Version, _) => version_message(),
        (Command::Link | Command::Unlink | Command::Status, None) => {
            solver::commands::DISABLED_TEXT.to_owned()
        }
        (Command::Link, Some(solver)) => solver.link(chat_id, Timestamp::now().as_secs()).await,
        (Command::Unlink, Some(solver)) => solver.unlink(chat_id).await,
        (Command::Status, Some(solver)) => solver.status(chat_id).await,
    };
    bot.send_message(msg.chat.id, text)
        .parse_mode(teloxide::types::ParseMode::MarkdownV2)
        .await?;

    Ok(())
}

/// Start the Telegram command listener in the background.
///
/// It runs alongside the Nostr event loop: long polling for updates must not
/// block dispute processing.
fn start_command_listener(bot: Bot, solver: Option<solver::commands::SolverCommands>) {
    tokio::spawn(async move {
        use teloxide::repls::CommandReplExt;
        use teloxide::utils::command::BotCommands as _;

        // Register the command list so it shows up in Telegram's command menu.
        if let Err(e) = bot.set_my_commands(Command::bot_commands()).await {
            warn!("Failed to register bot commands with Telegram: {}", e);
        }

        info!("Telegram command listener started");
        let handler = move |bot: Bot, msg: Message, cmd: Command| {
            let solver = solver.clone();
            async move { answer_command(bot, msg, cmd, solver.as_ref()).await }
        };
        <Command as CommandReplExt>::repl(bot, handler).await;
        warn!("Telegram command listener stopped");
    });
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

    // Fails startup when `[serbero]` or `[solver_notifications]` is present
    // but the watchdog's key is not.
    let serbero_settings = resolve_serbero(&config)?;
    let solver_settings = solver::resolve(&config)?;
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

    // Initialize Nostr client with bootstrap relays
    let client = Client::default();

    for relay in &config.nostr.relays {
        info!("Adding bootstrap relay: {}", relay);
        client.add_relay(relay).await?;
    }

    client.connect().await;
    info!(
        "Connected to {} bootstrap relay(s)",
        config.nostr.relays.len()
    );

    // Subscribe to dispute events (kind 38386) from the configured Mostro pubkey
    let mostro_pubkey = PublicKey::from_bech32(&config.mostro.pubkey)
        .or_else(|_| PublicKey::from_hex(&config.mostro.pubkey))?;

    let dispute_filter = live_dispute_filter(mostro_pubkey, Timestamp::now());

    // Open the notification stream *before* subscribing: it only delivers what arrives
    // after this call, so events received while the rest of the startup runs would
    // otherwise be lost.
    let notifications = client.notifications();

    client
        .subscribe(dispute_filter)
        .with_id(SubscriptionId::new(LIVE_DISPUTES))
        .await?;

    info!("🔍 Subscribed to dispute events on bootstrap relays. Watching...");

    // Shared state: active relays (starts with bootstrap, updated by NIP-65 discovery)
    let active_relays: ActiveRelays = Arc::new(RwLock::new(config.nostr.relays.clone()));

    // Notified when NIP-65 discovery swaps the relays, so the Serbero and
    // solver subscriptions follow them.
    let relays_changed = Arc::new(Notify::new());
    let solver_relays_changed = Arc::new(Notify::new());

    // Start NIP-65 relay discovery background task
    start_nip65_task(
        client.clone(),
        mostro_pubkey,
        config.nostr.relays.clone(),
        active_relays.clone(),
        config.nostr.nip65_refresh_interval,
        vec![relays_changed.clone(), solver_relays_changed.clone()],
    );

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
        active_relays.clone(),
    );

    // Initialize dispute message store
    let db_path = config_path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("disputes.db");
    let dispute_store = Arc::new(
        DisputeMessageStore::new(&db_path)
            .await
            .expect("Failed to initialize dispute message store"),
    );
    let solver_store = match solver_settings {
        Some(_) => Some(solver::store::SolverStore::new(dispute_store.pool()).await?),
        None => None,
    };

    // Answer Telegram commands while the Nostr event loop runs
    start_command_listener(
        bot.clone(),
        solver_settings
            .as_ref()
            .zip(solver_store.clone())
            .map(|(settings, store)| {
                solver::commands::SolverCommands::new(store, settings.keys.public_key())
            }),
    );

    // Send startup notification
    let startup_msg = format!(
        "🐕 *mostro\\-watchdog* is now online and monitoring for disputes\\.\n\n\
         📊 Heartbeat: {}\n\
         🔔 Event silence alert: {}",
        if health_config.heartbeat_enabled {
            format!("every {} seconds", health_config.heartbeat_interval)
        } else {
            "disabled".to_string()
        },
        if health_config.event_alert_threshold > 0 {
            format!("{} seconds", health_config.event_alert_threshold)
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

    // Keep up with Serbero in the background: its DMs reach the event loop
    // live as notifications, and caught up through `serbero_backlog`.
    let (serbero_inbox, serbero_backlog) = match serbero_settings {
        Some(settings) => {
            let (inbox, backlog) = serbero::start(
                &client,
                settings,
                mostro_pubkey,
                relays_changed,
                Duration::from_secs(config.nostr.nip65_refresh_interval),
            );
            (Some(inbox), Some(backlog))
        }
        None => (None, None),
    };

    // Watch the dispute chats solvers delegate to the watchdog.
    let (solver_inbox, solver_backlog) = match solver_settings.zip(solver_store) {
        Some((settings, store)) => {
            let (inbox, backlog) = solver::start(
                &client,
                settings,
                mostro_pubkey,
                store,
                solver_relays_changed,
                Duration::from_secs(config.nostr.nip65_refresh_interval),
            );
            (Some(inbox), Some(backlog))
        }
        None => (None, None),
    };

    let alerts_config = config.alerts.unwrap_or_default();
    let chat_id = config.telegram.chat_id;
    let names = Names::new(alerts_config.solver_names.clone());
    let nudge = Nudge::from_config(&alerts_config);
    match nudge {
        Some(nudge) => info!(
            "🔔 Edit notifications on: a reply deleted after {}s follows each live edit",
            nudge.lifetime.as_secs()
        ),
        None => info!("Edit notifications off: edits of a dispute's message are silent"),
    }
    let serbero_alerts = SerberoAlerts {
        store: &dispute_store,
        telegram: &bot,
        show_progress: alerts_config.serbero_progress,
        names: names.clone(),
        nudge,
    };

    run_event_loop(
        notifications,
        EventLoop {
            bot: &bot,
            chat_id,
            alerts_config: &alerts_config,
            dispute_store: &dispute_store,
            names: &names,
            nudge,
            health_monitor: &health_monitor,
            serbero_alerts: &serbero_alerts,
            serbero_inbox,
            serbero_backlog,
            solver_inbox,
            solver_backlog,
        },
    )
    .await;

    Ok(())
}

/// Whether an alert is enabled for a kind-38386 dispute `status`, per
/// `[alerts]` in the config. A status this version does not know falls back
/// to `other`.
fn alert_enabled(status: &str, alerts_config: &config::AlertsConfig) -> bool {
    match status {
        "initiated" => alerts_config.initiated,
        "in-progress" => alerts_config.in_progress,
        "seller-refunded" => alerts_config.seller_refunded,
        "settled" => alerts_config.settled,
        "released" => alerts_config.released,
        "cooperatively-canceled" => alerts_config.cooperatively_canceled,
        _ => alerts_config.other,
    }
}

/// Resolves `[serbero]`: the watchdog's own keys from the environment and
/// Serbero's key when configured. `None` when Serbero alerts are off.
fn resolve_serbero(config: &Config) -> Result<Option<SerberoSettings>, Box<dyn std::error::Error>> {
    let Some(serbero) = &config.serbero else {
        return Ok(None);
    };
    // As a string, like the config errors: `main` prints it with `Debug`.
    let settings = serbero
        .resolve(|name| std::env::var(name).ok())
        .map_err(|e| e.to_string())?;
    let watchdog = settings.keys.public_key();
    info!(
        "🤖 Serbero alerts enabled. Watchdog Nostr pubkey: {} (hex: {}). \
         Add the hex key to Serbero's [[observers]].",
        watchdog.to_bech32()?,
        watchdog.to_hex()
    );
    match settings.pubkey {
        Some(serbero) => info!("Serbero pubkey (configured): {}", serbero.to_hex()),
        None => info!("Serbero pubkey: read from the Mostro node's info event"),
    }
    Ok(Some(settings))
}

/// How often held solver notifications are checked for an ended grace period.
const SOLVER_FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// What the event loop works with.
struct EventLoop<'a, M> {
    bot: &'a M,
    chat_id: i64,
    alerts_config: &'a config::AlertsConfig,
    dispute_store: &'a DisputeMessageStore,
    names: &'a Names,
    /// The notification after a live edit, when edits notify.
    nudge: Option<Nudge>,
    health_monitor: &'a HealthMonitor,
    serbero_alerts: &'a SerberoAlerts<'a, M>,
    serbero_inbox: Option<serbero::SerberoInbox>,
    serbero_backlog: Option<mpsc::UnboundedReceiver<Vec<Event>>>,
    solver_inbox: Option<solver::inbox::SolverInbox>,
    solver_backlog: Option<mpsc::UnboundedReceiver<Vec<Event>>>,
}

/// Handles dispute events and Serbero DMs one at a time, so a dispute's
/// alert and Serbero's line on it never race each other. Returns when the
/// Nostr client shuts down.
async fn run_event_loop<M: Messenger>(
    mut notifications: impl StreamExt<Item = ClientNotification> + Unpin,
    mut ctx: EventLoop<'_, M>,
) {
    // Held solver notifications are sent once their grace period is over.
    let mut solver_tick = tokio::time::interval(SOLVER_FLUSH_INTERVAL);
    solver_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // `Client::handle_notifications` was removed in nostr-sdk 0.45: notifications are
    // now consumed as a stream, which ends when the client shuts down.
    loop {
        tokio::select! {
            notification = notifications.next() => match notification {
                Some(ClientNotification::Event { subscription_id, event, .. }) => {
                    if event.kind == DISPUTE_KIND {
                        // The solver catch-up's fetches reach here too; those
                        // events come back through its backlog, in catch-up
                        // mode, so they are dropped here.
                        if !is_live_dispute_subscription(&subscription_id) {
                            debug!(
                                subscription = %subscription_id,
                                event_id = %event.id,
                                "Ignoring a dispute event from a subscription other than the live one"
                            );
                            continue;
                        }
                        ctx.health_monitor.record_event().await;
                        ctx.handle_dispute(&event, AlertMode::Live).await;
                        if let Some(solver) = ctx.solver_inbox.as_ref() {
                            solver.on_dispute_event(&event).await;
                        }
                    } else if event.kind == Kind::PrivateDirectMessage {
                        if let Some(inbox) = ctx.serbero_inbox.as_mut() {
                            inbox.receive(&event, ctx.serbero_alerts, AlertMode::Live).await;
                        }
                        if let Some(solver) = ctx.solver_inbox.as_mut() {
                            solver.receive(&event, ctx.bot, Timestamp::now().as_secs()).await;
                        }
                    }
                }
                Some(ClientNotification::Message { .. }) => {}
                Some(ClientNotification::Shutdown) | None => {
                    info!("Nostr client shut down, stopping event loop");
                    break;
                }
            },
            Some(events) = next_batch(&mut ctx.serbero_backlog) => {
                if let Some(inbox) = ctx.serbero_inbox.as_mut() {
                    inbox.receive_batch(events, ctx.serbero_alerts).await;
                }
            }
            Some(events) = next_batch(&mut ctx.solver_backlog) => {
                // Oldest first, as a catch-up may hold several statuses of
                // one dispute. The solver inbox ends the resolved disputes
                // from the same batch, once per event.
                let mut disputes: Vec<&Event> =
                    events.iter().filter(|event| event.kind == DISPUTE_KIND).collect();
                disputes.sort_by_key(|event| (event.created_at, event.id));
                for event in disputes {
                    ctx.handle_dispute(event, AlertMode::CatchUp).await;
                }
                if let Some(solver) = ctx.solver_inbox.as_mut() {
                    solver.receive_batch(events, ctx.bot, Timestamp::now().as_secs()).await;
                }
            }
            _ = solver_tick.tick(), if ctx.solver_inbox.is_some() => {
                if let Some(solver) = ctx.solver_inbox.as_mut() {
                    solver.flush(ctx.bot, Timestamp::now().as_secs()).await;
                }
            }
        }
    }
}

impl<M: Messenger> EventLoop<'_, M> {
    async fn handle_dispute(&self, event: &Event, mode: AlertMode) {
        handle_dispute_event(
            self.bot,
            self.chat_id,
            event,
            mode,
            AlertSettings {
                config: self.alerts_config,
                names: self.names,
                nudge: self.nudge,
            },
            self.dispute_store,
        )
        .await;
    }
}

/// The next batch of caught-up events from a background sync. Never resolves
/// when that sync is off.
async fn next_batch(
    backlog: &mut Option<mpsc::UnboundedReceiver<Vec<Event>>>,
) -> Option<Vec<Event>> {
    match backlog {
        Some(backlog) => backlog.recv().await,
        None => std::future::pending().await,
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
    use crate::serbero::testing::{Call, FakeTelegram};
    use config::AlertsConfig;
    use nostr_sdk::prelude::MockRelay;

    const DISPUTE: &str = "51733e4d-a155-465b-a97e-07fba5f0e485";
    const CHAT: i64 = -100_123;
    const SETTLE: Duration = Duration::from_millis(500);

    /// A client of the mock relay, plus a store and a fake Telegram for an
    /// event loop over it.
    struct Relayed {
        _dir: tempfile::TempDir,
        relay: MockRelay,
        client: Client,
        publisher: Client,
        mostro: Keys,
        store: DisputeMessageStore,
        telegram: FakeTelegram,
        alerts: AlertsConfig,
        names: Names,
        health: HealthMonitor,
    }

    async fn connect(url: &str) -> Client {
        let client = Client::default();
        client.add_relay(url).await.unwrap();
        client.connect().await;
        client
    }

    impl Relayed {
        async fn new() -> Self {
            let relay = MockRelay::run().await.unwrap();
            let url = relay.url().await.to_string();
            let dir = tempfile::tempdir().unwrap();
            let store = DisputeMessageStore::new(&dir.path().join("disputes.db"))
                .await
                .unwrap();
            Self {
                _dir: dir,
                client: connect(&url).await,
                publisher: connect(&url).await,
                relay,
                mostro: Keys::generate(),
                store,
                telegram: FakeTelegram::default(),
                alerts: AlertsConfig::default(),
                names: Names::default(),
                health: HealthMonitor::new(),
            }
        }

        fn dispute_event(&self, dispute_id: &str, status: &str, created_at: Timestamp) -> Event {
            EventBuilder::new(DISPUTE_KIND, "")
                .tag(Tag::identifier(dispute_id))
                .tag(Tag::parse(["s", status]).unwrap())
                .custom_created_at(created_at)
                .finalize(&self.mostro)
                .unwrap()
        }

        async fn publish(&self, event: &Event) {
            self.publisher.send_event(event).await.unwrap();
        }

        fn serbero_alerts(&self) -> SerberoAlerts<'_, FakeTelegram> {
            SerberoAlerts {
                store: &self.store,
                telegram: &self.telegram,
                show_progress: false,
                names: Names::default(),
                nudge: None,
            }
        }

        /// Runs the event loop over the client's notifications, with
        /// `solver_backlog` as the solver catch-up, until the client shuts
        /// down.
        async fn run_loop(
            &self,
            notifications: impl StreamExt<Item = ClientNotification> + Unpin,
            serbero_alerts: &SerberoAlerts<'_, FakeTelegram>,
            solver_backlog: mpsc::UnboundedReceiver<Vec<Event>>,
        ) {
            run_event_loop(
                notifications,
                EventLoop {
                    bot: &self.telegram,
                    chat_id: CHAT,
                    alerts_config: &self.alerts,
                    dispute_store: &self.store,
                    names: &self.names,
                    nudge: None,
                    health_monitor: &self.health,
                    serbero_alerts,
                    serbero_inbox: None,
                    serbero_backlog: None,
                    solver_inbox: None,
                    solver_backlog: Some(solver_backlog),
                },
            )
            .await;
        }

        async fn shutdown(self) {
            self.client.shutdown().await;
            self.publisher.shutdown().await;
            drop(self.relay);
        }
    }

    fn sent_texts(telegram: &FakeTelegram) -> Vec<String> {
        telegram
            .sends()
            .into_iter()
            .map(|call| match call {
                Call::Send { text, .. } => text,
                _ => unreachable!(),
            })
            .collect()
    }

    /// The incident: the solver catch-up fetches, on the same client, the
    /// months-old status of a dispute the channel never saw. nostr-sdk
    /// notifies it like any event, under the fetch's subscription id.
    #[tokio::test]
    async fn only_the_live_subscription_posts_dispute_events() {
        let fx = Relayed::new().await;
        let (backlog, solver_backlog) = mpsc::unbounded_channel();
        let notifications = fx.client.notifications();
        fx.client
            .subscribe(live_dispute_filter(
                fx.mostro.public_key(),
                Timestamp::now(),
            ))
            .with_id(SubscriptionId::new(LIVE_DISPUTES))
            .await
            .unwrap();
        let serbero_alerts = fx.serbero_alerts();
        let stale = fx.dispute_event(
            DISPUTE,
            "in-progress",
            Timestamp::from_secs(Timestamp::now().as_secs() - 180 * 86_400),
        );
        let fresh_id = "9b7e6c1d-2f3a-4b5c-8d9e-0f1a2b3c4d5e";

        let drive = async {
            fx.publish(&stale).await;
            // What the solver catch-up does: a fetch with no lower bound,
            // handed to the event loop through its backlog.
            let fetched = fx
                .client
                .fetch_events(
                    Filter::new()
                        .kind(DISPUTE_KIND)
                        .author(fx.mostro.public_key())
                        .identifier(DISPUTE),
                )
                .timeout(Duration::from_secs(5))
                .await
                .unwrap();
            assert_eq!(fetched.len(), 1, "the relay holds the stale status");
            backlog.send(fetched.into_iter().collect()).unwrap();
            tokio::time::sleep(SETTLE).await;

            let fresh = fx.dispute_event(fresh_id, "initiated", Timestamp::now());
            fx.publish(&fresh).await;
            tokio::time::sleep(SETTLE).await;
            fx.client.shutdown().await;
        };
        tokio::join!(
            fx.run_loop(notifications, &serbero_alerts, solver_backlog),
            drive
        );

        let sent = sent_texts(&fx.telegram);
        assert_eq!(sent.len(), 1, "sent: {sent:?}");
        assert!(sent[0].contains("OPEN · needs a solver") && sent[0].contains(fresh_id));
        assert!(fx.telegram.edits().is_empty());
        assert_eq!(fx.store.get_message(DISPUTE).await.unwrap(), None);
        fx.shutdown().await;
    }

    /// A status the catch-up fetched before the live subscription saw it
    /// (nostr-sdk notifies an event once, under the subscription that
    /// stored it first) still updates the dispute's message.
    #[tokio::test]
    async fn a_caught_up_newer_status_updates_the_channel_message() {
        let fx = Relayed::new().await;
        let (backlog, solver_backlog) = mpsc::unbounded_channel();
        let notifications = fx.client.notifications();
        fx.client
            .subscribe(live_dispute_filter(
                fx.mostro.public_key(),
                Timestamp::now(),
            ))
            .with_id(SubscriptionId::new(LIVE_DISPUTES))
            .await
            .unwrap();
        let serbero_alerts = fx.serbero_alerts();
        let now = Timestamp::now();

        let drive = async {
            fx.publish(&fx.dispute_event(DISPUTE, "initiated", now))
                .await;
            tokio::time::sleep(SETTLE).await;
            let settled = fx.dispute_event(DISPUTE, "settled", now + 60);
            backlog.send(vec![settled]).unwrap();
            tokio::time::sleep(SETTLE).await;
            fx.client.shutdown().await;
        };
        tokio::join!(
            fx.run_loop(notifications, &serbero_alerts, solver_backlog),
            drive
        );

        assert_eq!(fx.telegram.sends().len(), 1);
        assert!(matches!(
            fx.telegram.edits().as_slice(),
            [Call::Edit { message_id: 1, text, .. }] if text.contains("RESOLVED · settled")
        ));
        fx.shutdown().await;
    }

    #[tokio::test]
    async fn a_relay_swap_keeps_the_live_subscription_id() {
        let fx = Relayed::new().await;
        let other = MockRelay::run().await.unwrap();
        let other_url = other.url().await.to_string();
        fx.client
            .subscribe(live_dispute_filter(
                fx.mostro.public_key(),
                Timestamp::now(),
            ))
            .with_id(SubscriptionId::new(LIVE_DISPUTES))
            .await
            .unwrap();

        swap_relays(
            &fx.client,
            std::slice::from_ref(&other_url),
            fx.mostro.public_key(),
        )
        .await
        .unwrap();

        let live = fx
            .client
            .subscription(&SubscriptionId::new(LIVE_DISPUTES))
            .await;
        let relays: Vec<String> = live.keys().map(|url| url.to_string()).collect();
        assert_eq!(relays, vec![other_url]);
        let filters = live.values().next().unwrap();
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].kinds.as_ref().unwrap().len(), 1);
        assert!(filters[0].kinds.as_ref().unwrap().contains(&DISPUTE_KIND));
        assert!(filters[0].since.is_some());
        fx.shutdown().await;
        drop(other);
    }

    #[test]
    fn only_the_live_subscription_id_is_live() {
        assert!(is_live_dispute_subscription(&SubscriptionId::new(
            LIVE_DISPUTES
        )));
        assert!(!is_live_dispute_subscription(&SubscriptionId::new(
            "solver-chat-0"
        )));
        assert!(!is_live_dispute_subscription(&SubscriptionId::generate()));
    }

    /// Build a `Chat` the way Telegram sends it, since the type has no public
    /// constructor.
    fn chat_from_json(json: &str) -> teloxide::types::Chat {
        serde_json::from_str(json).expect("valid chat payload")
    }

    #[test]
    fn answers_commands_in_private_chats() {
        let chat = chat_from_json(r#"{"id": 42, "type": "private", "first_name": "Admin"}"#);

        assert!(is_answerable_chat(&chat));
    }

    #[test]
    fn ignores_commands_in_groups_and_channels() {
        // The dispute channel is a group or channel: answering there would put
        // non-dispute traffic into it.
        for payload in [
            r#"{"id": -42, "type": "group", "title": "Disputes"}"#,
            r#"{"id": -42, "type": "supergroup", "title": "Disputes"}"#,
            r#"{"id": -42, "type": "channel", "title": "Disputes"}"#,
        ] {
            let chat = chat_from_json(payload);

            assert!(!is_answerable_chat(&chat), "should ignore: {payload}");
        }
    }

    #[test]
    fn a_missing_serbero_key_stops_startup_with_a_readable_error() {
        let config: Config = toml::from_str(
            r#"
            [mostro]
            pubkey = "npub1..."
            [nostr]
            relays = ["wss://relay.mostro.network"]
            [telegram]
            bot_token = "123:abc"
            chat_id = -1001
            [serbero]
            private_key_env = "MOSTRO_WATCHDOG_TEST_SURELY_UNSET_KEY"
            "#,
        )
        .expect("valid config");

        let err = resolve_serbero(&config).expect_err("no key in the environment");

        // `main` prints the error it returns with `Debug`.
        let printed = format!("{err:?}");
        assert!(
            printed.contains("MOSTRO_WATCHDOG_TEST_SURELY_UNSET_KEY"),
            "{printed}"
        );
        assert!(printed.contains("is not set"), "{printed}");
    }

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
        assert!(config.cooperatively_canceled);
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
        assert!(should_send_alert("cooperatively-canceled", &config));
        assert!(should_send_alert("unknown-status", &config)); // maps to other

        // `cooperatively-canceled` has its own switch, not `other`: turning
        // off unknown statuses must not silence it.
        config.other = false;
        assert!(should_send_alert("cooperatively-canceled", &config));
        config.cooperatively_canceled = false;
        assert!(!should_send_alert("cooperatively-canceled", &config));
        config.other = true;
        config.cooperatively_canceled = true;

        // Test specific disabling
        config.initiated = false;
        assert!(!should_send_alert("initiated", &config));
        assert!(should_send_alert("in-progress", &config)); // still enabled

        config.other = false;
        assert!(!should_send_alert("unknown-status", &config)); // maps to other
        assert!(should_send_alert("settled", &config)); // still enabled
    }

    /// The production gate itself, not a copy of it: a copy let a status be
    /// added to `handle_dispute_event` without any test noticing.
    fn should_send_alert(status: &str, alerts_config: &AlertsConfig) -> bool {
        alert_enabled(status, alerts_config)
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

        assert!(!config.heartbeat_enabled); // Disabled by default to avoid flooding chat
        assert_eq!(config.heartbeat_interval, 3600); // 1 hour
        assert!(config.check_relays);
        assert_eq!(config.relay_timeout, 30);
        assert_eq!(config.event_alert_threshold, 7200); // 2 hours
        assert!(!config.enable_http_endpoint); // Disabled by default
        assert_eq!(config.http_port, 8080);
    }
}
