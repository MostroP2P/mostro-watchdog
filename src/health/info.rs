//! Phase 0: Mostro's info event (kind 38385) as a heartbeat. mostrod
//! republishes it every `publish_mostro_info_interval` (300 s by default),
//! so a stale one means mostrod stopped; its `maintenance_mode` tag says
//! whether the instance takes new orders.

use mostro_core::prelude::NOSTR_INFO_EVENT_KIND;
use nostr_sdk::prelude::*;

use super::event::MAX_FUTURE_SKEW_SECS;

/// What the watchdog keeps of the newest info event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InfoSnapshot {
    pub created_at: u64,
    /// `maintenance_mode = "true"`. Daemons that predate the tag omit it,
    /// which reads as `false`.
    pub maintenance_mode: bool,
}

/// Reads a verified info event from `mostro`; `None` for anything else.
pub fn parse_info(event: &Event, mostro: PublicKey, now: u64) -> Option<InfoSnapshot> {
    let address_matches = event.tags.iter().any(|tag| {
        matches!(tag.as_slice(), [name, value, ..] if name == "d" && *value == mostro.to_hex())
    });
    let valid = event.kind == Kind::Custom(NOSTR_INFO_EVENT_KIND)
        && event.pubkey == mostro
        && address_matches
        && event.created_at.as_secs() <= now.saturating_add(MAX_FUTURE_SKEW_SECS)
        && event.verify().is_ok();
    if !valid {
        return None;
    }
    let maintenance_mode = event.tags.iter().any(|tag| {
        matches!(tag.as_slice(), [name, value, ..] if name == "maintenance_mode" && value == "true")
    });
    Some(InfoSnapshot {
        created_at: event.created_at.as_secs(),
        maintenance_mode,
    })
}

/// The newest valid info event among `events`.
pub fn newest_info<'a>(
    events: impl IntoIterator<Item = &'a Event>,
    mostro: PublicKey,
    now: u64,
) -> Option<InfoSnapshot> {
    events
        .into_iter()
        .filter_map(|event| parse_info(event, mostro, now))
        .max_by_key(|info| info.created_at)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::event::tests::NOW;

    fn info_event(keys: &Keys, d: &str, created_at: u64, extra: &[&[&str]]) -> Event {
        let tags = std::iter::once(Tag::identifier(d))
            .chain(extra.iter().map(|t| Tag::parse(t.iter().copied()).unwrap()));
        EventBuilder::new(Kind::Custom(NOSTR_INFO_EVENT_KIND), "")
            .tags(tags)
            .custom_created_at(Timestamp::from(created_at))
            .finalize(keys)
            .unwrap()
    }

    #[test]
    fn the_newest_info_event_wins_and_carries_maintenance() {
        let mostro = Keys::generate();
        let d = mostro.public_key().to_hex();
        let old = info_event(&mostro, &d, NOW - 600, &[&["maintenance_mode", "false"]]);
        let new = info_event(&mostro, &d, NOW - 60, &[&["maintenance_mode", "true"]]);

        let info = newest_info([&old, &new], mostro.public_key(), NOW).unwrap();

        assert_eq!(info.created_at, NOW - 60);
        assert!(info.maintenance_mode);
    }

    #[test]
    fn a_missing_maintenance_tag_reads_as_false() {
        let mostro = Keys::generate();
        let d = mostro.public_key().to_hex();

        let info =
            parse_info(&info_event(&mostro, &d, NOW, &[]), mostro.public_key(), NOW).unwrap();

        assert!(!info.maintenance_mode);
    }

    #[test]
    fn info_events_from_other_keys_addresses_or_the_future_are_ignored() {
        let mostro = Keys::generate();
        let other = Keys::generate();
        let d = mostro.public_key().to_hex();

        let foreign = info_event(&other, &other.public_key().to_hex(), NOW, &[]);
        let wrong_d = info_event(&mostro, "elsewhere", NOW, &[]);
        let future = info_event(&mostro, &d, NOW + 301, &[]);

        for event in [&foreign, &wrong_d, &future] {
            assert!(parse_info(event, mostro.public_key(), NOW).is_none());
        }
    }
}
