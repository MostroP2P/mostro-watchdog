//! Telegram messages for health incidents, in MarkdownV2. All the notices of
//! one evaluation go out as one message: a section per subject for new
//! alerts, reminders and recoveries, so a round rings the phones once.
//!
//! Untrusted text (instance names, details built from event data) is
//! clipped before it is escaped, so no item can outgrow a message and no
//! cut can split an escape sequence. A round too large for one message is
//! split between items, repeating the section heading.

use super::rules::{human_duration, Severity, Subject};
use super::tracker::Notice;
use crate::{chrono_timestamp, escape_markdown, escape_markdown_code};

/// Telegram's limit on a message, in characters, minus some room.
pub const MAX_MESSAGE_CHARS: usize = 4000;

/// Longest instance name shown, in characters.
const MAX_NAME_CHARS: usize = 64;

/// Longest detail shown, in characters, before escaping.
const MAX_DETAIL_CHARS: usize = 600;

/// `text` cut to at most `max` characters, on a character boundary, with
/// an ellipsis when cut. Applied before escaping.
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

/// How a subject is named in messages.
pub fn subject_label(subject: &Subject, mostro_name: Option<&str>) -> String {
    match subject {
        Subject::Mostro(pk) => {
            let short = short_key(&pk.to_hex());
            match mostro_name {
                Some(name) => format!(
                    "*{}* `{}`",
                    escape_markdown(&clip(name, MAX_NAME_CHARS)),
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

fn detail(text: &str) -> String {
    escape_markdown(&clip(text, MAX_DETAIL_CHARS))
}

/// A heading (kind of notice and subject) and one item per notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub heading: String,
    pub items: Vec<String>,
}

#[cfg(test)]
impl Section {
    /// The section as one block of text.
    pub fn text(&self) -> String {
        std::iter::once(self.heading.as_str())
            .chain(self.items.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// New alerts about one subject.
pub fn opened(label: &str, notices: &[&Notice]) -> Section {
    let critical = notices
        .iter()
        .any(|n| n.incident().severity == Severity::Critical);
    let header = if critical {
        "🚨 *Mostro health alert*"
    } else {
        "⚠️ *Mostro health warning*"
    };
    let items = notices
        .iter()
        .map(|notice| {
            let incident = notice.incident();
            format!(
                "{} *{}*\n{}\nSince: {}",
                icon(incident.severity),
                escape_markdown(incident.rule.title()),
                detail(&incident.detail),
                escape_markdown(&chrono_timestamp(incident.started_at))
            )
        })
        .collect();
    Section {
        heading: format!("{header}\n{label}"),
        items,
    }
}

/// Reminders of critical alerts still active about one subject.
pub fn reminders(label: &str, notices: &[&Notice], now: u64) -> Section {
    let items = notices
        .iter()
        .map(|notice| {
            let incident = notice.incident();
            format!(
                "{} *{}*\n{}\nFor: {}",
                icon(incident.severity),
                escape_markdown(incident.rule.title()),
                detail(&incident.detail),
                escape_markdown(&human_duration(now.saturating_sub(incident.started_at)))
            )
        })
        .collect();
    Section {
        heading: format!("🔁 *Still active*\n{label}"),
        items,
    }
}

/// Recoveries about one subject.
pub fn resolved(label: &str, notices: &[&Notice]) -> Section {
    let items = notices
        .iter()
        .map(|notice| {
            let incident = notice.incident();
            let (at, resolution) = match notice {
                Notice::Resolved { at, detail, .. } => (*at, detail.as_deref()),
                _ => (incident.opened_at, None),
            };
            let resolution = resolution
                .map(|d| format!("{}\n", detail(d)))
                .unwrap_or_default();
            format!(
                "*{}*\n{}Duration: {}",
                escape_markdown(incident.rule.title()),
                resolution,
                escape_markdown(&human_duration(at.saturating_sub(incident.started_at)))
            )
        })
        .collect();
    Section {
        heading: format!("✅ *Resolved*\n{label}"),
        items,
    }
}

/// One message to send, and the items it carries as `(section, item)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub items: Vec<(usize, usize)>,
}

/// Packs `sections` into as few messages as fit Telegram's limit. Almost
/// always one. When a round doesn't fit, it is split between items, and a
/// section that continues in the next message repeats its heading. Items
/// are bounded by clipping, so each fits a message with its heading.
pub fn pack(sections: &[Section]) -> Vec<Message> {
    let mut messages: Vec<Message> = Vec::new();
    // The section the last message ends in.
    let mut open_section: Option<usize> = None;
    for (s, section) in sections.iter().enumerate() {
        for (i, item) in section.items.iter().enumerate() {
            let continues = open_section == Some(s) && !messages.is_empty();
            let addition = if continues {
                format!("\n\n{item}")
            } else {
                format!("{}\n\n{item}", section.heading)
            };
            let fits = messages.last().is_some_and(|m| {
                let separator = if continues { 0 } else { 2 };
                m.text.chars().count() + separator + addition.chars().count() <= MAX_MESSAGE_CHARS
            });
            match messages.last_mut() {
                Some(message) if fits => {
                    if !continues {
                        message.text.push_str("\n\n");
                    }
                    message.text.push_str(&addition);
                    message.items.push((s, i));
                }
                _ => messages.push(Message {
                    text: format!("{}\n\n{item}", section.heading),
                    items: vec![(s, i)],
                }),
            }
            open_section = Some(s);
        }
    }
    messages
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

        let text = opened("Mostro `0123…cdef`", &[&lag, &waiting]).text();

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

        assert!(opened("x", &[&n])
            .text()
            .starts_with("⚠️ *Mostro health warning*"));
    }

    #[test]
    fn a_recovery_says_how_long_it_lasted() {
        let n = Notice::Resolved {
            incident: incident(RuleId::BlockLag, "lag"),
            at: T0 + 74_460,
            detail: Some("LND at 971486, reference 971486 from mempool.space.".into()),
        };

        let text = resolved("Mostro `0123…cdef`", &[&n]).text();

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

        let text = reminders("x", &[&n], T0 + 7200).text();

        assert!(text.contains("For: 2 h 0 min"), "{text}");
        assert!(text.contains(r"synced\_to\_chain"), "{text}");
    }

    fn section(heading: &str, items: &[String]) -> Section {
        Section {
            heading: heading.into(),
            items: items.to_vec(),
        }
    }

    #[test]
    fn sections_share_one_message_when_they_fit() {
        let sections = [
            section("A", &["a".repeat(100)]),
            section("B", &["b".repeat(100), "c".repeat(100)]),
        ];

        let messages = pack(&sections);

        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].text,
            format!(
                "A\n\n{}\n\nB\n\n{}\n\n{}",
                "a".repeat(100),
                "b".repeat(100),
                "c".repeat(100)
            )
        );
        assert_eq!(messages[0].items, vec![(0, 0), (1, 0), (1, 1)]);
        assert!(pack(&[]).is_empty());
    }

    #[test]
    fn an_oversized_section_is_split_between_items_repeating_its_heading() {
        let items: Vec<String> = (0..10).map(|i| format!("{i}{}", "x".repeat(799))).collect();
        let sections = [section("HEAD", &items)];

        let messages = pack(&sections);

        assert!(messages.len() > 1);
        for message in &messages {
            assert!(message.text.chars().count() <= MAX_MESSAGE_CHARS);
            assert!(message.text.starts_with("HEAD\n\n"), "{}", message.text);
        }
        let carried: Vec<_> = messages.iter().flat_map(|m| m.items.clone()).collect();
        assert_eq!(carried, (0..10).map(|i| (0, i)).collect::<Vec<_>>());
    }

    #[test]
    fn untrusted_text_is_clipped_before_escaping_so_no_escape_is_cut() {
        // A detail of 5000 dots: escaped, each becomes two characters.
        let n = Notice::Opened(incident(RuleId::PayoutFailureRate, &".".repeat(5000)));

        let section = opened("x", &[&n]);
        let messages = pack(std::slice::from_ref(&section));

        assert_eq!(messages.len(), 1);
        assert!(messages[0].text.chars().count() <= MAX_MESSAGE_CHARS);
        assert!(messages[0].text.contains("…"));
        assert_escaped(&messages[0].text);
        assert_eq!(clip("ñandú", 3), "ña…");
        let label = subject_label(
            &Subject::Mostro(Keys::generate().public_key()),
            Some(&"_".repeat(500)),
        );
        assert!(label.chars().count() < 200, "{label}");
        assert_escaped(&label);
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
