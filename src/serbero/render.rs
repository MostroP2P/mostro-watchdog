//! What the Serbero relay needs to know about dispute statuses and how
//! Serbero's words are shown.

use mostro_core::dispute::Status;

/// The cooperative-cancel status of older nodes, shown as the end of the
/// dispute's timeline.
const CANCELED_STATUS: &str = "canceled";

/// Whether a dispute is over, by its kind-38386 status. A status this
/// version does not know is not taken as an outcome.
pub fn dispute_is_resolved(status: &str) -> bool {
    status == CANCELED_STATUS
        || matches!(
            status.parse(),
            Ok(Status::SellerRefunded
                | Status::Settled
                | Status::Released
                | Status::CooperativelyCanceled)
        )
}

/// A Serbero reason or path for people: `conflicting_claims` →
/// `conflicting claims`.
pub fn humanize(word: &str) -> String {
    word.replace('_', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_and_paths_read_as_words() {
        assert_eq!(humanize("conflicting_claims"), "conflicting claims");
        assert_eq!(
            humanize("self_resolution_stalled"),
            "self resolution stalled"
        );
        assert_eq!(humanize("flood"), "flood");
    }

    #[test]
    fn only_known_outcomes_count_as_resolved() {
        for resolved in [
            "settled",
            "seller-refunded",
            "released",
            "cooperatively-canceled",
            // What older nodes send on a cooperative cancel; the watchdog
            // deletes the dispute's message on it.
            "canceled",
        ] {
            assert!(dispute_is_resolved(resolved), "{resolved}");
        }
        // An unknown status might still need a solver.
        for other in ["initiated", "in-progress", "some-new-status", ""] {
            assert!(!dispute_is_resolved(other), "{other}");
        }
    }
}
