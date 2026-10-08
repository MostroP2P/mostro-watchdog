//! Telegram texts (MarkdownV2) for Serbero's updates.

use mostro_core::dispute::Status;

use super::dm::Update;
use crate::{chrono_timestamp, escape_markdown, escape_markdown_code};

/// What the team should do when Serbero needs a human (MarkdownV2).
const TAKE_OVER_HINT: &str =
    "⚡ A solver must take it over in Mostrix \\(Ctrl\\+T on Disputes Pending\\)\\.";

/// Whether a dispute still waits for a solver, by its kind-38386 status.
pub fn dispute_is_open(status: &str) -> bool {
    matches!(status.parse(), Ok(Status::Initiated | Status::InProgress))
}

/// The cooperative-cancel status of older nodes, which `handle_dispute_event`
/// treats as the end of the dispute (it deletes the dispute's message).
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

/// Where a dispute stands for Serbero's line under its alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisputeStage {
    /// Still waiting for a solver, or held by Serbero.
    Open,
    /// A solver took the dispute over from Serbero; it stays so once the
    /// dispute closes.
    TakenOver,
    /// Resolved, or in a status this version does not know.
    Closed,
}

impl DisputeStage {
    /// The stage of a dispute in kind-38386 `status`, `taken_over` when a
    /// solver took it over from Serbero.
    pub fn of(status: &str, taken_over: bool) -> Self {
        if taken_over {
            Self::TakenOver
        } else if dispute_is_open(status) {
            Self::Open
        } else {
            Self::Closed
        }
    }
}

/// The line shown under a dispute's alert for Serbero's latest state. It
/// asks for a solver only while one is still needed, so it stays true once
/// a solver takes over or the dispute closes.
pub fn status_line(update: &Update, stage: DisputeStage) -> String {
    let (icon, state) = match update {
        Update::Mediating if stage == DisputeStage::Open => ("🤖", "mediating".to_owned()),
        Update::Mediating => ("🤖", "mediated".to_owned()),
        Update::GuidanceSent { path } => (
            "🤖",
            detailed(
                "guided the parties to resolve it themselves",
                path.as_deref(),
            ),
        ),
        Update::HandedOff { reason } => ("🙋", detailed("handed off", reason.as_deref())),
        Update::CouldNotStart => ("🙋", "mediation could not start".to_owned()),
    };
    let state = match stage {
        DisputeStage::TakenOver => format!("{state} — a solver took it over"),
        DisputeStage::Open if update.needs_human() => {
            format!("{state} — a solver must take it over")
        }
        DisputeStage::Open | DisputeStage::Closed => state,
    };
    format!("{icon} *Serbero:* {}", escape_markdown(&state))
}

fn detailed(text: &str, detail: Option<&str>) -> String {
    match detail {
        Some(detail) => format!("{text} ({})", humanize(detail)),
        None => text.to_owned(),
    }
}

/// A dispute's alert text with Serbero's line, when there is one.
pub fn with_status_line(base: &str, line: Option<&str>) -> String {
    match line {
        Some(line) => format!("{base}\n\n{line}"),
        None => base.to_owned(),
    }
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
    fn only_initiated_and_in_progress_disputes_are_open() {
        assert!(dispute_is_open("initiated"));
        assert!(dispute_is_open("in-progress"));
        for other in [
            "settled",
            "seller-refunded",
            "released",
            "canceled",
            "cooperatively-canceled",
            "unknown",
        ] {
            assert!(!dispute_is_open(other), "{other}");
        }
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
    fn progress_lines_while_the_dispute_is_open() {
        assert_eq!(
            status_line(&Update::Mediating, DisputeStage::Open),
            "🤖 *Serbero:* mediating"
        );
        assert_eq!(
            status_line(
                &Update::GuidanceSent {
                    path: Some("payment_arrived".into())
                },
                DisputeStage::Open
            ),
            "🤖 *Serbero:* guided the parties to resolve it themselves \\(payment arrived\\)"
        );
        assert_eq!(
            status_line(&Update::GuidanceSent { path: None }, DisputeStage::Open),
            "🤖 *Serbero:* guided the parties to resolve it themselves"
        );
    }

    #[test]
    fn handoff_lines_ask_for_a_solver_while_the_dispute_is_open() {
        assert_eq!(
            status_line(&handed_off("conflicting_claims"), DisputeStage::Open),
            "🙋 *Serbero:* handed off \\(conflicting claims\\) — a solver must take it over"
        );
        assert_eq!(
            status_line(&Update::HandedOff { reason: None }, DisputeStage::Open),
            "🙋 *Serbero:* handed off — a solver must take it over"
        );
        assert_eq!(
            status_line(&Update::CouldNotStart, DisputeStage::Open),
            "🙋 *Serbero:* mediation could not start — a solver must take it over"
        );
    }

    #[test]
    fn lines_stay_true_once_the_dispute_is_closed() {
        assert_eq!(
            status_line(&Update::Mediating, DisputeStage::Closed),
            "🤖 *Serbero:* mediated"
        );
        assert_eq!(
            status_line(&handed_off("fraud_signal"), DisputeStage::Closed),
            "🙋 *Serbero:* handed off \\(fraud signal\\)"
        );
        assert_eq!(
            status_line(&Update::CouldNotStart, DisputeStage::Closed),
            "🙋 *Serbero:* mediation could not start"
        );
        assert_eq!(
            status_line(
                &Update::GuidanceSent {
                    path: Some("payment_not_sent".into())
                },
                DisputeStage::Closed
            ),
            "🤖 *Serbero:* guided the parties to resolve it themselves \\(payment not sent\\)"
        );
    }

    #[test]
    fn the_status_line_goes_below_the_alert() {
        assert_eq!(
            with_status_line("🚨 *NEW DISPUTE*", Some("🤖 *Serbero:* mediating")),
            "🚨 *NEW DISPUTE*\n\n🤖 *Serbero:* mediating"
        );
        assert_eq!(
            with_status_line("🚨 *NEW DISPUTE*", None),
            "🚨 *NEW DISPUTE*"
        );
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
    fn a_takeover_outranks_the_dispute_status() {
        assert_eq!(DisputeStage::of("initiated", false), DisputeStage::Open);
        assert_eq!(DisputeStage::of("in-progress", false), DisputeStage::Open);
        assert_eq!(DisputeStage::of("settled", false), DisputeStage::Closed);
        assert_eq!(
            DisputeStage::of("in-progress", true),
            DisputeStage::TakenOver
        );
        // A dispute closed after the takeover keeps saying who took it.
        assert_eq!(DisputeStage::of("settled", true), DisputeStage::TakenOver);
    }

    #[test]
    fn lines_say_a_solver_took_over_instead_of_asking_for_one() {
        assert_eq!(
            status_line(&handed_off("conflicting_claims"), DisputeStage::TakenOver),
            "🙋 *Serbero:* handed off \\(conflicting claims\\) — a solver took it over"
        );
        assert_eq!(
            status_line(&Update::CouldNotStart, DisputeStage::TakenOver),
            "🙋 *Serbero:* mediation could not start — a solver took it over"
        );
        // Serbero stops mediating as soon as a solver takes over.
        assert_eq!(
            status_line(&Update::Mediating, DisputeStage::TakenOver),
            "🤖 *Serbero:* mediated — a solver took it over"
        );
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
