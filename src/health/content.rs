//! The encrypted `content` of a health event: a NIP-44 v2 JSON payload with
//! operational detail (liquidity, payout counts, HTLC expiry) addressed to
//! the operator's watchdog.
//!
//! A reader ignores fields it doesn't know, ignores the whole payload when
//! it can't decrypt or parse it or doesn't know its version, and never
//! trusts it over the public tags.

use std::collections::BTreeMap;

use nostr_sdk::prelude::*;
use serde::Deserialize;

use super::event::{EscrowMode, HealthReport};

/// The payload version this watchdog reads.
pub const SUPPORTED_VERSION: u64 = 1;

/// Largest plaintext accepted, per the spec.
pub const MAX_PLAINTEXT_BYTES: usize = 32 * 1024;

/// Largest encrypted payload worth decrypting: a 32 KiB plaintext padded
/// and base64-encoded stays well under this.
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// Version 1 of the payload. Every field is required by the spec, but a
/// missing one reads as `None` ("unknown") rather than failing the parse:
/// `null` already means "I don't know".
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct HealthDetails {
    pub v: Option<u64>,
    pub generated_at: Option<u64>,
    pub lnd: Option<LndDetails>,
    pub payouts: Option<PayoutDetails>,
    pub dev_fee: Option<DevFeeDetails>,
    pub relays: Option<RelayDetails>,
    pub maintenance: Option<MaintenanceDetails>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct LndDetails {
    pub reachable: Option<bool>,
    pub synced_to_chain: Option<bool>,
    pub unsynced_since: Option<u64>,
    pub block_height: Option<u64>,
    pub best_header_timestamp: Option<u64>,
    pub peers: Option<u64>,
    pub channels_active: Option<u64>,
    pub channels_inactive: Option<u64>,
    pub channels_pending: Option<u64>,
    pub outbound_liquidity_sats: Option<u64>,
    pub inbound_liquidity_sats: Option<u64>,
    pub pending_htlcs: Option<u64>,
    /// Can be zero or negative: an HTLC already at its expiry.
    pub nearest_htlc_expiry_blocks: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct PayoutDetails {
    pub window_secs: Option<u64>,
    pub payouts_succeeded_1h: Option<u64>,
    pub payouts_failed_1h: Option<u64>,
    /// Failed attempts by reason. A reason this version doesn't know (from
    /// a newer LND or daemon) is kept as is; it never fails the payload.
    pub failure_reasons_1h: BTreeMap<String, u64>,
    pub last_payout_success_at: Option<u64>,
    pub settled_unpaid_orders: Option<u64>,
    pub oldest_settled_unpaid_order_age_secs: Option<u64>,
    pub awaiting_buyer_invoice_orders: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct DevFeeDetails {
    pub pending: Option<u64>,
    pub failed_1h: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct RelayDetails {
    pub configured: Option<u64>,
    pub connected: Option<u64>,
    /// When the daemon's last outage (no relay connected) began and ended.
    pub last_outage_started_at: Option<u64>,
    pub last_outage_ended_at: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct MaintenanceDetails {
    pub enabled: Option<bool>,
    pub reason: Option<String>,
}

/// What reading the content gave. Only `Details` is used; the others say
/// why the public tags are all there is.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    /// The event has no content.
    Empty,
    /// Addressed to another key, or the watchdog has no key.
    NotForUs,
    /// Didn't decrypt, wasn't UTF-8 JSON of the right shape, or too large.
    Unreadable,
    /// A version this watchdog doesn't know.
    UnknownVersion(Option<u64>),
    /// Decrypted, but it contradicts the public tags.
    Inconsistent,
    Details(Box<HealthDetails>),
}

/// Reads the content of `report` with the watchdog's `keys`.
pub fn read(report: &HealthReport, keys: Option<&Keys>) -> Content {
    if report.content.is_empty() {
        return Content::Empty;
    }
    let Some(keys) = keys else {
        return Content::NotForUs;
    };
    if report.recipient != Some(keys.public_key()) {
        return Content::NotForUs;
    }
    if report.content.len() > MAX_PAYLOAD_BYTES {
        return Content::Unreadable;
    }
    let Ok(plaintext) = nip44::decrypt(keys.secret_key(), &report.envelope.pubkey, &report.content)
    else {
        return Content::Unreadable;
    };
    parse(&plaintext, report)
}

/// Parses a decrypted payload and checks it against the public tags.
pub fn parse(plaintext: &str, report: &HealthReport) -> Content {
    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Content::Unreadable;
    }
    let Ok(details) = serde_json::from_str::<HealthDetails>(plaintext) else {
        return Content::Unreadable;
    };
    if details.v != Some(SUPPORTED_VERSION) {
        return Content::UnknownVersion(details.v);
    }
    // `null` is for a cashu instance, which has no LND and pays out no
    // Lightning invoice. A Lightning payload without them is malformed.
    if report.escrow_mode == EscrowMode::Lightning
        && (details.lnd.is_none() || details.payouts.is_none())
    {
        return Content::Unreadable;
    }
    if !agrees_with_tags(&details, report) {
        return Content::Inconsistent;
    }
    Content::Details(Box::new(details))
}

/// Whether the payload says the same as the public tags where they
/// overlap. They are produced together; a mismatch is a publisher bug,
/// and the tags win.
fn agrees_with_tags(details: &HealthDetails, report: &HealthReport) -> bool {
    let maintenance_agrees = details
        .maintenance
        .as_ref()
        .and_then(|m| m.enabled)
        .is_none_or(|enabled| enabled == report.maintenance_mode);
    let lnd_agrees = details.lnd.as_ref().is_none_or(|lnd| {
        let height = lnd
            .block_height
            .is_none_or(|h| report.lnd_block_height == Some(h));
        let synced = lnd
            .synced_to_chain
            .is_none_or(|s| report.lnd_synced_to_chain == Some(s));
        height && synced
    });
    maintenance_agrees && lnd_agrees
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::health::event::parse_health;
    use crate::health::event::tests::{health_tags, set_tag, sign, NOW};

    /// The spec's example payload, for a report at height 971485, synced.
    pub fn payload() -> serde_json::Value {
        serde_json::json!({
            "v": 1,
            "generated_at": NOW,
            "lnd": {
                "reachable": true,
                "synced_to_chain": true,
                "unsynced_since": null,
                "block_height": 971485,
                "best_header_timestamp": NOW - 30,
                "peers": 19,
                "channels_active": 8,
                "channels_inactive": 1,
                "channels_pending": 0,
                "outbound_liquidity_sats": 52341870,
                "inbound_liquidity_sats": 13874104,
                "pending_htlcs": 37,
                "nearest_htlc_expiry_blocks": 291
            },
            "payouts": {
                "window_secs": 3600,
                "payouts_succeeded_1h": 3,
                "payouts_failed_1h": 0,
                "failure_reasons_1h": {},
                "last_payout_success_at": NOW - 600,
                "settled_unpaid_orders": 0,
                "oldest_settled_unpaid_order_age_secs": null,
                "awaiting_buyer_invoice_orders": 0
            },
            "dev_fee": { "pending": 0, "failed_1h": 0 },
            "relays": {
                "configured": 4,
                "connected": 4,
                "last_outage_started_at": null,
                "last_outage_ended_at": null
            },
            "maintenance": { "enabled": false, "reason": null }
        })
    }

    /// A health event from `mostro` whose content is `plaintext` encrypted
    /// to `watchdog`, with tags edited by `edit`.
    pub fn encrypted_event(
        mostro: &Keys,
        watchdog: &PublicKey,
        plaintext: &str,
        seq: u64,
        edit: impl FnOnce(&mut Vec<Vec<String>>),
    ) -> Event {
        let mut tags = health_tags(mostro, NOW, seq);
        set_tag(&mut tags, "p", &[&watchdog.to_hex()]);
        edit(&mut tags);
        let content =
            nip44::encrypt(mostro.secret_key(), watchdog, plaintext, nip44::Version::V2).unwrap();
        sign(mostro, tags, &content, NOW)
    }

    fn report(event: &Event, mostro: &Keys) -> HealthReport {
        parse_health(event, mostro.public_key(), NOW).unwrap()
    }

    #[test]
    fn the_payload_decrypts_with_the_watchdog_key() {
        let mostro = Keys::generate();
        let watchdog = Keys::generate();
        let event = encrypted_event(
            &mostro,
            &watchdog.public_key(),
            &payload().to_string(),
            1,
            |_| {},
        );

        let Content::Details(details) = read(&report(&event, &mostro), Some(&watchdog)) else {
            panic!("expected details");
        };

        let lnd = details.lnd.unwrap();
        assert_eq!(lnd.block_height, Some(971_485));
        assert_eq!(lnd.nearest_htlc_expiry_blocks, Some(291));
        assert_eq!(details.payouts.unwrap().payouts_succeeded_1h, Some(3));
    }

    #[test]
    fn content_for_another_key_or_without_a_key_is_not_read() {
        let mostro = Keys::generate();
        let watchdog = Keys::generate();
        let other = Keys::generate();
        let event = encrypted_event(
            &mostro,
            &watchdog.public_key(),
            &payload().to_string(),
            1,
            |_| {},
        );
        let report = report(&event, &mostro);

        assert_eq!(read(&report, Some(&other)), Content::NotForUs);
        assert_eq!(read(&report, None), Content::NotForUs);
    }

    #[test]
    fn an_unknown_version_falls_back_to_the_tags() {
        let mostro = Keys::generate();
        let watchdog = Keys::generate();
        let mut body = payload();
        body["v"] = 2.into();
        let event = encrypted_event(
            &mostro,
            &watchdog.public_key(),
            &body.to_string(),
            1,
            |_| {},
        );

        assert_eq!(
            read(&report(&event, &mostro), Some(&watchdog)),
            Content::UnknownVersion(Some(2))
        );
    }

    #[test]
    fn malformed_json_or_a_wrong_type_is_unreadable() {
        let mostro = Keys::generate();
        let watchdog = Keys::generate();
        let mut wrong_type = payload();
        wrong_type["lnd"]["peers"] = "many".into();

        for plaintext in ["not json".to_string(), wrong_type.to_string()] {
            let event = encrypted_event(&mostro, &watchdog.public_key(), &plaintext, 1, |_| {});
            assert_eq!(
                read(&report(&event, &mostro), Some(&watchdog)),
                Content::Unreadable
            );
        }
    }

    #[test]
    fn unknown_fields_and_unknown_failure_reasons_are_accepted() {
        let mostro = Keys::generate();
        let watchdog = Keys::generate();
        let mut body = payload();
        body["future_field"] = serde_json::json!({"x": 1});
        body["payouts"]["failure_reasons_1h"] = serde_json::json!({
            "no_route": 2,
            "quantum_decoherence": 1
        });
        let event = encrypted_event(
            &mostro,
            &watchdog.public_key(),
            &body.to_string(),
            1,
            |_| {},
        );

        let Content::Details(details) = read(&report(&event, &mostro), Some(&watchdog)) else {
            panic!("expected details");
        };

        let reasons = details.payouts.unwrap().failure_reasons_1h;
        assert_eq!(reasons.get("no_route"), Some(&2));
        assert_eq!(reasons.get("quantum_decoherence"), Some(&1));
    }

    #[test]
    fn a_payload_that_contradicts_the_tags_is_not_trusted() {
        let mostro = Keys::generate();
        let watchdog = Keys::generate();
        let mut body = payload();
        body["lnd"]["block_height"] = 971_000.into();
        let event = encrypted_event(
            &mostro,
            &watchdog.public_key(),
            &body.to_string(),
            1,
            |_| {},
        );

        assert_eq!(
            read(&report(&event, &mostro), Some(&watchdog)),
            Content::Inconsistent
        );
    }

    #[test]
    fn a_cashu_payload_with_null_lightning_objects_is_accepted() {
        let mostro = Keys::generate();
        let watchdog = Keys::generate();
        let mut body = payload();
        body["lnd"] = serde_json::Value::Null;
        body["payouts"] = serde_json::Value::Null;
        body["dev_fee"] = serde_json::Value::Null;
        let event = encrypted_event(
            &mostro,
            &watchdog.public_key(),
            &body.to_string(),
            1,
            |tags| {
                set_tag(tags, "escrow_mode", &["cashu"]);
                for name in ["lnd_block_height", "lnd_synced_to_chain", "lnd_network"] {
                    set_tag(tags, name, &[]);
                }
                tags.retain(|t| t[0] != "check" || (t[1] != "lnd_sync" && t[1] != "payouts"));
            },
        );

        let Content::Details(details) = read(&report(&event, &mostro), Some(&watchdog)) else {
            panic!("expected details");
        };

        assert_eq!(details.lnd, None);
        assert_eq!(details.payouts, None);
        assert_eq!(details.relays.unwrap().connected, Some(4));
    }

    #[test]
    fn a_lightning_payload_with_null_lnd_or_payouts_is_unreadable() {
        let mostro = Keys::generate();
        let watchdog = Keys::generate();
        for field in ["lnd", "payouts"] {
            let mut body = payload();
            body[field] = serde_json::Value::Null;
            let event = encrypted_event(
                &mostro,
                &watchdog.public_key(),
                &body.to_string(),
                1,
                |_| {},
            );

            assert_eq!(
                read(&report(&event, &mostro), Some(&watchdog)),
                Content::Unreadable,
                "{field}"
            );
        }
    }

    #[test]
    fn an_oversized_plaintext_is_unreadable() {
        let mostro = Keys::generate();
        let event = encrypted_event(&mostro, &Keys::generate().public_key(), "{}", 1, |_| {});
        let big = format!(
            "{{\"v\":1,\"pad\":\"{}\"}}",
            "x".repeat(MAX_PLAINTEXT_BYTES)
        );

        assert_eq!(parse(&big, &report(&event, &mostro)), Content::Unreadable);
    }
}
