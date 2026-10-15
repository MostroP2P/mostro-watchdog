//! Telegram messages for health incidents, in MarkdownV2. The notices of
//! one evaluation are grouped: one message per subject for new alerts, one
//! for reminders, one for recoveries, so an incident that trips several
//! rules at once rings the phones once.

use super::rules::{human_duration, Severity, Subject};
use super::tracker::Notice;
use crate::{chrono_timestamp, escape_markdown, escape_markdown_code};

/// How a subject is named in messages.
pub fn subject_label(subject: &Subject, mostro_name: Option<&str>) -> String {
    match subject {
        Subject::Mostro(pk) => {
            let short = short_key(&pk.to_hex());
            match mostro_name {
                Some(name) => format!(
                    "*{}* `{}`",
                    escape_markdown(name),
                    escape_markdown_code(&short)
                ),
                None => format!("Mostro `{}`", escape_markdown_code(&short)),
            }
        }
        Subject::PeerWatchdog(pk) => format!(
            "Peer watchdog `{}`",
            escape_markdown_code(&short_key(&pk.to_hex()))
        ),
        Subject::Relays => "This watchdog's relays".into(),
    }
}

fn short_key(hex: &str) -> String {
    match (hex.get(..8), hex.get(hex.len().saturating_sub(4)..)) {
        (Some(head), Some(tail)) if hex.len() > 12 => format!("{head}…{tail}"),
        _ => hex.to_string(),
    }
}

fn icon(severity: Severity) -> &'static str {
    match severity {
        Severity::Critical => "🔴",
        Severity::Warning => "🟠",
    }
}

/// New alerts about one subject.
pub fn opened(label: &str, notices: &[&Notice]) -> String {
    let critical = notices
        .iter()
        .any(|n| n.incident().severity == Severity::Critical);
    let header = if critical {
        "🚨 *Mostro health alert*"
    } else {
        "⚠️ *Mostro health warning*"
    };
    let body = notices
        .iter()
        .map(|notice| {
            let incident = notice.incident();
            format!(
                "{} *{}*\n{}\nSince: {}",
                icon(incident.severity),
                escape_markdown(incident.rule.title()),
                escape_markdown(&incident.detail),
                escape_markdown(&chrono_timestamp(incident.started_at))
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!("{header}\n{label}\n\n{body}")
}

/// Reminders of critical alerts still active about one subject.
pub fn reminders(label: &str, notices: &[&Notice], now: u64) -> String {
    let body = notices
        .iter()
        .map(|notice| {
            let incident = notice.incident();
            format!(
                "{} *{}*\n{}\nFor: {}",
                icon(incident.severity),
                escape_markdown(incident.rule.title()),
                escape_markdown(&incident.detail),
                escape_markdown(&human_duration(now.saturating_sub(incident.started_at)))
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!("🔁 *Still active*\n{label}\n\n{body}")
}

/// Recoveries about one subject.
pub fn resolved(label: &str, notices: &[&Notice]) -> String {
    let body = notices
        .iter()
        .filter_map(|notice| match notice {
            Notice::Resolved {
                incident,
                at,
                detail,
            } => {
                let detail = detail
                    .as_deref()
                    .map(|d| format!("{}\n", escape_markdown(d)))
                    .unwrap_or_default();
                Some(format!(
                    "*{}*\n{}Duration: {}",
                    escape_markdown(incident.rule.title()),
                    detail,
                    escape_markdown(&human_duration(at.saturating_sub(incident.started_at)))
                ))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!("✅ *Resolved*\n{label}\n\n{body}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::rules::RuleId;
    use crate::health::tracker::Incident;
    use nostr_sdk::prelude::Keys;

    const T0: u64 = 1_791_952_200;

    fn incident(rule: RuleId, detail: &str) -> Incident {
        Incident {
            subject: Subject::Relays,
            rule,
            severity: rule.severity(),
            started_at: T0,
            opened_at: T0 + 900,
            notified_at: None,
            detail: detail.into(),
        }
    }

    /// Every MarkdownV2 reserved character outside an entity is escaped.
    fn assert_escaped(text: &str) {
        let mut in_code = false;
        let mut prev = ' ';
        for c in text.chars() {
            if c == '`' && prev != '\\' {
                in_code = !in_code;
            }
            if !in_code && "_[]()~>#+-=|{}.!".contains(c) {
                assert_eq!(prev, '\\', "unescaped {c:?} in {text}");
            }
            prev = if prev == '\\' && c == '\\' { ' ' } else { c };
        }
    }

    #[test]
    fn an_alert_lists_each_rule_with_its_detail_and_start() {
        let lag = Notice::Opened(incident(
            RuleId::BlockLag,
            "LND is 136 blocks behind (971349 vs 971485 from mempool.space).",
        ));
        let waiting = Notice::Opened(incident(
            RuleId::SettledUnpaid,
            "4 order(s) settled but unpaid.",
        ));

        let text = opened("Mostro `0123…cdef`", &[&lag, &waiting]);

        assert!(text.starts_with("🚨 *Mostro health alert*"), "{text}");
        assert!(text.contains("🔴 *LND behind the chain*"), "{text}");
        assert!(
            text.contains(r"\(971349 vs 971485 from mempool\.space\)\."),
            "{text}"
        );
        assert!(
            text.contains("🟠 *Orders waiting for their payout*"),
            "{text}"
        );
        assert!(text.contains(r"Since: 2026\-10\-14 04:30:00 UTC"), "{text}");
        assert_escaped(&text);
    }

    #[test]
    fn warnings_alone_are_headed_as_a_warning() {
        let n = Notice::Opened(incident(RuleId::Maintenance, "New orders are rejected."));

        assert!(opened("x", &[&n]).starts_with("⚠️ *Mostro health warning*"));
    }

    #[test]
    fn a_recovery_says_how_long_it_lasted() {
        let n = Notice::Resolved {
            incident: incident(RuleId::BlockLag, "lag"),
            at: T0 + 74_460,
            detail: Some("LND at 971486, reference 971486 from mempool.space.".into()),
        };

        let text = resolved("Mostro `0123…cdef`", &[&n]);

        assert!(text.starts_with("✅ *Resolved*"), "{text}");
        assert!(text.contains("Duration: 20 h 41 min"), "{text}");
        assert_escaped(&text);
    }

    #[test]
    fn a_reminder_says_for_how_long_it_has_lasted() {
        let n = Notice::Reminder(incident(
            RuleId::LndUnsynced,
            "LND reports synced_to_chain = false.",
        ));

        let text = reminders("x", &[&n], T0 + 7200);

        assert!(text.contains("For: 2 h 0 min"), "{text}");
        assert!(text.contains(r"synced\_to\_chain"), "{text}");
    }

    #[test]
    fn instance_names_are_escaped_and_keys_shortened() {
        let pk = Keys::generate().public_key();

        let named = subject_label(&Subject::Mostro(pk), Some("Mostro_P2P (main)"));
        let unnamed = subject_label(&Subject::Mostro(pk), None);

        assert!(named.starts_with(r"*Mostro\_P2P \(main\)*"), "{named}");
        assert!(unnamed.contains('…'), "{unnamed}");
        assert_escaped(&named);
    }
}
