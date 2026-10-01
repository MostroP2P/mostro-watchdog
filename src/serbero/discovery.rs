//! Finding the node's Serbero in its info event.
//!
//! mostrod announces its Serbero in the instance-info event (kind 38385,
//! addressable, `d` = the node's hex pubkey) as `["serbero", "<hex
//! pubkey>"]`, and leaves the tag out when it has none.
//!
//! One addressable event needs no quorum: the first copy plus a short grace
//! for a newer one is enough, so a slow relay never holds the answer back
//! (ported from Serbero's `src/nostr/first_answer.rs`).

use std::cmp::Reverse;
use std::time::Duration;

use mostro_core::prelude::NOSTR_INFO_EVENT_KIND;
use nostr_sdk::prelude::*;
use tracing::warn;

/// Tag of the info event that names the node's Serbero.
const SERBERO_TAG: &str = "serbero";

/// How long to keep listening after the first copy, in case that relay held
/// a stale one. Relays that answer at all do so within moments.
pub const GRACE: Duration = Duration::from_millis(750);

/// What the node's info event says about its Serbero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Discovery {
    /// No relay answered with the info event.
    NotFound,
    /// The info event names no Serbero.
    NoSerbero,
    /// The node's Serbero.
    Serbero(PublicKey),
}

/// The node's info event, by its address.
pub fn info_filter(mostro: PublicKey) -> Filter {
    Filter::new()
        .kind(Kind::Custom(NOSTR_INFO_EVENT_KIND))
        .author(mostro)
        .identifier(mostro.to_hex())
}

/// Reads the `serbero` tag of an info event. A tag that holds no valid key
/// names no Serbero.
pub fn serbero_from_info(event: &Event) -> Discovery {
    event
        .tags
        .iter()
        .find_map(|tag| match tag.as_slice() {
            [name, value, ..] if name == SERBERO_TAG => Some(value),
            _ => None,
        })
        .and_then(|value| PublicKey::parse(value).ok())
        .map_or(Discovery::NoSerbero, Discovery::Serbero)
}

/// Reads the node's Serbero from the newest copy of its info event, without
/// waiting for slow relays.
pub async fn discover(client: &Client, mostro: PublicKey, timeout: Duration) -> Discovery {
    let answers = match client
        .stream_events(info_filter(mostro))
        .timeout(timeout)
        .await
    {
        Ok(answers) => answers,
        Err(e) => {
            warn!(error = %e, "Failed to request the Mostro node's info event");
            return Discovery::NotFound;
        }
    };
    let events = answers.filter_map(|(_relay, answer)| async move { answer.ok() });
    match newest_answer(Box::pin(events), GRACE, rank).await {
        Some(event) => serbero_from_info(&event),
        None => Discovery::NotFound,
    }
}

/// NIP-01 order of addressable events: newer first, then the lowest id.
pub fn rank(event: &Event) -> (Timestamp, Reverse<EventId>) {
    (event.created_at, Reverse(event.id))
}

/// The best of `answers` by `rank`: the first one, plus whatever else
/// arrives within `grace` of it. `None` when the stream ends empty.
pub async fn newest_answer<T, K: Ord>(
    mut answers: impl StreamExt<Item = T> + Unpin,
    grace: Duration,
    rank: impl Fn(&T) -> K,
) -> Option<T> {
    let mut newest = answers.next().await?;
    // Elapsing is the expected way out: it means a relay is still silent.
    let _ = tokio::time::timeout(grace, async {
        while let Some(answer) = answers.next().await {
            if rank(&answer) > rank(&newest) {
                newest = answer;
            }
        }
    })
    .await;
    Some(newest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use std::future::pending;
    use std::time::Instant;

    fn info_event(mostro: &Keys, tags: &[&[&str]], created_at: Timestamp) -> Event {
        let address = mostro.public_key().to_hex();
        let tags = std::iter::once(Tag::identifier(address))
            .chain(tags.iter().map(|t| Tag::parse(t.iter().copied()).unwrap()));
        EventBuilder::new(Kind::Custom(NOSTR_INFO_EVENT_KIND), "")
            .tags(tags)
            .custom_created_at(created_at)
            .finalize(mostro)
            .unwrap()
    }

    async fn client_for(urls: &[String]) -> Client {
        let client = Client::default();
        for url in urls {
            client.add_relay(url).await.unwrap();
        }
        client.connect().await;
        client
    }

    #[test]
    fn reads_the_serbero_tag() {
        let mostro = Keys::generate();
        let serbero = Keys::generate().public_key();
        let hex = serbero.to_hex();
        let event = info_event(
            &mostro,
            &[&["pow", "0"], &["serbero", hex.as_str()]],
            Timestamp::now(),
        );

        assert_eq!(serbero_from_info(&event), Discovery::Serbero(serbero));
    }

    #[test]
    fn a_node_without_serbero_has_no_tag() {
        let event = info_event(&Keys::generate(), &[&["pow", "0"]], Timestamp::now());

        assert_eq!(serbero_from_info(&event), Discovery::NoSerbero);
    }

    #[test]
    fn a_malformed_tag_names_no_serbero() {
        for tags in [&[&["serbero", "nope"][..]][..], &[&["serbero"][..]]] {
            let event = info_event(&Keys::generate(), tags, Timestamp::now());

            assert_eq!(serbero_from_info(&event), Discovery::NoSerbero, "{tags:?}");
        }
    }

    #[test]
    fn the_filter_asks_for_the_nodes_own_info_event() {
        let mostro = Keys::generate().public_key();

        let json = info_filter(mostro).as_json();

        assert!(json.contains("\"kinds\":[38385]"), "{json}");
        assert!(
            json.contains(&format!("\"authors\":[\"{}\"]", mostro.to_hex())),
            "{json}"
        );
        assert!(
            json.contains(&format!("\"#d\":[\"{}\"]", mostro.to_hex())),
            "{json}"
        );
    }

    #[test]
    fn newer_events_rank_higher_and_ties_go_to_the_lowest_id() {
        let mostro = Keys::generate();
        let old = info_event(&mostro, &[], Timestamp::from_secs(10));
        let new = info_event(&mostro, &[], Timestamp::from_secs(20));
        let twin = info_event(&mostro, &[&["pow", "1"]], Timestamp::from_secs(20));

        assert!(rank(&new) > rank(&old));
        let lowest = if new.id < twin.id { &new } else { &twin };
        let highest = if new.id < twin.id { &twin } else { &new };
        assert!(rank(lowest) > rank(highest));
    }

    #[tokio::test(start_paused = true)]
    async fn returns_after_the_grace_while_a_source_stays_silent() {
        let answers = stream::iter([1]).chain(stream::once(pending::<i32>()));

        assert_eq!(
            newest_answer(Box::pin(answers), GRACE, |n| *n).await,
            Some(1)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_better_answer_within_the_grace_wins() {
        let answers = stream::iter([1, 3, 2]);

        assert_eq!(newest_answer(answers, GRACE, |n| *n).await, Some(3));
    }

    #[tokio::test(start_paused = true)]
    async fn no_answer_is_none() {
        let answers = stream::iter(Vec::<i32>::new());

        assert_eq!(newest_answer(answers, GRACE, |n| *n).await, None);
    }

    #[tokio::test]
    async fn discovers_serbero_without_waiting_for_a_silent_relay() {
        let healthy = MockRelay::run().await.unwrap();
        let silent = MockRelay::run_with_opts(LocalRelayTestOptions {
            unresponsive_connection: Some(Duration::from_secs(60)),
            ..Default::default()
        })
        .await
        .unwrap();
        let healthy_url = healthy.url().await.to_string();
        let mostro = Keys::generate();
        let serbero = Keys::generate().public_key();
        let hex = serbero.to_hex();
        let publisher = client_for(std::slice::from_ref(&healthy_url)).await;
        publisher
            .send_event(&info_event(
                &mostro,
                &[&["serbero", hex.as_str()]],
                Timestamp::now(),
            ))
            .await
            .unwrap();
        let client = client_for(&[healthy_url, silent.url().await.to_string()]).await;

        let started = Instant::now();
        let found = discover(&client, mostro.public_key(), Duration::from_secs(20)).await;

        assert_eq!(found, Discovery::Serbero(serbero));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "waited {:?} for the silent relay",
            started.elapsed()
        );
        client.shutdown().await;
        publisher.shutdown().await;
    }

    #[tokio::test]
    async fn the_newest_info_event_wins_across_relays() {
        // The operator removed Serbero; one relay still holds the old event.
        let stale_relay = MockRelay::run().await.unwrap();
        let fresh_relay = MockRelay::run().await.unwrap();
        let stale_url = stale_relay.url().await.to_string();
        let fresh_url = fresh_relay.url().await.to_string();
        let mostro = Keys::generate();
        let old_serbero = Keys::generate().public_key().to_hex();
        let now = Timestamp::now();
        let stale_publisher = client_for(std::slice::from_ref(&stale_url)).await;
        stale_publisher
            .send_event(&info_event(
                &mostro,
                &[&["serbero", old_serbero.as_str()]],
                now - 60u64,
            ))
            .await
            .unwrap();
        let fresh_publisher = client_for(std::slice::from_ref(&fresh_url)).await;
        fresh_publisher
            .send_event(&info_event(&mostro, &[&["pow", "0"]], now))
            .await
            .unwrap();
        let client = client_for(&[stale_url, fresh_url]).await;

        let found = discover(&client, mostro.public_key(), Duration::from_secs(5)).await;

        assert_eq!(found, Discovery::NoSerbero);
        for c in [client, stale_publisher, fresh_publisher] {
            c.shutdown().await;
        }
    }

    #[tokio::test]
    async fn a_node_with_no_info_event_is_not_found() {
        let relay = MockRelay::run().await.unwrap();
        let client = client_for(&[relay.url().await.to_string()]).await;

        let found = discover(
            &client,
            Keys::generate().public_key(),
            Duration::from_secs(5),
        )
        .await;

        assert_eq!(found, Discovery::NotFound);
        client.shutdown().await;
    }
}
