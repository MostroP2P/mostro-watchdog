//! The messages Mostrix sends the watchdog (SOLVER_NOTIFICATIONS.md,
//! "Protocol"): `link`, `watch`, `sent` and `unwatch`, as versioned JSON in a
//! Mostro protocol v2 `send-dm`.

use mostro_core::message::{Action, Payload};
use mostro_core::transport::unwrap_message_nip44;
use nostr_sdk::prelude::*;
use serde::Deserialize;
use serde_json::Value;
use tracing::debug;

use super::code;

/// The only protocol version this watchdog speaks.
pub const PROTOCOL_VERSION: u64 = 1;

/// Conversations in one dispute: the solver with each party.
pub const MAX_CONVERSATIONS: usize = 2;

/// Longest message text read; real ones are a few hundred bytes.
const MAX_TEXT_LEN: usize = 4096;

/// The party on the other side of a solver's conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Party {
    Buyer,
    Seller,
}

impl Party {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Buyer => "buyer",
            Self::Seller => "seller",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "buyer" => Some(Self::Buyer),
            "seller" => Some(Self::Seller),
            _ => None,
        }
    }
}

/// One conversation of a dispute, by the public key that signs its messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    pub party: Party,
    pub sign_pubkey: PublicKey,
}

/// A message from a solver's Mostrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SolverMessage {
    /// Tie the sender's key to the Telegram chat that asked for `code`.
    Link { code: String },
    /// Follow these conversations of the dispute.
    Watch {
        dispute_id: String,
        conversations: Vec<Conversation>,
    },
    /// The solver wrote chat event `event_id`: never notify it.
    Sent { event_id: EventId },
    /// Stop following the dispute.
    Unwatch { dispute_id: String },
}

/// A message, who sent it and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    /// The solver key proven inside the ciphertext.
    pub identity: PublicKey,
    pub message: SolverMessage,
    /// Event time in seconds, capped at the time it was read.
    pub created_at: u64,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Wire {
    Link {
        code: String,
    },
    Watch {
        dispute_id: String,
        conversations: Vec<WireConversation>,
    },
    Sent {
        event_id: String,
    },
    Unwatch {
        dispute_id: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireConversation {
    party: Party,
    sign_pubkey: String,
}

/// Why a message text was rejected. Never quotes the text.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("the message is too long")]
    TooLong,
    #[error("the message is not a JSON object")]
    NotAnObject,
    #[error("unsupported protocol version")]
    UnsupportedVersion,
    /// Not one of the four messages. Serde's own error is not kept: it
    /// quotes the unknown names and values it met.
    #[error("malformed message")]
    Malformed,
    #[error("invalid link code")]
    InvalidCode,
    #[error("invalid dispute id")]
    InvalidDisputeId,
    #[error("invalid sign_pubkey")]
    InvalidSignPubkey,
    #[error("invalid event_id")]
    InvalidEventId,
    #[error("a watch needs one or two conversations, each party at most once")]
    InvalidConversations,
}

/// Parses a message text.
pub fn parse_text(text: &str) -> Result<SolverMessage, ProtocolError> {
    if text.len() > MAX_TEXT_LEN {
        return Err(ProtocolError::TooLong);
    }
    let Ok(Value::Object(mut object)) = serde_json::from_str::<Value>(text) else {
        return Err(ProtocolError::NotAnObject);
    };
    if object.remove("v").and_then(|v| v.as_u64()) != Some(PROTOCOL_VERSION) {
        return Err(ProtocolError::UnsupportedVersion);
    }
    let wire: Wire =
        serde_json::from_value(Value::Object(object)).map_err(|_| ProtocolError::Malformed)?;
    match wire {
        Wire::Link { code } => Ok(SolverMessage::Link {
            code: code::normalize(&code).ok_or(ProtocolError::InvalidCode)?,
        }),
        Wire::Watch {
            dispute_id,
            conversations,
        } => Ok(SolverMessage::Watch {
            dispute_id: dispute_id_of(&dispute_id)?,
            conversations: conversations_of(conversations)?,
        }),
        Wire::Sent { event_id } => Ok(SolverMessage::Sent {
            event_id: EventId::from_hex(&event_id).map_err(|_| ProtocolError::InvalidEventId)?,
        }),
        Wire::Unwatch { dispute_id } => Ok(SolverMessage::Unwatch {
            dispute_id: dispute_id_of(&dispute_id)?,
        }),
    }
}

/// A dispute id in the form Mostro publishes it (hyphenated, lowercase).
fn dispute_id_of(text: &str) -> Result<String, ProtocolError> {
    uuid::Uuid::try_parse(text)
        .map(|id| id.to_string())
        .map_err(|_| ProtocolError::InvalidDisputeId)
}

fn conversations_of(wire: Vec<WireConversation>) -> Result<Vec<Conversation>, ProtocolError> {
    if wire.is_empty() || wire.len() > MAX_CONVERSATIONS {
        return Err(ProtocolError::InvalidConversations);
    }
    let conversations = wire
        .into_iter()
        .map(|c| {
            Ok(Conversation {
                party: c.party,
                sign_pubkey: PublicKey::from_hex(&c.sign_pubkey)
                    .map_err(|_| ProtocolError::InvalidSignPubkey)?,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let repeated = conversations.len() == MAX_CONVERSATIONS
        && (conversations[0].party == conversations[1].party
            || conversations[0].sign_pubkey == conversations[1].sign_pubkey);
    if repeated {
        return Err(ProtocolError::InvalidConversations);
    }
    Ok(conversations)
}

/// A `send-dm` text encrypted to the watchdog that is not a valid message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub identity: PublicKey,
    pub error: ProtocolError,
}

/// Opens `event` as a message from a solver's Mostrix, when it is a
/// `send-dm` text encrypted to `watchdog`. Anything else is `None`. Nothing
/// is logged here: whether a rejection matters depends on whether the
/// sender is linked.
pub fn parse_solver_dm(event: &Event, watchdog: &Keys) -> Option<Result<Received, Rejected>> {
    if event.kind != Kind::PrivateDirectMessage {
        return None;
    }
    let opened = match unwrap_message_nip44(event, watchdog) {
        Ok(Some(opened)) => opened,
        Ok(None) => return None,
        Err(e) => {
            debug!(event_id = %event.id, error = %e, "Ignoring a DM that cannot be opened");
            return None;
        }
    };
    let kind = opened.message.get_inner_message_kind();
    let (Action::SendDm, Some(Payload::TextMessage(text))) = (&kind.action, &kind.payload) else {
        return None;
    };
    match parse_text(text) {
        Ok(message) => Some(Ok(Received {
            identity: opened.identity,
            message,
            created_at: opened.created_at.as_secs().min(Timestamp::now().as_secs()),
        })),
        // Serbero's DMs to the same key are plain text, not this protocol.
        Err(ProtocolError::NotAnObject) => None,
        Err(error) => Some(Err(Rejected {
            identity: opened.identity,
            error,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mostro_core::message::Message;
    use mostro_core::transport::{wrap_message_nip44, WrapOptions};

    const DISPUTE: &str = "58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a";

    fn hex_key() -> String {
        Keys::generate().public_key().to_hex()
    }

    fn watch(conversations: &str) -> String {
        format!(
            r#"{{"v":1,"type":"watch","dispute_id":"{DISPUTE}","conversations":[{conversations}]}}"#
        )
    }

    #[test]
    fn reads_a_link() {
        let message = parse_text(r#"{"v":1,"type":"link","code":"k7qm2xpa"}"#);

        assert_eq!(
            message,
            Ok(SolverMessage::Link {
                code: "K7QM-2XPA".into()
            })
        );
    }

    #[test]
    fn reads_a_watch_of_both_conversations() {
        let (buyer, seller) = (hex_key(), hex_key());
        let text = watch(&format!(
            r#"{{"party":"buyer","sign_pubkey":"{buyer}"}},{{"party":"seller","sign_pubkey":"{seller}"}}"#
        ));

        let message = parse_text(&text).unwrap();

        assert_eq!(
            message,
            SolverMessage::Watch {
                dispute_id: DISPUTE.into(),
                conversations: vec![
                    Conversation {
                        party: Party::Buyer,
                        sign_pubkey: PublicKey::from_hex(&buyer).unwrap(),
                    },
                    Conversation {
                        party: Party::Seller,
                        sign_pubkey: PublicKey::from_hex(&seller).unwrap(),
                    },
                ],
            }
        );
    }

    #[test]
    fn reads_sent_and_unwatch() {
        let id = EventId::from_byte_array([7; 32]).to_hex();

        assert_eq!(
            parse_text(&format!(r#"{{"v":1,"type":"sent","event_id":"{id}"}}"#)),
            Ok(SolverMessage::Sent {
                event_id: EventId::from_byte_array([7; 32])
            })
        );
        assert_eq!(
            parse_text(&format!(
                r#"{{"v":1,"type":"unwatch","dispute_id":"{}"}}"#,
                DISPUTE.to_uppercase()
            )),
            Ok(SolverMessage::Unwatch {
                dispute_id: DISPUTE.into()
            })
        );
    }

    #[test]
    fn rejects_other_versions_and_shapes() {
        let cases = [
            ("not json", ProtocolError::NotAnObject),
            ("[1]", ProtocolError::NotAnObject),
            (
                r#"{"type":"link","code":"K7QM2XPA"}"#,
                ProtocolError::UnsupportedVersion,
            ),
            (
                r#"{"v":2,"type":"link","code":"K7QM2XPA"}"#,
                ProtocolError::UnsupportedVersion,
            ),
            (
                r#"{"v":1,"type":"link","code":"nope"}"#,
                ProtocolError::InvalidCode,
            ),
            (
                r#"{"v":1,"type":"unwatch","dispute_id":"42"}"#,
                ProtocolError::InvalidDisputeId,
            ),
            (
                r#"{"v":1,"type":"sent","event_id":"zz"}"#,
                ProtocolError::InvalidEventId,
            ),
        ];
        for (text, expected) in cases {
            assert_eq!(parse_text(text), Err(expected), "{text}");
        }
    }

    #[test]
    fn rejects_unknown_types_and_fields() {
        for text in [
            r#"{"v":1,"type":"settle","dispute_id":"x"}"#,
            r#"{"v":1,"type":"link","code":"K7QM2XPA","private_key":"nsec1x"}"#,
        ] {
            assert_eq!(parse_text(text), Err(ProtocolError::Malformed), "{text}");
        }
    }

    #[test]
    fn a_rejected_message_never_quotes_unknown_names() {
        for text in [
            r#"{"v":1,"type":"INJECTED\nline","code":"K7QM2XPA"}"#,
            r#"{"v":1,"type":"link","code":"K7QM2XPA","INJECTED":1}"#,
            r#"{"v":1,"type":"link","code":["INJECTED"]}"#,
        ] {
            let err = parse_text(text).unwrap_err();

            assert!(!err.to_string().contains("INJECTED"), "{err}");
        }
    }

    #[test]
    fn a_rejected_message_never_quotes_its_values() {
        let err = parse_text(r#"{"v":1,"type":"link","code":"K7QM2XPA","secret":"PARTY-TEXT"}"#)
            .unwrap_err();

        assert!(!err.to_string().contains("PARTY-TEXT"), "{err}");
    }

    #[test]
    fn a_watch_needs_one_or_two_distinct_conversations() {
        let key = hex_key();
        let one = format!(r#"{{"party":"buyer","sign_pubkey":"{key}"}}"#);
        let same_party = format!(r#"{one},{{"party":"buyer","sign_pubkey":"{}"}}"#, hex_key());
        let same_key = format!(r#"{one},{{"party":"seller","sign_pubkey":"{key}"}}"#);
        let three = format!(
            r#"{one},{{"party":"seller","sign_pubkey":"{}"}},{{"party":"seller","sign_pubkey":"{}"}}"#,
            hex_key(),
            hex_key()
        );

        assert!(parse_text(&watch(&one)).is_ok());
        for conversations in ["", &same_party, &same_key, &three] {
            assert_eq!(
                parse_text(&watch(conversations)),
                Err(ProtocolError::InvalidConversations),
                "{conversations}"
            );
        }
        assert_eq!(
            parse_text(&watch(r#"{"party":"buyer","sign_pubkey":"zz"}"#)),
            Err(ProtocolError::InvalidSignPubkey)
        );
        assert_eq!(
            parse_text(&watch(&format!(
                r#"{{"party":"judge","sign_pubkey":"{key}"}}"#
            ))),
            Err(ProtocolError::Malformed)
        );
    }

    #[test]
    fn an_oversized_message_is_not_parsed() {
        let text = format!(
            r#"{{"v":1,"type":"link","code":"{}"}}"#,
            "A".repeat(MAX_TEXT_LEN)
        );

        assert_eq!(parse_text(&text), Err(ProtocolError::TooLong));
    }

    fn dm(identity: &Keys, trade: &Keys, to: PublicKey, action: Action, text: &str) -> Event {
        let message = Message::new_dm(
            None,
            None,
            action,
            Some(Payload::TextMessage(text.to_owned())),
        );
        wrap_message_nip44(&message, identity, trade, to, WrapOptions::default()).unwrap()
    }

    #[test]
    fn trusts_the_identity_proven_inside_not_the_event_author() {
        let solver = Keys::generate();
        let ephemeral = Keys::generate();
        let watchdog = Keys::generate();
        let event = dm(
            &solver,
            &ephemeral,
            watchdog.public_key(),
            Action::SendDm,
            r#"{"v":1,"type":"link","code":"K7QM2XPA"}"#,
        );

        let received = parse_solver_dm(&event, &watchdog)
            .expect("for the watchdog")
            .expect("valid");

        assert_eq!(event.pubkey, ephemeral.public_key());
        assert_eq!(received.identity, solver.public_key());
        assert_eq!(received.created_at, event.created_at.as_secs());
    }

    #[test]
    fn ignores_dms_for_another_key_other_actions_and_bad_text() {
        let solver = Keys::generate();
        let watchdog = Keys::generate();
        let link = r#"{"v":1,"type":"link","code":"K7QM2XPA"}"#;
        let elsewhere = dm(
            &solver,
            &solver,
            Keys::generate().public_key(),
            Action::SendDm,
            link,
        );
        let other_action = dm(
            &solver,
            &solver,
            watchdog.public_key(),
            Action::AdminSettle,
            link,
        );
        let plain_text = dm(
            &solver,
            &solver,
            watchdog.public_key(),
            Action::SendDm,
            "Dispute 5851 · mediating",
        );

        for event in [elsewhere, other_action, plain_text] {
            assert_eq!(parse_solver_dm(&event, &watchdog), None);
        }
    }

    #[test]
    fn an_invalid_message_names_its_sender() {
        let solver = Keys::generate();
        let watchdog = Keys::generate();
        let event = dm(
            &solver,
            &solver,
            watchdog.public_key(),
            Action::SendDm,
            r#"{"v":9,"type":"link","code":"K7QM2XPA"}"#,
        );

        assert_eq!(
            parse_solver_dm(&event, &watchdog),
            Some(Err(Rejected {
                identity: solver.public_key(),
                error: ProtocolError::UnsupportedVersion,
            }))
        );
    }
}
