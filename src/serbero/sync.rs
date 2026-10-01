//! Keeping up with Serbero in the background: which key to trust, the live
//! subscription to its DMs, and a catch-up of what was missed. Nothing here
//! holds the event loop back: live DMs reach it as notifications and
//! caught-up ones through the backlog channel.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::{mpsc, Notify, RwLock};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn};

use super::discovery::{discover, Discovery};

/// Start of the live subscription's id. The rest names the Serbero key:
/// nostr-sdk refuses an id it already holds, so a new key needs a new id.
const SUBSCRIPTION_PREFIX: &str = "mostro-watchdog-serbero";

/// Hex digits of the Serbero key in the subscription id (NIP-01 caps ids at
/// 64 characters).
const SUBSCRIPTION_KEY_DIGITS: usize = 16;

/// What nostr-sdk answers when a subscription id is already registered on a
/// relay: the subscription is in place, nothing failed.
const ALREADY_SUBSCRIBED: &str = "subscription ID already exists";

/// How far back each catch-up reads. A fixed window rather than "since the
/// newest DM relayed": an alert whose send failed is not marked as relayed,
/// so the next catch-up retries it, and the relayed-header table makes
/// reading the rest again harmless.
pub const CATCH_UP_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

/// Upper bound for one catch-up fetch, which waits for every relay.
pub const CATCH_UP_TIMEOUT: Duration = Duration::from_secs(30);

/// Upper bound for one discovery request; the grace usually ends it sooner.
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// The Serbero key whose DMs are trusted; `None` until one is known.
pub type TrustedSerbero = Arc<RwLock<Option<PublicKey>>>;

/// Id of the live subscription to `serbero`'s DMs.
pub fn subscription_id(serbero: &PublicKey) -> SubscriptionId {
    let digits: String = serbero
        .to_hex()
        .chars()
        .take(SUBSCRIPTION_KEY_DIGITS)
        .collect();
    SubscriptionId::new(format!("{SUBSCRIPTION_PREFIX}-{digits}"))
}

/// The relays that refused a subscription, and why. A relay that already
/// holds it is not a refusal.
pub fn refusals(failed: &HashMap<RelayUrl, String>) -> Vec<(&RelayUrl, &str)> {
    failed
        .iter()
        .filter(|(_, reason)| reason.as_str() != ALREADY_SUBSCRIBED)
        .map(|(relay, reason)| (relay, reason.as_str()))
        .collect()
}

/// Serbero's DMs to the watchdog.
pub fn dm_filter(serbero: PublicKey, watchdog: PublicKey) -> Filter {
    Filter::new()
        .kind(Kind::PrivateDirectMessage)
        .author(serbero)
        .pubkey(watchdog)
}

/// Start of the catch-up window.
pub fn catch_up_since(now: Timestamp) -> Timestamp {
    now - CATCH_UP_WINDOW
}

/// The key to trust after a discovery. A node that did not answer keeps
/// the current key; one that names no Serbero drops it.
pub fn next_serbero(current: Option<PublicKey>, discovery: Discovery) -> Option<PublicKey> {
    match discovery {
        Discovery::Serbero(serbero) => Some(serbero),
        Discovery::NoSerbero => None,
        Discovery::NotFound => current,
    }
}

/// The background task that keeps up with Serbero.
pub struct SerberoSync {
    pub client: Client,
    /// The watchdog's own key, which Serbero writes to.
    pub watchdog: PublicKey,
    pub mostro: PublicKey,
    /// `serbero.pubkey`; when set, nothing is discovered.
    pub configured: Option<PublicKey>,
    pub trusted: TrustedSerbero,
    pub backlog: mpsc::UnboundedSender<Vec<Event>>,
    /// Notified after the relay set changed (NIP-65): the subscription went
    /// with the old relays, so it is sent again and missed DMs fetched.
    pub relays_changed: Arc<Notify>,
    /// How often to rediscover the key, resend the subscription and catch
    /// up, for relays that dropped it or delivered nothing.
    pub refresh: Duration,
}

impl SerberoSync {
    /// Runs until the event loop stops reading the backlog.
    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(self.run())
    }

    async fn run(self) {
        let mut interval = tokio::time::interval(self.refresh);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            // The first tick is immediate: catch up at startup.
            tokio::select! {
                _ = interval.tick() => {}
                () = self.relays_changed.notified() => {}
            }
            if self.backlog.is_closed() {
                return;
            }
            self.sync_once().await;
        }
    }

    /// One round: settle which key to trust, send the live subscription
    /// first, then catch up.
    pub async fn sync_once(&self) {
        if let Some(serbero) = self.refresh_key().await {
            self.subscribe(serbero).await;
            self.catch_up(serbero).await;
        }
    }

    /// Updates the trusted key from the config or the node's info event.
    async fn refresh_key(&self) -> Option<PublicKey> {
        let current = *self.trusted.read().await;
        let next = match self.configured {
            Some(configured) => Some(configured),
            None => {
                let discovery = discover(&self.client, self.mostro, DISCOVERY_TIMEOUT).await;
                log_discovery(discovery, current);
                next_serbero(current, discovery)
            }
        };
        if next != current {
            *self.trusted.write().await = next;
            // The old key's subscription would keep delivering DMs that are
            // no longer trusted.
            if let Some(old) = current {
                self.unsubscribe(old).await;
            }
            if let Some(serbero) = next {
                info!(
                    serbero = %serbero.to_hex(),
                    "🤖 Relaying Serbero's updates to Telegram"
                );
            }
        }
        next
    }

    /// Sends the live subscription. Relays that already hold it keep it
    /// (nostr-sdk refuses a known id); relays that lost it, e.g. after a
    /// relay swap or a refused REQ, get it again.
    async fn subscribe(&self, serbero: PublicKey) {
        let filter = dm_filter(serbero, self.watchdog).since(Timestamp::now());
        match self
            .client
            .subscribe(filter)
            .with_id(subscription_id(&serbero))
            .await
        {
            Ok(output) => {
                for (relay, reason) in refusals(&output.failed) {
                    warn!(%relay, reason, "A relay refused the Serbero DM subscription");
                }
            }
            Err(e) => warn!(error = %e, "Failed to subscribe to Serbero's DMs"),
        }
    }

    async fn unsubscribe(&self, serbero: PublicKey) {
        if let Err(e) = self.client.unsubscribe(&subscription_id(&serbero)).await {
            debug!(error = %e, "Failed to close the Serbero DM subscription");
        }
    }

    /// Fetches the DMs of the catch-up window and hands them to the event
    /// loop, which skips the ones already relayed.
    async fn catch_up(&self, serbero: PublicKey) {
        let filter = dm_filter(serbero, self.watchdog).since(catch_up_since(Timestamp::now()));
        match self
            .client
            .fetch_events(filter)
            .timeout(CATCH_UP_TIMEOUT)
            .await
        {
            Ok(events) if events.is_empty() => {}
            Ok(events) => {
                debug!(count = events.len(), "Caught up on Serbero DMs");
                if self.backlog.send(events.into_iter().collect()).is_err() {
                    debug!("The event loop stopped; dropping caught-up Serbero DMs");
                }
            }
            Err(e) => warn!(error = %e, "Failed to catch up on Serbero DMs"),
        }
    }
}

fn log_discovery(discovery: Discovery, current: Option<PublicKey>) {
    match (discovery, current) {
        (Discovery::Serbero(_), _) => {}
        (Discovery::NoSerbero, Some(_)) => {
            warn!("The Mostro node no longer announces a Serbero; Serbero alerts are off")
        }
        (Discovery::NoSerbero, None) => warn!(
            "The Mostro node announces no Serbero (no `serbero` tag in its info event); \
             Serbero alerts stay off until it does"
        ),
        (Discovery::NotFound, Some(_)) => {
            debug!("The Mostro node's info event did not arrive; keeping the known Serbero key")
        }
        (Discovery::NotFound, None) => warn!(
            "The Mostro node's info event did not arrive, so Serbero's key is unknown; \
             Serbero alerts stay off until it is (set serbero.pubkey to skip discovery)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serbero::testing::serbero_dm;
    use mostro_core::prelude::NOSTR_INFO_EVENT_KIND;
    use std::time::Instant;

    const DISPUTE: &str = "58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a";
    const WAIT: Duration = Duration::from_secs(5);

    struct Setup {
        relay: MockRelay,
        client: Client,
        publisher: Client,
        serbero: Keys,
        watchdog: Keys,
        mostro: Keys,
    }

    async fn connect(urls: &[String]) -> Client {
        let client = Client::default();
        for url in urls {
            client.add_relay(url).await.unwrap();
        }
        client.connect().await;
        client
    }

    impl Setup {
        async fn new() -> Self {
            let relay = MockRelay::run().await.unwrap();
            let url = relay.url().await.to_string();
            Self {
                client: connect(std::slice::from_ref(&url)).await,
                publisher: connect(&[url]).await,
                relay,
                serbero: Keys::generate(),
                watchdog: Keys::generate(),
                mostro: Keys::generate(),
            }
        }

        fn sync(
            &self,
            configured: Option<PublicKey>,
            trusted: Option<PublicKey>,
        ) -> (SerberoSync, mpsc::UnboundedReceiver<Vec<Event>>) {
            let (backlog, received) = mpsc::unbounded_channel();
            let sync = SerberoSync {
                client: self.client.clone(),
                watchdog: self.watchdog.public_key(),
                mostro: self.mostro.public_key(),
                configured,
                trusted: Arc::new(RwLock::new(trusted)),
                backlog,
                relays_changed: Arc::new(Notify::new()),
                refresh: Duration::from_secs(3600),
            };
            (sync, received)
        }

        fn dm(&self, text: &str, created_at: Timestamp) -> Event {
            serbero_dm(
                &self.serbero,
                self.watchdog.public_key(),
                Some(DISPUTE),
                text,
                Some(created_at),
            )
        }

        async fn publish(&self, event: &Event) {
            self.publisher.send_event(event).await.unwrap();
        }

        async fn publish_info(&self, tags: &[&[&str]]) {
            let tags = std::iter::once(Tag::identifier(self.mostro.public_key().to_hex()))
                .chain(tags.iter().map(|t| Tag::parse(t.iter().copied()).unwrap()));
            let event = EventBuilder::new(Kind::Custom(NOSTR_INFO_EVENT_KIND), "")
                .tags(tags)
                .finalize(&self.mostro)
                .unwrap();
            self.publish(&event).await;
        }

        async fn shutdown(self) {
            self.client.shutdown().await;
            self.publisher.shutdown().await;
            drop(self.relay);
        }
    }

    async fn next_dm(
        notifications: &mut (impl StreamExt<Item = ClientNotification> + Unpin),
    ) -> (SubscriptionId, Event) {
        tokio::time::timeout(WAIT, async {
            while let Some(notification) = notifications.next().await {
                if let ClientNotification::Event {
                    subscription_id,
                    event,
                    ..
                } = notification
                {
                    if event.kind == Kind::PrivateDirectMessage {
                        return (subscription_id, *event);
                    }
                }
            }
            panic!("the notification stream ended");
        })
        .await
        .expect("a DM notification")
    }

    #[test]
    fn the_filter_reads_only_serbero_writing_to_the_watchdog() {
        let serbero = Keys::generate().public_key();
        let watchdog = Keys::generate().public_key();

        let json = dm_filter(serbero, watchdog).as_json();

        assert!(json.contains("\"kinds\":[14]"), "{json}");
        assert!(
            json.contains(&format!("\"authors\":[\"{}\"]", serbero.to_hex())),
            "{json}"
        );
        assert!(
            json.contains(&format!("\"#p\":[\"{}\"]", watchdog.to_hex())),
            "{json}"
        );
    }

    #[test]
    fn the_catch_up_reads_the_last_day() {
        let now = Timestamp::from_secs(1_000_000);

        assert_eq!(
            catch_up_since(now),
            Timestamp::from_secs(1_000_000 - 86_400)
        );
    }

    #[test]
    fn the_trusted_key_follows_the_node_and_survives_silence() {
        let current = Keys::generate().public_key();
        let announced = Keys::generate().public_key();

        assert_eq!(
            next_serbero(Some(current), Discovery::Serbero(announced)),
            Some(announced)
        );
        assert_eq!(
            next_serbero(Some(current), Discovery::NotFound),
            Some(current)
        );
        assert_eq!(next_serbero(Some(current), Discovery::NoSerbero), None);
        assert_eq!(next_serbero(None, Discovery::NotFound), None);
    }

    #[tokio::test]
    async fn trusts_the_configured_key_and_catches_up_on_the_last_day() {
        let setup = Setup::new().await;
        let now = Timestamp::now();
        let recent = setup.dm(&format!("Dispute {DISPUTE} · mediating"), now - 3_600u64);
        let too_old = setup.dm(
            &format!("Dispute {DISPUTE} · handed off: flood"),
            now - 25 * 3_600u64,
        );
        let to_someone_else = serbero_dm(
            &setup.serbero,
            Keys::generate().public_key(),
            Some(DISPUTE),
            "x",
            None,
        );
        let from_a_stranger = serbero_dm(
            &Keys::generate(),
            setup.watchdog.public_key(),
            Some(DISPUTE),
            "x",
            None,
        );
        for event in [&recent, &too_old, &to_someone_else, &from_a_stranger] {
            setup.publish(event).await;
        }
        let (sync, mut backlog) = setup.sync(Some(setup.serbero.public_key()), None);

        sync.sync_once().await;

        assert_eq!(*sync.trusted.read().await, Some(setup.serbero.public_key()));
        let caught_up = backlog.try_recv().expect("a backlog batch");
        assert_eq!(caught_up, vec![recent]);
        assert!(!setup
            .client
            .subscription(&subscription_id(&setup.serbero.public_key()))
            .await
            .is_empty());
        setup.shutdown().await;
    }

    #[test]
    fn each_serbero_key_has_its_own_short_subscription_id() {
        let a = Keys::generate().public_key();
        let b = Keys::generate().public_key();

        assert_ne!(subscription_id(&a), subscription_id(&b));
        assert_eq!(subscription_id(&a), subscription_id(&a));
        assert!(subscription_id(&a).to_string().len() <= 64);
    }

    #[test]
    fn a_relay_that_already_holds_the_subscription_did_not_refuse_it() {
        let held = RelayUrl::parse("wss://held.example").unwrap();
        let refusing = RelayUrl::parse("wss://refusing.example").unwrap();
        let failed = HashMap::from([
            (held, ALREADY_SUBSCRIBED.to_string()),
            (refusing.clone(), "blocked: not allowed".to_string()),
        ]);

        assert_eq!(refusals(&failed), vec![(&refusing, "blocked: not allowed")]);
    }

    #[tokio::test]
    async fn a_new_serbero_key_replaces_the_live_subscription() {
        // Discovery or a config change moves trust from one key to another.
        let setup = Setup::new().await;
        let old_serbero = Keys::generate();
        let (first, _backlog) = setup.sync(Some(old_serbero.public_key()), None);
        first.sync_once().await;
        let (second, _backlog) = setup.sync(Some(setup.serbero.public_key()), None);
        let second = SerberoSync {
            trusted: first.trusted.clone(),
            ..second
        };
        let mut notifications = setup.client.notifications();

        second.sync_once().await;

        assert!(setup
            .client
            .subscription(&subscription_id(&old_serbero.public_key()))
            .await
            .is_empty());
        let live = setup.dm(&format!("Dispute {DISPUTE} · mediating"), Timestamp::now());
        setup.publish(&live).await;
        let (subscription, event) = next_dm(&mut notifications).await;
        assert_eq!(subscription, subscription_id(&setup.serbero.public_key()));
        assert_eq!(event, live);
        setup.shutdown().await;
    }

    #[tokio::test]
    async fn sending_the_same_subscription_again_keeps_it() {
        let setup = Setup::new().await;
        let (sync, _backlog) = setup.sync(Some(setup.serbero.public_key()), None);
        let mut notifications = setup.client.notifications();

        sync.sync_once().await;
        sync.sync_once().await;

        let live = setup.dm(&format!("Dispute {DISPUTE} · mediating"), Timestamp::now());
        setup.publish(&live).await;
        let (_, event) = next_dm(&mut notifications).await;
        assert_eq!(event, live);
        setup.shutdown().await;
    }

    #[tokio::test]
    async fn live_dms_arrive_on_the_keys_subscription() {
        let setup = Setup::new().await;
        let (sync, _backlog) = setup.sync(Some(setup.serbero.public_key()), None);
        let mut notifications = setup.client.notifications();
        sync.sync_once().await;

        let live = setup.dm(&format!("Dispute {DISPUTE} · mediating"), Timestamp::now());
        setup.publish(&live).await;

        let (subscription, event) = next_dm(&mut notifications).await;
        assert_eq!(subscription, subscription_id(&setup.serbero.public_key()));
        assert_eq!(event, live);
        setup.shutdown().await;
    }

    #[tokio::test]
    async fn discovers_the_key_from_the_nodes_info_event() {
        let setup = Setup::new().await;
        let hex = setup.serbero.public_key().to_hex();
        setup.publish_info(&[&["serbero", hex.as_str()]]).await;
        let (sync, _backlog) = setup.sync(None, None);

        sync.sync_once().await;

        assert_eq!(*sync.trusted.read().await, Some(setup.serbero.public_key()));
        assert!(!setup
            .client
            .subscription(&subscription_id(&setup.serbero.public_key()))
            .await
            .is_empty());
        setup.shutdown().await;
    }

    #[tokio::test]
    async fn stops_trusting_serbero_once_the_node_names_none() {
        let setup = Setup::new().await;
        let hex = setup.serbero.public_key().to_hex();
        setup.publish_info(&[&["serbero", hex.as_str()]]).await;
        let (sync, _backlog) = setup.sync(None, None);
        sync.sync_once().await;
        tokio::time::sleep(Duration::from_secs(1)).await; // a newer created_at
        setup.publish_info(&[&["pow", "0"]]).await;

        sync.sync_once().await;

        assert_eq!(*sync.trusted.read().await, None);
        assert!(setup
            .client
            .subscription(&subscription_id(&setup.serbero.public_key()))
            .await
            .is_empty());
        setup.shutdown().await;
    }

    #[tokio::test]
    async fn without_a_key_nothing_is_subscribed() {
        let setup = Setup::new().await;
        let (sync, mut backlog) = setup.sync(None, None);

        sync.sync_once().await;

        assert_eq!(*sync.trusted.read().await, None);
        assert!(setup
            .client
            .subscription(&subscription_id(&setup.serbero.public_key()))
            .await
            .is_empty());
        assert!(backlog.try_recv().is_err());
        setup.shutdown().await;
    }

    #[tokio::test]
    async fn live_dms_flow_while_the_catch_up_waits_for_a_silent_relay() {
        let healthy = MockRelay::run().await.unwrap();
        let silent = MockRelay::run_with_opts(LocalRelayTestOptions {
            unresponsive_connection: Some(Duration::from_secs(120)),
            ..Default::default()
        })
        .await
        .unwrap();
        let healthy_url = healthy.url().await.to_string();
        let client = connect(&[healthy_url.clone(), silent.url().await.to_string()]).await;
        let publisher = connect(&[healthy_url]).await;
        let serbero = Keys::generate();
        let watchdog = Keys::generate();
        let (backlog, _received) = mpsc::unbounded_channel();
        let sync = SerberoSync {
            client: client.clone(),
            watchdog: watchdog.public_key(),
            mostro: Keys::generate().public_key(),
            configured: Some(serbero.public_key()),
            trusted: Arc::new(RwLock::new(None)),
            backlog,
            relays_changed: Arc::new(Notify::new()),
            refresh: Duration::from_secs(3600),
        };
        let mut notifications = client.notifications();
        let task = sync.spawn();
        // The subscription goes out before the catch-up starts waiting.
        tokio::time::sleep(Duration::from_millis(500)).await;

        let published = Instant::now();
        let live = serbero_dm(
            &serbero,
            watchdog.public_key(),
            Some(DISPUTE),
            &format!("Dispute {DISPUTE} · mediating"),
            None,
        );
        publisher.send_event(&live).await.unwrap();

        let (_, event) = next_dm(&mut notifications).await;
        assert_eq!(event, live);
        assert!(
            published.elapsed() < Duration::from_secs(3),
            "the live DM waited {:?}",
            published.elapsed()
        );
        task.abort();
        client.shutdown().await;
        publisher.shutdown().await;
    }
}
