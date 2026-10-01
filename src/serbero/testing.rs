//! Test doubles shared by the Serbero tests.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use mostro_core::message::{Action, Message, Payload};
use mostro_core::transport::{wrap_message_nip44, WrapOptions};
use nostr_sdk::prelude::*;
use teloxide::{ApiError, RequestError};

use super::telegram::Messenger;

/// A `send-dm` text built exactly as Serbero builds it (serbero
/// `src/nostr/dm.rs`), dated `created_at` when given.
pub fn serbero_dm(
    serbero: &Keys,
    to: PublicKey,
    dispute_id: Option<&str>,
    text: &str,
    created_at: Option<Timestamp>,
) -> Event {
    let id = dispute_id.map(|id| uuid::Uuid::parse_str(id).unwrap());
    let message = Message::new_dm(
        id,
        None,
        Action::SendDm,
        Some(Payload::TextMessage(text.to_owned())),
    );
    let event = wrap_message_nip44(&message, serbero, serbero, to, WrapOptions::default()).unwrap();
    match created_at {
        // Same content and tags, signed again with another date.
        Some(at) => EventBuilder::new(Kind::PrivateDirectMessage, event.content.clone())
            .tags(event.tags.clone())
            .custom_created_at(at)
            .finalize(serbero)
            .unwrap(),
        None => event,
    }
}

/// A Telegram call made through [`FakeTelegram`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Send {
        chat_id: i64,
        text: String,
        reply_to: Option<i32>,
    },
    Edit {
        chat_id: i64,
        message_id: i32,
        text: String,
    },
}

/// Records the calls it gets; fails them all while `down` is set.
#[derive(Default)]
pub struct FakeTelegram {
    calls: Mutex<Vec<Call>>,
    pub down: AtomicBool,
}

impl FakeTelegram {
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    pub fn sends(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| matches!(c, Call::Send { .. }))
            .collect()
    }

    pub fn edits(&self) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| matches!(c, Call::Edit { .. }))
            .collect()
    }

    pub fn set_down(&self, down: bool) {
        self.down.store(down, Ordering::SeqCst);
    }

    fn reachable(&self) -> Result<(), RequestError> {
        if self.down.load(Ordering::SeqCst) {
            Err(RequestError::Api(ApiError::Unknown("unreachable".into())))
        } else {
            Ok(())
        }
    }
}

impl Messenger for FakeTelegram {
    async fn send(
        &self,
        chat_id: i64,
        text: &str,
        reply_to: Option<i32>,
    ) -> Result<(), RequestError> {
        self.reachable()?;
        self.calls.lock().unwrap().push(Call::Send {
            chat_id,
            text: text.into(),
            reply_to,
        });
        Ok(())
    }

    async fn edit(&self, chat_id: i64, message_id: i32, text: &str) -> Result<(), RequestError> {
        self.reachable()?;
        self.calls.lock().unwrap().push(Call::Edit {
            chat_id,
            message_id,
            text: text.into(),
        });
        Ok(())
    }
}
