//! An independent block height to compare the instance's LND with: on
//! 2026-10-14 the daemon's own height was the problem, so it can't vouch
//! for itself.
//!
//! Two references, only ever within the instance's `lnd_network`:
//! - public explorers for that network (the highest answer wins);
//! - the median height of the instances the operator trusts
//!   (`trusted_peers`) on the same network, from their fresh health
//!   events, when at least [`MIN_PEERS`] are available. Never every
//!   discovered instance: anyone can sign an event with a made-up height.
//!
//! With neither (for example on regtest), the cross-check is skipped.

use std::time::Duration;

use nostr_sdk::prelude::*;
use tracing::{debug, warn};

use super::event::{parse_health, EscrowMode, HealthReport};
use super::rules::Reference;

/// Fewest other instances whose median is trusted.
pub const MIN_PEERS: usize = 3;

/// How long an explorer gets to answer.
pub const EXPLORER_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest answer read from an explorer: a height is a few digits.
const MAX_ANSWER_BYTES: usize = 32;

/// The median of `heights` (the lower one of the middle two when even).
pub fn median(mut heights: Vec<u64>) -> Option<u64> {
    if heights.is_empty() {
        return None;
    }
    heights.sort_unstable();
    Some(heights[(heights.len() - 1) / 2])
}

/// The valid health events of `peer` among `events`, newest first by
/// nothing in particular: [`newest`] picks among them against the
/// revision already accepted, so an older one a relay still serves is
/// never taken over it.
pub fn peer_reports(events: &[Event], peer: PublicKey, now: u64) -> Vec<HealthReport> {
    events
        .iter()
        .filter(|event| event.pubkey == peer)
        .filter_map(|event| parse_health(event, peer, now).ok())
        .collect()
}

/// Heights on `network` of the trusted instances' current revisions that
/// are still fresh.
pub fn peer_heights<'a>(
    current: impl IntoIterator<Item = &'a HealthReport>,
    network: &str,
    now: u64,
) -> Vec<u64> {
    current
        .into_iter()
        .filter(|r| r.envelope.is_fresh(now))
        .filter(|r| {
            r.escrow_mode == EscrowMode::Lightning && r.lnd_network.as_deref() == Some(network)
        })
        .filter_map(|r| r.lnd_block_height)
        .collect()
}

/// Reads an explorer's plain-text answer.
pub fn parse_tip(body: &str) -> Option<u64> {
    let body = body.trim();
    if body.is_empty() || body.len() > MAX_ANSWER_BYTES || !body.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    body.parse().ok().filter(|height| *height > 0)
}

/// The host of `url`, to name the source in messages.
pub fn source_name(url: &str) -> String {
    url.split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or(url)
        .to_string()
}

/// Asks every explorer in `urls` and returns the highest tip, with the
/// explorer that gave it. `None` when none answered.
pub async fn explorer_tip(http: &reqwest::Client, urls: &[String]) -> Option<(u64, String)> {
    let mut answers = tokio::task::JoinSet::new();
    for url in urls {
        let http = http.clone();
        let url = url.clone();
        answers.spawn(async move {
            let result = async {
                let response = http
                    .get(&url)
                    .timeout(EXPLORER_TIMEOUT)
                    .send()
                    .await?
                    .error_for_status()?;
                read_capped(response, MAX_ANSWER_BYTES).await
            }
            .await;
            (url, result)
        });
    }
    let mut best: Option<(u64, String)> = None;
    while let Some(joined) = answers.join_next().await {
        let Ok((url, result)) = joined else {
            continue;
        };
        let tip = result.map(|body| {
            body.and_then(|bytes| String::from_utf8(bytes).ok())
                .and_then(|text| parse_tip(&text))
        });
        match tip {
            Ok(Some(height)) => {
                debug!(%url, height, "Explorer tip");
                if best.as_ref().is_none_or(|(h, _)| height > *h) {
                    best = Some((height, source_name(&url)));
                }
            }
            Ok(None) => warn!(%url, "Explorer answered something that is not a block height"),
            Err(e) => warn!(%url, error = %e, "Explorer did not answer"),
        }
    }
    best
}

/// Reads at most `max` bytes of `response`, chunk by chunk. `None` when the
/// body is longer: an explorer answer is a few digits, and a large body is
/// never buffered whole.
async fn read_capped(
    mut response: reqwest::Response,
    max: usize,
) -> Result<Option<Vec<u8>>, reqwest::Error> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > max {
            return Ok(None);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(Some(body))
}

/// The reference to compare with: the highest of the explorer tip and the
/// peer median, whichever are available.
pub fn reference(explorer: Option<(u64, String)>, peers: &[u64]) -> Option<Reference> {
    let peer = (peers.len() >= MIN_PEERS)
        .then(|| median(peers.to_vec()))
        .flatten()
        .map(|height| Reference {
            height,
            source: format!("the median of {} trusted instances", peers.len()),
        });
    let explorer = explorer.map(|(height, source)| Reference { height, source });
    match (explorer, peer) {
        (Some(e), Some(p)) => Some(if p.height > e.height { p } else { e }),
        (e, p) => e.or(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::event::tests::{health_tags, set_tag, sign, NOW};

    fn instance(keys: &Keys, height: &str, network: &str, created_at: u64) -> Event {
        let mut tags = health_tags(keys, created_at, 1);
        set_tag(&mut tags, "lnd_block_height", &[height]);
        set_tag(&mut tags, "lnd_network", &[network]);
        sign(keys, tags, "", created_at)
    }

    #[test]
    fn the_median_is_the_middle_height() {
        assert_eq!(median(vec![]), None);
        assert_eq!(median(vec![5, 1, 9]), Some(5));
        assert_eq!(median(vec![4, 1, 9, 7]), Some(4));
    }

    #[test]
    fn peer_heights_only_count_fresh_reports_on_the_same_network() {
        let (a, b, c, d) = (
            Keys::generate(),
            Keys::generate(),
            Keys::generate(),
            Keys::generate(),
        );
        let reports: Vec<HealthReport> = [
            (&a, "971485", "mainnet", NOW),
            (&b, "971486", "mainnet", NOW - 30),
            (&c, "2000", "signet", NOW),
            (&d, "971000", "mainnet", NOW - 1000),
        ]
        .into_iter()
        .map(|(keys, height, network, at)| {
            parse_health(&instance(keys, height, network, at), keys.public_key(), NOW).unwrap()
        })
        .collect();

        let mut heights = peer_heights(&reports, "mainnet", NOW);
        heights.sort();

        assert_eq!(heights, vec![971_485, 971_486]);
    }

    #[test]
    fn peer_reports_keep_only_the_peers_valid_events() {
        let peer = Keys::generate();
        let stranger = Keys::generate();
        let events = vec![
            instance(&peer, "971485", "mainnet", NOW),
            instance(&stranger, "999999", "mainnet", NOW),
        ];

        let reports = peer_reports(&events, peer.public_key(), NOW);

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].lnd_block_height, Some(971_485));
    }

    #[test]
    fn explorer_answers_must_be_a_plain_height() {
        assert_eq!(parse_tip("971485\n"), Some(971_485));
        for body in ["", "0", "-5", "971485.0", "<html>", &"9".repeat(40)] {
            assert_eq!(parse_tip(body), None, "{body:?}");
        }
    }

    /// A one-shot HTTP server answering `body`, for the explorer client.
    async fn serve(body: Vec<u8>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/tip", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(&body).await;
        });
        url
    }

    #[tokio::test]
    async fn an_explorer_answer_is_read_up_to_a_few_bytes_only() {
        let http = reqwest::Client::new();

        let small = serve(b"971485".to_vec()).await;
        assert_eq!(
            explorer_tip(&http, &[small]).await.map(|(h, _)| h),
            Some(971_485)
        );

        // A megabyte of digits: refused without being read whole.
        let huge = serve(vec![b'9'; 1 << 20]).await;
        assert_eq!(explorer_tip(&http, &[huge]).await, None);
    }

    #[test]
    fn sources_are_named_by_host() {
        assert_eq!(
            source_name("https://mempool.space/api/blocks/tip/height"),
            "mempool.space"
        );
    }

    #[test]
    fn the_reference_is_the_highest_available_and_peers_need_a_quorum() {
        let explorer = Some((971_485, "mempool.space".to_string()));

        assert_eq!(reference(None, &[971_485, 971_486]), None);
        assert_eq!(reference(explorer.clone(), &[]).unwrap().height, 971_485);
        let peers = [971_486, 971_486, 971_487];
        let both = reference(explorer, &peers).unwrap();
        assert_eq!(both.height, 971_486);
        assert_eq!(both.source, "the median of 3 trusted instances");
        assert_eq!(reference(None, &peers).unwrap().height, 971_486);
    }
}
