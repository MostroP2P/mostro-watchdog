//! The Telegram calls Serbero alerts make, behind a trait so the relay logic
//! is tested without Telegram.

use std::future::Future;

use teloxide::prelude::*;
use teloxide::types::{MessageId, ParseMode, ReplyParameters};
use teloxide::{ApiError, RequestError};

/// Sends and edits MarkdownV2 messages.
pub trait Messenger: Sync {
    /// Sends `text`, as a reply to message `reply_to` when given.
    fn send(
        &self,
        chat_id: i64,
        text: &str,
        reply_to: Option<i32>,
    ) -> impl Future<Output = Result<(), RequestError>> + Send;

    /// Replaces the text of message `message_id`.
    fn edit(
        &self,
        chat_id: i64,
        message_id: i32,
        text: &str,
    ) -> impl Future<Output = Result<(), RequestError>> + Send;
}

impl Messenger for Bot {
    async fn send(
        &self,
        chat_id: i64,
        text: &str,
        reply_to: Option<i32>,
    ) -> Result<(), RequestError> {
        let request = self
            .send_message(ChatId(chat_id), text)
            .parse_mode(ParseMode::MarkdownV2);
        let request = match reply_to {
            // Still sent if the dispute's message was deleted meanwhile.
            Some(id) => request.reply_parameters(
                ReplyParameters::new(MessageId(id)).allow_sending_without_reply(),
            ),
            None => request,
        };
        request.await.map(|_| ())
    }

    async fn edit(&self, chat_id: i64, message_id: i32, text: &str) -> Result<(), RequestError> {
        let result = self
            .edit_message_text(ChatId(chat_id), MessageId(message_id), text)
            .parse_mode(ParseMode::MarkdownV2)
            .await;
        match result {
            Ok(_) => Ok(()),
            // The message already shows this text: a retried update.
            Err(RequestError::Api(ApiError::MessageNotModified)) => Ok(()),
            Err(e) => Err(e),
        }
    }
}
