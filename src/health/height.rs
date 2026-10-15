//! An independent block height to compare the instance's LND with: on
//! 2026-10-14 the daemon's own height was the problem, so it can't vouch
//! for itself.
//!
//! Two references, only ever within the instance's `lnd_network`:
//! - public explorers for that network (the highest answer wins);
//! - the median height of other instances on the same network, from their
//!   fresh health events, when at least [`MIN_PEERS`] are available.
//!
//! With neither (for example on regtest), the cross-check is skipped.

use std::collections::HashMap;
use std::time::Duration;

use nostr_sdk::prelude::*;
use tracing::{debug, warn};

use super::event::{newest, parse_any_health, EscrowMode, HealthReport};
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

/// Heights of other instances on `network`, from their fresh health
/// events, one per instance (its highest `seq`).
pub fn peer_heights(events: &[Event], exclude: PublicKey, network: &str, now: u64) -> Vec<u64> {
    let mut by_instance: HashMap<PublicKey, Vec<HealthReport>> = HashMap::new();
    for report in events
        .iter()
        .filter_map(|event| parse_any_health(event, now).ok())
    {
        if report.envelope.pubkey != exclude && report.envelope.is_fresh(now) {
            by_instance
                .entry(report.envelope.pubkey)
                .or_default()
                .push(report);
        }
    }
    by_instance
        .values()
        .filter_map(|reports| newest(reports, |r| &r.envelope, None))
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
                response.text().await
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
        match result.map(|body| parse_tip(&body)) {
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

/// The reference to compare with: the highest of the explorer tip and the
/// peer median, whichever are available.
pub fn reference(explorer: Option<(u64, String)>, peers: &[u64]) -> Option<Reference> {
    let peer = (peers.len() >= MIN_PEERS)
        .then(|| median(peers.to_vec()))
        .flatten()
        .map(|height| Reference {
            height,
            source: format!("the median of {} other instances", peers.len()),
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

    fn instance(height: &str, network: &str, created_at: u64) -> Event {
        let keys = Keys::generate();
        let mut tags = health_tags(&keys, created_at, 1);
        set_tag(&mut tags, "lnd_block_height", &[height]);
        set_tag(&mut tags, "lnd_network", &[network]);
        sign(&keys, tags, "", created_at)
    }

    #[test]
    fn the_median_is_the_middle_height() {
        assert_eq!(median(vec![]), None);
        assert_eq!(median(vec![5, 1, 9]), Some(5));
        assert_eq!(median(vec![4, 1, 9, 7]), Some(4));
    }

    #[test]
    fn peer_heights_only_count_fresh_instances_on_the_same_network() {
        let me = Keys::generate();
        let mine = sign(&me, health_tags(&me, NOW, 1), "", NOW);
        let events = vec![
            instance("971485", "mainnet", NOW),
            instance("971486", "mainnet", NOW - 30),
            instance("2000", "signet", NOW),
            instance("971000", "mainnet", NOW - 1000),
            mine,
        ];

        let mut heights = peer_heights(&events, me.public_key(), "mainnet", NOW);
        heights.sort();

        assert_eq!(heights, vec![971_485, 971_486]);
    }

    #[test]
    fn explorer_answers_must_be_a_plain_height() {
        assert_eq!(parse_tip("971485\n"), Some(971_485));
        for body in ["", "0", "-5", "971485.0", "<html>", &"9".repeat(40)] {
            assert_eq!(parse_tip(body), None, "{body:?}");
        }
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
        assert_eq!(both.source, "the median of 3 other instances");
        assert_eq!(reference(None, &peers).unwrap().height, 971_486);
    }
}
