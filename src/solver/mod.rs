//! Solver notifications: tell a solver in private when a party writes in the
//! chat of a dispute they took (SOLVER_NOTIFICATIONS.md).
//!
//! The watchdog never holds a key of the solver or of a party and never reads
//! a chat message. The solver's Mostrix links the solver key (`link`), names
//! the public signing keys of each dispute's conversations (`watch`,
//! `unwatch`), and announces the solver's own messages (`sent`), so they are
//! not notified.

pub mod code;
pub mod commands;
pub mod inbox;
pub mod notifier;
pub mod protocol;
pub mod store;
pub mod sync;

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::{mpsc, Notify};
use tracing::info;

use crate::config::{Config, SolverNotificationsSettings};
use inbox::SolverInbox;
use store::SolverStore;
use sync::SolverSync;

/// Resolves `[solver_notifications]`: the watchdog's own keys from the
/// environment. `None` when solver notifications are off.
pub fn resolve(config: &Config) -> Result<Option<SolverNotificationsSettings>, String> {
    let Some(section) = &config.solver_notifications else {
        return Ok(None);
    };
    // As a string, like the config errors: `main` prints it with `Debug`.
    let settings = section
        .resolve(|name| std::env::var(name).ok())
        .map_err(|e| e.to_string())?;
    let watchdog = settings.keys.public_key();
    info!(
        "📩 Solver notifications enabled (grace period {}s). Watchdog Nostr pubkey: {}",
        settings.grace_period.as_secs(),
        watchdog.to_bech32().unwrap_or_else(|_| watchdog.to_hex())
    );
    Ok(Some(settings))
}

/// Starts keeping up with solvers in the background. Returns the inbox for
/// the event loop and the channel caught-up events arrive on.
pub fn start(
    client: &Client,
    settings: SolverNotificationsSettings,
    mostro: PublicKey,
    store: SolverStore,
    relays_changed: Arc<Notify>,
    nip65_refresh: Duration,
) -> (SolverInbox, mpsc::UnboundedReceiver<Vec<Event>>) {
    let conversations_changed = Arc::new(Notify::new());
    let (backlog, caught_up) = mpsc::unbounded_channel();
    SolverSync {
        client: client.clone(),
        watchdog: settings.keys.public_key(),
        mostro,
        store: store.clone(),
        backlog,
        relays_changed,
        conversations_changed: conversations_changed.clone(),
        refresh: crate::serbero::sync::sync_interval(nip65_refresh),
        followed: Vec::new(),
        chat_subscriptions: 0,
    }
    .spawn();
    let inbox = SolverInbox::new(
        settings.keys,
        mostro,
        store,
        settings.grace_period,
        conversations_changed,
    );
    (inbox, caught_up)
}
