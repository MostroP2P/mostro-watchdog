//! The Telegram calls Serbero alerts make, behind a trait so the relay logic
//! is tested without Telegram.

use std::future::Future;

use teloxide::prelude::*;
use teloxide::types::{MessageId, ParseMode};
use teloxide::{ApiError, RequestError};

/// Sends and edits MarkdownV2 messages.
pub trait Messenger: Sync {
    /// Sends `text`. Returns the id of the sent message.
    fn send(
        &self,
        chat_id: i64,
        text: &str,
    ) -> impl Future<Output = Result<i32, RequestError>> + Send;

    /// Replaces the text of message `message_id`.
    fn edit(
        &self,
        chat_id: i64,
        message_id: i32,
        text: &str,
    ) -> impl Future<Output = Result<(), RequestError>> + Send;
}

impl Messenger for Bot {
    async fn send(&self, chat_id: i64, text: &str) -> Result<i32, RequestError> {
        self.send_message(ChatId(chat_id), text)
            .parse_mode(ParseMode::MarkdownV2)
            .await
            .map(|sent| sent.id.0)
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
