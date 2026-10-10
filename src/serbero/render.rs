//! Telegram texts (MarkdownV2) for Serbero's updates.

use mostro_core::dispute::Status;

use super::dm::Update;
use crate::{chrono_timestamp, escape_markdown, escape_markdown_code};

/// What the team should do when Serbero needs a human (MarkdownV2).
const TAKE_OVER_HINT: &str =
    "⚡ A solver must take it over in Mostrix \\(Ctrl\\+T on Disputes Pending\\)\\.";

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

/// The new message sent when Serbero needs a human solver for a dispute.
/// `None` for updates that need nobody.
pub fn needs_human_alert(dispute_id: &str, update: &Update, created_at: u64) -> Option<String> {
    let (title, reason) = match update {
        Update::HandedOff { reason } => ("SERBERO HANDED OFF A DISPUTE", reason.as_deref()),
        Update::CouldNotStart => ("SERBERO COULD NOT START MEDIATION", None),
        Update::Mediating | Update::GuidanceSent { .. } => return None,
    };
    let reason = reason
        .map(|reason| format!("💬 *Reason:* {}\n", escape_markdown(&humanize(reason))))
        .unwrap_or_default();
    Some(format!(
        "🙋 *{title}*\n\n\
         📋 *Dispute ID:* `{}`\n\
         {reason}\
         ⏰ *Time:* {}\n\n\
         {TAKE_OVER_HINT}",
        escape_markdown_code(dispute_id),
        escape_markdown(&chrono_timestamp(created_at)),
    ))
}

/// The new message sent when a solver takes a dispute over from Serbero.
pub fn takeover_alert(dispute_id: &str, created_at: u64) -> String {
    format!(
        "👨‍⚖️ *SOLVER TOOK OVER FROM SERBERO*\n\n\
         📋 *Dispute ID:* `{}`\n\
         ⏰ *Time:* {}\n\n\
         ℹ️ A solver is now handling the dispute\\.",
        escape_markdown_code(dispute_id),
        escape_markdown(&chrono_timestamp(created_at)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DISPUTE: &str = "58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a";
    /// 2021-01-01 00:00:00 UTC
    const AT: u64 = 1_609_459_200;

    fn handed_off(reason: &str) -> Update {
        Update::HandedOff {
            reason: Some(reason.into()),
        }
    }

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

    #[test]
    fn a_handoff_alert_names_the_dispute_reason_and_next_step() {
        let alert = needs_human_alert(DISPUTE, &handed_off("conflicting_claims"), AT);

        assert_eq!(
            alert.as_deref(),
            Some(
                "🙋 *SERBERO HANDED OFF A DISPUTE*\n\n\
                 📋 *Dispute ID:* `58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a`\n\
                 💬 *Reason:* conflicting claims\n\
                 ⏰ *Time:* 2021\\-01\\-01 00:00:00 UTC\n\n\
                 ⚡ A solver must take it over in Mostrix \\(Ctrl\\+T on Disputes Pending\\)\\."
            )
        );
    }

    #[test]
    fn a_handoff_without_a_reason_leaves_the_reason_out() {
        let alert =
            needs_human_alert(DISPUTE, &Update::HandedOff { reason: None }, AT).expect("alert");

        assert!(!alert.contains("Reason"), "{alert}");
        assert!(alert.starts_with("🙋 *SERBERO HANDED OFF A DISPUTE*"));
    }

    #[test]
    fn a_failed_opening_alert_asks_for_a_solver() {
        let alert = needs_human_alert(DISPUTE, &Update::CouldNotStart, AT);

        assert_eq!(
            alert.as_deref(),
            Some(
                "🙋 *SERBERO COULD NOT START MEDIATION*\n\n\
                 📋 *Dispute ID:* `58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a`\n\
                 ⏰ *Time:* 2021\\-01\\-01 00:00:00 UTC\n\n\
                 ⚡ A solver must take it over in Mostrix \\(Ctrl\\+T on Disputes Pending\\)\\."
            )
        );
    }

    #[test]
    fn progress_updates_send_no_alert() {
        assert_eq!(needs_human_alert(DISPUTE, &Update::Mediating, AT), None);
        assert_eq!(
            needs_human_alert(DISPUTE, &Update::GuidanceSent { path: None }, AT),
            None
        );
    }

    #[test]
    fn the_dispute_id_is_escaped_inside_its_code_span() {
        let alert = needs_human_alert("a`b\\c", &Update::CouldNotStart, AT).expect("alert");

        assert!(alert.contains("`a\\`b\\\\c`"), "{alert}");
    }

    #[test]
    fn a_takeover_alert_names_the_dispute_and_says_who_handles_it() {
        assert_eq!(
            takeover_alert(DISPUTE, AT),
            "👨‍⚖️ *SOLVER TOOK OVER FROM SERBERO*\n\n\
             📋 *Dispute ID:* `58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a`\n\
             ⏰ *Time:* 2021\\-01\\-01 00:00:00 UTC\n\n\
             ℹ️ A solver is now handling the dispute\\."
        );
    }

    #[test]
    fn the_takeover_alert_escapes_the_dispute_id() {
        assert!(takeover_alert("a`b", AT).contains("`a\\`b`"));
    }
}
