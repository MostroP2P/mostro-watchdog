//! The watchdog's own heartbeat: a kind-38389 event with `z` = `watcher`.
//! It lets another watchdog watch this one, and it is the relay round-trip
//! probe: an event this watchdog can't read back means the relays, not
//! Mostro, are the problem.

use nostr_sdk::prelude::*;

use super::event::{HEALTH_KIND, MAX_FUTURE_SKEW_SECS, Z_WATCHER};
use super::store::LastHeartbeat;

/// `y` of the heartbeat: the software publishing it.
pub const SOFTWARE: &str = "mostro-watchdog";

/// The `seq` and `created_at` of the next heartbeat. Both strictly rise
/// over the last one published, even across restarts and clock steps:
/// `seq` is the clock in milliseconds, `created_at` the clock in seconds,
/// each bumped past the last value when the clock went back. Relays keep
/// the revision with the newest `created_at`, so it must never go back.
pub fn next(last: Option<LastHeartbeat>, now_ms: u64) -> LastHeartbeat {
    let now = now_ms / 1000;
    match last {
        None => LastHeartbeat {
            seq: now_ms.max(1),
            created_at: now,
        },
        Some(last) => LastHeartbeat {
            seq: now_ms.max(last.seq.saturating_add(1)),
            created_at: now.max(last.created_at.saturating_add(1)),
        },
    }
}

/// Whether `created_at` is so far ahead of the clock that readers will
/// drop the heartbeat as coming from the future: the clock went back by
/// more than they tolerate.
pub fn too_far_ahead(next: LastHeartbeat, now: u64) -> bool {
    next.created_at > now.saturating_add(MAX_FUTURE_SKEW_SECS)
}

/// Signs the heartbeat.
pub fn build(
    keys: &Keys,
    next: LastHeartbeat,
    interval: u64,
    watching: Option<PublicKey>,
) -> Result<Event, String> {
    let mut tags = vec![
        Tag::identifier(keys.public_key().to_hex()),
        Tag::custom("y", [SOFTWARE]),
        Tag::custom("z", [Z_WATCHER]),
    ];
    if let Some(mostro) = watching {
        tags.push(Tag::custom("watching", [mostro.to_hex()]));
    }
    tags.extend([
        Tag::custom("interval", [interval.to_string()]),
        Tag::custom("seq", [next.seq.to_string()]),
        Tag::expiration(Timestamp::from(next.created_at + 3 * interval)),
    ]);
    EventBuilder::new(HEALTH_KIND, "")
        .tags(tags)
        .custom_created_at(Timestamp::from(next.created_at))
        .finalize(keys)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::event::parse_watcher;

    const NOW_MS: u64 = 1_792_000_000_500;

    #[test]
    fn the_heartbeat_is_a_valid_watcher_event() {
        let keys = Keys::generate();
        let mostro = Keys::generate().public_key();
        let next = next(None, NOW_MS);

        let event = build(&keys, next, 60, Some(mostro)).unwrap();
        let beat = parse_watcher(&event, Some(keys.public_key()), NOW_MS / 1000).unwrap();

        assert_eq!(beat.software, SOFTWARE);
        assert_eq!(beat.watching, vec![mostro]);
        assert_eq!(beat.envelope.seq, NOW_MS);
        assert_eq!(beat.envelope.expiration, NOW_MS / 1000 + 180);
    }

    #[test]
    fn the_watching_tag_can_be_left_out() {
        let keys = Keys::generate();

        let event = build(&keys, next(None, NOW_MS), 60, None).unwrap();

        assert!(event.tags.iter().all(|t| t.as_slice()[0] != "watching"));
    }

    #[test]
    fn seq_and_created_at_keep_rising_when_the_clock_steps_back() {
        let last = LastHeartbeat {
            seq: NOW_MS,
            created_at: NOW_MS / 1000,
        };

        let after_step_back = next(Some(last), NOW_MS - 10_000);

        assert_eq!(after_step_back.seq, NOW_MS + 1);
        assert_eq!(after_step_back.created_at, NOW_MS / 1000 + 1);
        assert!(!too_far_ahead(after_step_back, NOW_MS / 1000 - 10));
    }

    #[test]
    fn a_clock_far_behind_the_last_heartbeat_is_flagged() {
        let last = LastHeartbeat {
            seq: NOW_MS,
            created_at: NOW_MS / 1000,
        };
        let now = NOW_MS / 1000 - 600;

        let next = next(Some(last), now * 1000);

        assert!(too_far_ahead(next, now));
    }

    #[test]
    fn with_a_sane_clock_both_follow_it() {
        let last = LastHeartbeat {
            seq: NOW_MS,
            created_at: NOW_MS / 1000,
        };

        let next = next(Some(last), NOW_MS + 60_000);

        assert_eq!(next.seq, NOW_MS + 60_000);
        assert_eq!(next.created_at, NOW_MS / 1000 + 60);
    }
}
