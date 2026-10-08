//! Holding chat events for the grace period, grouping them per conversation
//! and solver, and the text of the notification (MarkdownV2).

use std::collections::{BTreeSet, HashMap};

use nostr_sdk::prelude::{EventId, PublicKey};

use super::protocol::Party;
use crate::{escape_markdown, escape_markdown_code};

/// Seconds before a notification Telegram refused is tried again.
pub const RETRY_DELAY_SECS: u64 = 60;

/// Tries per notification before it is dropped.
pub const MAX_ATTEMPTS: u8 = 3;

/// Chat events of one conversation waiting to be notified to one solver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub sign_pubkey: PublicKey,
    /// The solver to notify (hex key).
    pub solver: String,
    pub event_ids: BTreeSet<EventId>,
    /// When to notify (Unix seconds).
    pub due: u64,
    /// Sends already tried.
    pub attempts: u8,
}

/// A conversation as one solver watches it.
type BatchKey = (PublicKey, String);

/// Chat events held until their batch is due.
#[derive(Debug, Default)]
pub struct Pending {
    batches: HashMap<BatchKey, Batch>,
}

impl Pending {
    /// Holds `event_id` for `solver`. The first event of a conversation
    /// opens a batch due `grace` seconds later; later ones join it, so a
    /// burst of messages is one notification. Returns `false` when it was
    /// already held.
    pub fn add(
        &mut self,
        sign_pubkey: PublicKey,
        solver: &str,
        event_id: EventId,
        now: u64,
        grace: u64,
    ) -> bool {
        self.batches
            .entry((sign_pubkey, solver.to_owned()))
            .or_insert_with(|| Batch {
                sign_pubkey,
                solver: solver.to_owned(),
                event_ids: BTreeSet::new(),
                due: now.saturating_add(grace),
                attempts: 0,
            })
            .event_ids
            .insert(event_id)
    }

    /// Whether `event_id` is held for `solver`.
    pub fn contains(&self, event_id: &EventId, solver: &str) -> bool {
        self.batches
            .values()
            .any(|b| b.solver == solver && b.event_ids.contains(event_id))
    }

    /// Removes and returns the batches due at `now`.
    pub fn take_due(&mut self, now: u64) -> Vec<Batch> {
        let due: Vec<BatchKey> = self
            .batches
            .iter()
            .filter(|(_, b)| b.due <= now)
            .map(|(key, _)| key.clone())
            .collect();
        due.iter()
            .filter_map(|key| self.batches.remove(key))
            .collect()
    }

    /// Holds a batch Telegram refused for another try, with any events of
    /// its conversation that arrived meanwhile. Returns `false`, dropping
    /// it, after [`MAX_ATTEMPTS`].
    pub fn retry(&mut self, mut batch: Batch, now: u64) -> bool {
        batch.attempts += 1;
        if batch.attempts >= MAX_ATTEMPTS {
            return false;
        }
        batch.due = now.saturating_add(RETRY_DELAY_SECS);
        let key = (batch.sign_pubkey, batch.solver.clone());
        if let Some(newer) = self.batches.remove(&key) {
            batch.event_ids.extend(newer.event_ids);
            batch.due = batch.due.min(newer.due);
        }
        self.batches.insert(key, batch);
        true
    }
}

/// The private message telling a solver that a party wrote.
pub fn notification_text(party: Party, dispute_id: &str, count: usize) -> String {
    let what = match count {
        1 => format!("New message from the {}", party.as_str()),
        n => format!("{n} new messages from the {}", party.as_str()),
    };
    format!(
        "📩 *{}*\n\n\
         📋 *Dispute ID:* `{}`\n\n\
         Open Mostrix to read and answer\\.",
        escape_markdown(&what),
        escape_markdown_code(dispute_id),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::prelude::Keys;

    const DISPUTE: &str = "58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a";
    const GRACE: u64 = 20;
    const SOLVER: &str = "solver";

    fn id(n: u8) -> EventId {
        EventId::from_byte_array([n; 32])
    }

    #[test]
    fn a_batch_is_due_after_the_grace_period() {
        let mut pending = Pending::default();
        let conv = Keys::generate().public_key();
        pending.add(conv, SOLVER, id(1), 100, GRACE);

        assert!(pending.take_due(119).is_empty());
        let due = pending.take_due(120);

        assert_eq!(due.len(), 1);
        assert_eq!(due[0].event_ids, BTreeSet::from([id(1)]));
        assert_eq!(due[0].solver, SOLVER);
        assert!(pending.take_due(500).is_empty());
    }

    #[test]
    fn a_burst_of_messages_joins_the_first_batch() {
        let mut pending = Pending::default();
        let conv = Keys::generate().public_key();
        pending.add(conv, SOLVER, id(1), 100, GRACE);

        assert!(pending.add(conv, SOLVER, id(2), 115, GRACE));
        assert!(!pending.add(conv, SOLVER, id(2), 116, GRACE));
        let due = pending.take_due(120);

        assert_eq!(due[0].event_ids, BTreeSet::from([id(1), id(2)]));
        assert!(!pending.contains(&id(1), SOLVER));
    }

    #[test]
    fn conversations_are_batched_apart() {
        let mut pending = Pending::default();
        let (buyer, seller) = (Keys::generate().public_key(), Keys::generate().public_key());
        pending.add(buyer, SOLVER, id(1), 100, GRACE);
        pending.add(seller, SOLVER, id(2), 110, GRACE);

        assert_eq!(pending.take_due(120).len(), 1);
        assert_eq!(pending.take_due(130)[0].sign_pubkey, seller);
    }

    #[test]
    fn each_solver_watching_a_conversation_gets_its_own_batch() {
        let mut pending = Pending::default();
        let conv = Keys::generate().public_key();
        pending.add(conv, "a", id(1), 100, GRACE);

        assert!(pending.add(conv, "b", id(1), 100, GRACE));
        assert!(pending.contains(&id(1), "b"));
        assert!(!pending.contains(&id(1), "c"));
        assert_eq!(pending.take_due(120).len(), 2);
    }

    #[test]
    fn a_refused_batch_is_retried_then_dropped() {
        let mut pending = Pending::default();
        let conv = Keys::generate().public_key();
        pending.add(conv, SOLVER, id(1), 100, GRACE);
        let batch = pending.take_due(120).remove(0);

        assert!(pending.retry(batch, 120));
        let batch = pending.take_due(180).remove(0);
        assert_eq!(batch.attempts, 1);
        assert!(pending.retry(batch, 180));
        let batch = pending.take_due(240).remove(0);

        assert!(!pending.retry(batch, 240));
        assert!(pending.take_due(10_000).is_empty());
    }

    #[test]
    fn a_retry_keeps_messages_that_arrived_meanwhile() {
        let mut pending = Pending::default();
        let conv = Keys::generate().public_key();
        pending.add(conv, SOLVER, id(1), 100, GRACE);
        let batch = pending.take_due(120).remove(0);
        pending.add(conv, SOLVER, id(2), 125, GRACE);

        pending.retry(batch, 130);
        let due = pending.take_due(145);

        assert_eq!(due[0].event_ids, BTreeSet::from([id(1), id(2)]));
    }

    #[test]
    fn the_notification_names_the_party_and_the_dispute() {
        assert_eq!(
            notification_text(Party::Buyer, DISPUTE, 1),
            "📩 *New message from the buyer*\n\n\
             📋 *Dispute ID:* `58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a`\n\n\
             Open Mostrix to read and answer\\."
        );
        assert!(notification_text(Party::Seller, DISPUTE, 3)
            .starts_with("📩 *3 new messages from the seller*"));
    }

    #[test]
    fn the_dispute_id_is_escaped_inside_its_code_span() {
        assert!(notification_text(Party::Buyer, "a`b", 1).contains("`a\\`b`"));
    }
}
