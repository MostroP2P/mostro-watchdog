//! Decoding and classifying Serbero's direct messages.
//!
//! Serbero writes Mostro protocol v2 `send-dm` messages: a NIP-44 `kind 14`
//! event signed by its own key, whose `MessageKind.id` is the dispute id and
//! whose text starts with the header `Dispute <id> · <subject>` (serbero
//! `docs/messages.md` §3). Only that first line is ever read here: whatever
//! follows it may quote the parties, so it is dropped right after
//! decryption and never stored, logged or forwarded.

use mostro_core::message::{Action, Payload};
use mostro_core::transport::unwrap_message_nip44;
use nostr_sdk::prelude::*;
use tracing::warn;

/// Separates `Dispute <id>` from the subject on the header line.
const HEADER_SEPARATOR: &str = " · ";

/// Starts the header line, before the dispute id.
const HEADER_PREFIX: &str = "Dispute ";

const MEDIATING: &str = "mediating";
const COULD_NOT_START: &str = "mediation could not start";
const HANDED_OFF: &str = "handed off";
const GUIDANCE_SENT: &str = "guidance sent";

/// Longest handoff reason or guidance path accepted. Serbero's are short
/// snake_case words; anything longer is not one of them.
const MAX_DETAIL_LEN: usize = 64;

/// What Serbero says about a dispute, for the subjects the watchdog acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Update {
    /// `mediating`: Serbero started talking to the parties.
    Mediating,
    /// `guidance sent: <path>`: Serbero showed the parties how to resolve
    /// the dispute themselves.
    GuidanceSent { path: Option<String> },
    /// `handed off: <reason>`: a human solver must take the dispute over.
    HandedOff { reason: Option<String> },
    /// `mediation could not start`: Serbero took the dispute but cannot
    /// reach the parties, so a human solver must take it over.
    CouldNotStart,
}

impl Update {
    /// Classifies a header subject. Every subject other than the four the
    /// watchdog relays (`new`, `taken`, `transcript`, …) yields `None`.
    pub fn from_subject(subject: &str) -> Option<Self> {
        let subject = subject.trim();
        if after_word(subject, MEDIATING).is_some() {
            Some(Self::Mediating)
        } else if after_word(subject, COULD_NOT_START).is_some() {
            Some(Self::CouldNotStart)
        } else if let Some(rest) = after_word(subject, HANDED_OFF) {
            Some(Self::HandedOff {
                reason: detail(rest),
            })
        } else {
            after_word(subject, GUIDANCE_SENT).map(|rest| Self::GuidanceSent { path: detail(rest) })
        }
    }

    /// The canonical subject, as stored and used to deduplicate alerts.
    /// Parsing it with [`Update::from_subject`] gives back the same update.
    pub fn subject(&self) -> String {
        match self {
            Self::Mediating => MEDIATING.to_owned(),
            Self::CouldNotStart => COULD_NOT_START.to_owned(),
            Self::HandedOff { reason } => with_detail(HANDED_OFF, reason.as_deref()),
            Self::GuidanceSent { path } => with_detail(GUIDANCE_SENT, path.as_deref()),
        }
    }

    /// Position in a mediation's lifecycle, to order two updates written in
    /// the same second.
    pub fn stage(&self) -> u8 {
        match self {
            Self::Mediating => 0,
            Self::GuidanceSent { .. } => 1,
            Self::HandedOff { .. } | Self::CouldNotStart => 2,
        }
    }
}

/// What follows `word` at the start of `subject`, when `word` is a whole
/// word there (`mediatingx` is not `mediating`).
fn after_word<'a>(subject: &'a str, word: &str) -> Option<&'a str> {
    let rest = subject.strip_prefix(word)?;
    match rest.chars().next() {
        None | Some(' ' | ':' | '(') => Some(rest),
        Some(_) => None,
    }
}

/// The reason or path after `handed off:` or `guidance sent:`: one
/// snake_case word. Anything else is dropped, so free text can never reach
/// Telegram even if a future Serbero put some there.
fn detail(rest: &str) -> Option<String> {
    let word = rest.strip_prefix(':')?.split_whitespace().next()?;
    is_detail(word).then(|| word.to_owned())
}

fn is_detail(word: &str) -> bool {
    !word.is_empty()
        && word.len() <= MAX_DETAIL_LEN
        && word
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn with_detail(subject: &str, detail: Option<&str>) -> String {
    match detail {
        Some(detail) => format!("{subject}: {detail}"),
        None => subject.to_owned(),
    }
}

/// A `send-dm` text from Serbero, reduced to what its header line says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerberoDm {
    /// The dispute: the message id, or the id named in the header.
    pub dispute_id: Option<String>,
    /// The header's subject, when it is one the watchdog acts on.
    pub update: Option<Update>,
    /// More lines followed the header: Serbero is sending this key full
    /// solver messages, so it is registered as a solver, not an observer.
    pub full_message: bool,
    /// Event time in seconds, capped at the time it was read.
    pub created_at: u64,
}

/// One update about one dispute, ready to be relayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderUpdate {
    pub dispute_id: String,
    pub update: Update,
    pub created_at: u64,
}

impl SerberoDm {
    /// The update to relay, when the message names a dispute and a subject
    /// the watchdog acts on.
    pub fn into_update(self) -> Option<HeaderUpdate> {
        Some(HeaderUpdate {
            dispute_id: self.dispute_id?,
            update: self.update?,
            created_at: self.created_at,
        })
    }
}

/// Opens `event` as a Serbero DM when it is a `send-dm` text written by
/// `serbero` to `receiver`. Anything else (another author, another action
/// or payload, not decryptable with `receiver`, a bad signature) is `None`.
pub fn parse_dm(event: &Event, receiver: &Keys, serbero: &PublicKey) -> Option<SerberoDm> {
    if event.kind != Kind::PrivateDirectMessage || event.pubkey != *serbero {
        return None;
    }
    let opened = match unwrap_message_nip44(event, receiver) {
        Ok(Some(opened)) => opened,
        // Encrypted to another key.
        Ok(None) => return None,
        Err(e) => {
            warn!(event_id = %event.id, error = %e, "Ignoring a Serbero DM that cannot be opened");
            return None;
        }
    };
    if opened.identity != *serbero {
        return None;
    }
    let kind = opened.message.get_inner_message_kind();
    let (Action::SendDm, Some(Payload::TextMessage(text))) = (&kind.action, &kind.payload) else {
        return None;
    };
    let mut lines = text.lines();
    let header = lines.next().unwrap_or_default();
    let full_message = lines.next().is_some();
    let (header_id, subject) = split_header(header).unzip();
    Some(SerberoDm {
        dispute_id: kind
            .id
            .map(|id| id.to_string())
            .or_else(|| header_id.and_then(canonical_uuid)),
        update: subject.and_then(Update::from_subject),
        full_message,
        created_at: opened.created_at.as_secs().min(Timestamp::now().as_secs()),
    })
}

/// Splits a header line into the dispute id token and the subject.
pub fn split_header(line: &str) -> Option<(&str, &str)> {
    let (head, subject) = line.trim().split_once(HEADER_SEPARATOR)?;
    let dispute_id = head.strip_prefix(HEADER_PREFIX)?.trim();
    Some((dispute_id, subject.trim()))
}

/// A dispute id named in a header, in the form Mostro publishes it
/// (hyphenated, lowercase), when it is a UUID.
fn canonical_uuid(token: &str) -> Option<String> {
    uuid::Uuid::try_parse(token).ok().map(|id| id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mostro_core::message::Message;
    use mostro_core::transport::{wrap_message_nip44, WrapOptions};
    use uuid::Uuid;

    const DISPUTE: &str = "58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a";

    /// A DM built exactly as Serbero builds it (`src/nostr/dm.rs`).
    fn dm_event(
        author: &Keys,
        to: PublicKey,
        id: Option<Uuid>,
        action: Action,
        payload: Option<Payload>,
    ) -> Event {
        let message = Message::new_dm(id, None, action, payload);
        wrap_message_nip44(&message, author, author, to, WrapOptions::default()).unwrap()
    }

    fn text(t: &str) -> Option<Payload> {
        Some(Payload::TextMessage(t.to_owned()))
    }

    fn dispute() -> Uuid {
        Uuid::parse_str(DISPUTE).unwrap()
    }

    #[test]
    fn reads_the_header_of_a_send_dm_from_serbero() {
        let serbero = Keys::generate();
        let watchdog = Keys::generate();
        let body = format!("Dispute {DISPUTE} · handed off: conflicting_claims");
        let event = dm_event(
            &serbero,
            watchdog.public_key(),
            Some(dispute()),
            Action::SendDm,
            text(&body),
        );

        let dm = parse_dm(&event, &watchdog, &serbero.public_key()).expect("accepted");

        assert_eq!(dm.dispute_id.as_deref(), Some(DISPUTE));
        assert_eq!(
            dm.update,
            Some(Update::HandedOff {
                reason: Some("conflicting_claims".into())
            })
        );
        assert!(!dm.full_message);
        assert_eq!(dm.created_at, event.created_at.as_secs());
    }

    #[test]
    fn ignores_a_send_dm_from_another_author() {
        let stranger = Keys::generate();
        let watchdog = Keys::generate();
        let body = format!("Dispute {DISPUTE} · handed off: fraud_signal");
        let event = dm_event(
            &stranger,
            watchdog.public_key(),
            Some(dispute()),
            Action::SendDm,
            text(&body),
        );

        let serbero = Keys::generate().public_key();
        assert_eq!(parse_dm(&event, &watchdog, &serbero), None);
    }

    #[test]
    fn ignores_protocol_actions_from_serbero() {
        let serbero = Keys::generate();
        let watchdog = Keys::generate();
        let event = dm_event(
            &serbero,
            watchdog.public_key(),
            Some(dispute()),
            Action::AdminSettle,
            None,
        );

        assert_eq!(parse_dm(&event, &watchdog, &serbero.public_key()), None);
    }

    #[test]
    fn ignores_a_send_dm_without_a_text_payload() {
        let serbero = Keys::generate();
        let watchdog = Keys::generate();
        for payload in [None, Some(Payload::Dispute(dispute(), None))] {
            let event = dm_event(
                &serbero,
                watchdog.public_key(),
                Some(dispute()),
                Action::SendDm,
                payload,
            );

            assert_eq!(parse_dm(&event, &watchdog, &serbero.public_key()), None);
        }
    }

    #[test]
    fn ignores_a_dm_written_to_another_key() {
        let serbero = Keys::generate();
        let someone_else = Keys::generate().public_key();
        let body = format!("Dispute {DISPUTE} · mediating");
        let event = dm_event(
            &serbero,
            someone_else,
            Some(dispute()),
            Action::SendDm,
            text(&body),
        );

        let watchdog = Keys::generate();
        assert_eq!(parse_dm(&event, &watchdog, &serbero.public_key()), None);
    }

    #[test]
    fn ignores_events_that_are_not_direct_messages() {
        let serbero = Keys::generate();
        let watchdog = Keys::generate();
        let event = EventBuilder::new(Kind::TextNote, format!("Dispute {DISPUTE} · mediating"))
            .finalize(&serbero)
            .unwrap();

        assert_eq!(parse_dm(&event, &watchdog, &serbero.public_key()), None);
    }

    #[test]
    fn takes_the_dispute_from_the_header_when_the_message_has_no_id() {
        let serbero = Keys::generate();
        let watchdog = Keys::generate();
        let body = format!("Dispute {} · mediating", DISPUTE.to_uppercase());
        let event = dm_event(
            &serbero,
            watchdog.public_key(),
            None,
            Action::SendDm,
            text(&body),
        );

        let dm = parse_dm(&event, &watchdog, &serbero.public_key()).expect("accepted");

        assert_eq!(dm.dispute_id.as_deref(), Some(DISPUTE));
        assert_eq!(dm.update, Some(Update::Mediating));
    }

    #[test]
    fn a_header_that_names_no_uuid_gives_no_dispute() {
        let serbero = Keys::generate();
        let watchdog = Keys::generate();
        let event = dm_event(
            &serbero,
            watchdog.public_key(),
            None,
            Action::SendDm,
            text("Dispute d1 · mediating"),
        );

        let dm = parse_dm(&event, &watchdog, &serbero.public_key()).expect("accepted");

        assert_eq!(dm.dispute_id, None);
        assert_eq!(dm.clone().into_update(), None);
    }

    #[test]
    fn only_the_header_of_a_full_brief_is_kept() {
        // A watchdog registered as a solver by mistake gets whole briefs,
        // whose later lines quote the parties.
        let serbero = Keys::generate();
        let watchdog = Keys::generate();
        let body = format!(
            "Dispute {DISPUTE} · handed off: conflicting_claims\n\
             Buyer — says sent (0.96)\n  \"PARTY-SECRET ya envié el pago\""
        );
        let event = dm_event(
            &serbero,
            watchdog.public_key(),
            Some(dispute()),
            Action::SendDm,
            text(&body),
        );

        let dm = parse_dm(&event, &watchdog, &serbero.public_key()).expect("accepted");

        assert!(dm.full_message);
        assert!(!format!("{dm:?}").contains("PARTY-SECRET"));
        assert_eq!(
            dm.into_update(),
            Some(HeaderUpdate {
                dispute_id: DISPUTE.into(),
                update: Update::HandedOff {
                    reason: Some("conflicting_claims".into())
                },
                created_at: event.created_at.as_secs(),
            })
        );
    }

    #[test]
    fn solver_only_subjects_are_not_relayed() {
        let serbero = Keys::generate();
        let watchdog = Keys::generate();
        let body =
            format!("Dispute {DISPUTE} · transcript (2 messages, times UTC)\n[14:52] seller: hola");
        let event = dm_event(
            &serbero,
            watchdog.public_key(),
            Some(dispute()),
            Action::SendDm,
            text(&body),
        );

        let dm = parse_dm(&event, &watchdog, &serbero.public_key()).expect("accepted");

        assert_eq!(dm.update, None);
        assert!(dm.full_message);
        assert_eq!(dm.into_update(), None);
    }

    #[test]
    fn a_timestamp_in_the_future_is_capped_at_now() {
        let serbero = Keys::generate();
        let watchdog = Keys::generate();
        let message = Message::new_dm(
            Some(dispute()),
            None,
            Action::SendDm,
            text(&format!("Dispute {DISPUTE} · mediating")),
        );
        let event = wrap_message_nip44(
            &message,
            &serbero,
            &serbero,
            watchdog.public_key(),
            WrapOptions::default(),
        )
        .unwrap();
        // Re-sign the same content with a created_at far in the future.
        let future = Timestamp::now() + 86_400u64;
        let event = EventBuilder::new(Kind::PrivateDirectMessage, event.content.clone())
            .tags(event.tags.clone())
            .custom_created_at(future)
            .finalize(&serbero)
            .unwrap();

        let dm = parse_dm(&event, &watchdog, &serbero.public_key()).expect("accepted");

        assert!(dm.created_at <= Timestamp::now().as_secs());
    }

    #[test]
    fn classifies_the_four_relayed_subjects() {
        assert_eq!(Update::from_subject("mediating"), Some(Update::Mediating));
        assert_eq!(
            Update::from_subject("mediation could not start"),
            Some(Update::CouldNotStart)
        );
        assert_eq!(
            Update::from_subject("handed off: round_limit"),
            Some(Update::HandedOff {
                reason: Some("round_limit".into())
            })
        );
        assert_eq!(
            Update::from_subject("guidance sent: payment_arrived"),
            Some(Update::GuidanceSent {
                path: Some("payment_arrived".into())
            })
        );
    }

    #[test]
    fn every_documented_reason_and_path_is_accepted() {
        for reason in [
            "self_resolution_stalled",
            "facts_gathered",
            "conflicting_claims",
            "fraud_signal",
            "human_requested",
            "outside_scope",
            "unresponsive",
            "round_limit",
            "uncertain",
            "judge_unavailable",
            "flood",
            "opening_failed",
        ] {
            assert_eq!(
                Update::from_subject(&format!("handed off: {reason}")),
                Some(Update::HandedOff {
                    reason: Some(reason.into())
                }),
                "{reason}"
            );
        }
        for path in ["payment_arrived", "payment_not_sent"] {
            assert_eq!(
                Update::from_subject(&format!("guidance sent: {path}")),
                Some(Update::GuidanceSent {
                    path: Some(path.into())
                }),
                "{path}"
            );
        }
    }

    #[test]
    fn other_subjects_are_ignored() {
        for subject in [
            "new",
            "unattended (32 min)",
            "taken",
            "transcript (18 messages, times UTC)",
            "transcript (18 messages, times UTC), part 1/2",
            "new messages since handoff (2)",
            "resolved: settled",
            "mediatingx",
            "handed offx",
            "",
        ] {
            assert_eq!(Update::from_subject(subject), None, "{subject:?}");
        }
    }

    #[test]
    fn free_text_never_passes_as_a_reason_or_path() {
        // Still a handoff, so the alert goes out, but without the text.
        for subject in [
            "handed off: Ya envié el pago!",
            "handed off: <b>x</b>",
            "handed off:",
            "handed off",
        ] {
            assert_eq!(
                Update::from_subject(subject),
                Some(Update::HandedOff { reason: None }),
                "{subject:?}"
            );
        }
        let long = format!("guidance sent: {}", "a".repeat(MAX_DETAIL_LEN + 1));
        assert_eq!(
            Update::from_subject(&long),
            Some(Update::GuidanceSent { path: None })
        );
    }

    #[test]
    fn extra_words_after_a_reason_are_dropped() {
        assert_eq!(
            Update::from_subject("handed off: flood (12 messages)"),
            Some(Update::HandedOff {
                reason: Some("flood".into())
            })
        );
    }

    #[test]
    fn the_canonical_subject_parses_back_to_the_same_update() {
        for update in [
            Update::Mediating,
            Update::CouldNotStart,
            Update::HandedOff { reason: None },
            Update::HandedOff {
                reason: Some("uncertain".into()),
            },
            Update::GuidanceSent { path: None },
            Update::GuidanceSent {
                path: Some("payment_not_sent".into()),
            },
        ] {
            assert_eq!(
                Update::from_subject(&update.subject()),
                Some(update.clone())
            );
        }
        assert_eq!(
            Update::HandedOff {
                reason: Some("flood".into())
            }
            .subject(),
            "handed off: flood"
        );
    }

    #[test]
    fn later_stages_rank_higher() {
        assert!(Update::Mediating.stage() < Update::GuidanceSent { path: None }.stage());
        assert!(
            Update::GuidanceSent { path: None }.stage()
                < Update::HandedOff { reason: None }.stage()
        );
        assert!(Update::Mediating.stage() < Update::CouldNotStart.stage());
    }

    #[test]
    fn splits_the_header_line() {
        assert_eq!(
            split_header(&format!("Dispute {DISPUTE} · resolved: settled")),
            Some((DISPUTE, "resolved: settled"))
        );
        assert_eq!(split_header("New Mostro dispute"), None);
        assert_eq!(split_header("Order x · mediating"), None);
    }
}
