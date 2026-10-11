//! The Telegram calls Serbero alerts make, behind a trait so the relay logic
//! is tested without Telegram.

use std::future::Future;
use std::time::Duration;

use teloxide::prelude::*;
use teloxide::types::{MessageId, ParseMode, ReplyParameters};
use teloxide::{ApiError, RequestError};
use tracing::{debug, warn};

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

    /// Sends `text` as a reply to message `reply_to` and deletes it
    /// `lifetime` later: the notification an edit of `reply_to` does not
    /// give. Best effort, in the background: a failure is logged, and the
    /// call returns as soon as the work is scheduled.
    fn nudge(
        &self,
        chat_id: i64,
        reply_to: i32,
        text: &str,
        lifetime: Duration,
    ) -> impl Future<Output = ()> + Send;
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

    async fn nudge(&self, chat_id: i64, reply_to: i32, text: &str, lifetime: Duration) {
        let bot = self.clone();
        let text = text.to_owned();
        tokio::spawn(async move {
            let sent = bot
                .send_message(ChatId(chat_id), text)
                .parse_mode(ParseMode::MarkdownV2)
                .reply_parameters(ReplyParameters::new(MessageId(reply_to)))
                .await;
            let message_id = match sent {
                Ok(sent) => sent.id,
                Err(e) => {
                    warn!(chat_id, reply_to, error = %e, "Failed to send the edit notification");
                    return;
                }
            };
            tokio::time::sleep(lifetime).await;
            match bot.delete_message(ChatId(chat_id), message_id).await {
                Ok(_) => debug!(chat_id, reply_to, "Edit notification sent and deleted"),
                Err(e) => warn!(
                    chat_id,
                    reply_to,
                    error = %e,
                    "Failed to delete the edit notification; it stays in the channel"
                ),
            }
        });
    }
}
