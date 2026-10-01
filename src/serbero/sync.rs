//! Keeping up with Serbero in the background: which key to trust, the live
//! subscription to its DMs, and a catch-up of what was missed. Nothing here
//! holds the event loop back: live DMs reach it as notifications and
//! caught-up ones through the backlog channel.

use std::sync::Arc;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tokio::sync::{mpsc, Notify, RwLock};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;
use tracing::{debug, info, warn};

use super::discovery::{discover, Discovery};

/// Fixed id of the live subscription, so sending it again replaces it on
/// every relay instead of adding another one.
pub const SUBSCRIPTION_ID: &str = "mostro-watchdog-serbero";

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

/// Id of the live subscription.
pub fn subscription_id() -> SubscriptionId {
    SubscriptionId::new(SUBSCRIPTION_ID)
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
            match next {
                Some(serbero) => info!(
                    serbero = %serbero.to_hex(),
                    "🤖 Relaying Serbero's updates to Telegram"
                ),
                None => self.unsubscribe().await,
            }
        }
        next
    }

    /// Sends the live subscription. Sending it again with the same id
    /// replaces it, which restores it on relays that dropped it.
    async fn subscribe(&self, serbero: PublicKey) {
        let filter = dm_filter(serbero, self.watchdog).since(Timestamp::now());
        if let Err(e) = self
            .client
            .subscribe(filter)
            .with_id(subscription_id())
            .await
        {
            warn!(error = %e, "Failed to subscribe to Serbero's DMs");
        }
    }

    async fn unsubscribe(&self) {
        if let Err(e) = self.client.unsubscribe(&subscription_id()).await {
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
            .subscription(&subscription_id())
            .await
            .is_empty());
        setup.shutdown().await;
    }

    #[tokio::test]
    async fn live_dms_arrive_on_the_fixed_subscription() {
        let setup = Setup::new().await;
        let (sync, _backlog) = setup.sync(Some(setup.serbero.public_key()), None);
        let mut notifications = setup.client.notifications();
        sync.sync_once().await;

        let live = setup.dm(&format!("Dispute {DISPUTE} · mediating"), Timestamp::now());
        setup.publish(&live).await;

        let (subscription, event) = next_dm(&mut notifications).await;
        assert_eq!(subscription, subscription_id());
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
            .subscription(&subscription_id())
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
            .subscription(&subscription_id())
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
            .subscription(&subscription_id())
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
