//! `/link`, `/unlink` and `/status`: the Telegram side of linking a solver
//! key. Answered in private chats only (`is_answerable_chat`).

use nostr_sdk::prelude::{PublicKey, ToBech32};
use tracing::error;

use super::code::{self, CODE_TTL};
use super::store::SolverStore;
use crate::{escape_markdown, escape_markdown_code};

/// The answer to every solver command when `[solver_notifications]` is off.
pub const DISABLED_TEXT: &str = "Solver notifications are not enabled on this watchdog\\.";

/// Shown when the store fails; the error itself goes to the log.
const FAILURE_TEXT: &str = "Something went wrong; please try again in a moment\\.";

#[derive(Clone)]
pub struct SolverCommands {
    store: SolverStore,
    /// The watchdog's key, entered in Mostrix next to the code.
    watchdog: PublicKey,
}

impl SolverCommands {
    pub fn new(store: SolverStore, watchdog: PublicKey) -> Self {
        Self { store, watchdog }
    }

    /// Issues a one-time code for `chat_id` and says how to use it.
    pub async fn link(&self, chat_id: i64, now: u64) -> String {
        let code = code::generate();
        let now = seconds(now);
        let expires_at = now.saturating_add(seconds(CODE_TTL.as_secs()));
        if let Err(e) = self
            .store
            .create_link_code(&code, chat_id, expires_at, now)
            .await
        {
            error!(chat_id, error = %e, "Failed to store a solver link code");
            return FAILURE_TEXT.to_owned();
        }
        link_text(&npub(&self.watchdog), &code, CODE_TTL.as_secs() / 60)
    }

    /// Unlinks the chat's solver keys.
    pub async fn unlink(&self, chat_id: i64) -> String {
        match self.store.unlink_chat(chat_id).await {
            Ok(0) => "No solver key is linked to this chat\\.".to_owned(),
            Ok(count) => escape_markdown(&format!(
                "🔓 Unlinked {count} solver key{}. You will no longer get dispute chat \
                 notifications here.",
                plural(count)
            )),
            Err(e) => {
                error!(chat_id, error = %e, "Failed to unlink solver keys");
                FAILURE_TEXT.to_owned()
            }
        }
    }

    /// Lists the chat's solver keys and what they watch.
    pub async fn status(&self, chat_id: i64) -> String {
        let links = match self.store.links_of_chat(chat_id).await {
            Ok(links) => links,
            Err(e) => {
                error!(chat_id, error = %e, "Failed to read linked solver keys");
                return FAILURE_TEXT.to_owned();
            }
        };
        if links.is_empty() {
            return "No solver key is linked to this chat\\. Send /link to link one\\.".to_owned();
        }
        let lines: Vec<String> = links
            .iter()
            .map(|link| {
                let key = PublicKey::from_hex(&link.solver_pubkey)
                    .map(|key| npub(&key))
                    .unwrap_or_else(|_| link.solver_pubkey.clone());
                let count = u64::try_from(link.watched_disputes).unwrap_or(0);
                format!(
                    "• `{}` {}",
                    escape_markdown_code(&key),
                    escape_markdown(&format!("— watching {count} dispute{}", plural(count)))
                )
            })
            .collect();
        format!("🔗 *Linked solver keys*\n\n{}", lines.join("\n"))
    }
}

fn link_text(watchdog: &str, code: &str, minutes: u64) -> String {
    format!(
        "🔗 *Link your solver key*\n\n\
         In Mostrix, link the watchdog with:\n\n\
         🔑 *Watchdog key:* `{}`\n\
         🔢 *Code:* `{}`\n\n\
         {}",
        escape_markdown_code(watchdog),
        escape_markdown_code(code),
        escape_markdown(&format!(
            "The code works once and expires in {minutes} minutes."
        )),
    )
}

fn npub(key: &PublicKey) -> String {
    key.to_bech32().unwrap_or_else(|_| key.to_hex())
}

fn plural(count: u64) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

fn seconds(secs: u64) -> i64 {
    i64::try_from(secs).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DisputeMessageStore;
    use crate::solver::protocol::{Conversation, Party};
    use crate::solver::store::Redeemed;
    use nostr_sdk::prelude::Keys;

    const CHAT: i64 = 4242;
    const T0: u64 = 1_700_000_000;

    struct Fixture {
        _dir: tempfile::TempDir,
        commands: SolverCommands,
        store: SolverStore,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let disputes = DisputeMessageStore::new(&dir.path().join("disputes.db"))
            .await
            .unwrap();
        let store = SolverStore::new(disputes.pool()).await.unwrap();
        let commands = SolverCommands::new(store.clone(), Keys::generate().public_key());
        Fixture {
            _dir: dir,
            commands,
            store,
        }
    }

    async fn link(fx: &Fixture, solver: &PublicKey) {
        let code = code_in(&fx.commands.link(CHAT, T0).await);
        let linked = fx.store.redeem_link_code(&code, solver, T0 as i64).await;
        assert_eq!(linked.unwrap(), Redeemed::Linked(CHAT));
    }

    /// The code shown in a `/link` answer.
    fn code_in(text: &str) -> String {
        let after = text.split("*Code:* `").nth(1).expect("a code");
        after.split('`').next().unwrap().to_owned()
    }

    #[tokio::test]
    async fn link_issues_a_code_that_expires_in_ten_minutes() {
        let fx = fixture().await;

        let text = fx.commands.link(CHAT, T0).await;
        let code = code_in(&text);

        assert!(text.contains("*Watchdog key:* `npub1"), "{text}");
        assert!(text.ends_with("expires in 10 minutes\\."), "{text}");
        assert_eq!(
            fx.store
                .redeem_link_code(&code, &Keys::generate().public_key(), (T0 + 599) as i64)
                .await
                .unwrap(),
            Redeemed::Linked(CHAT)
        );
    }

    #[tokio::test]
    async fn a_code_is_not_used_after_ten_minutes() {
        let fx = fixture().await;

        let code = code_in(&fx.commands.link(CHAT, T0).await);

        assert_eq!(
            fx.store
                .redeem_link_code(&code, &Keys::generate().public_key(), (T0 + 600) as i64)
                .await
                .unwrap(),
            Redeemed::UnknownCode
        );
    }

    #[tokio::test]
    async fn status_lists_linked_keys_and_watched_disputes() {
        let fx = fixture().await;
        let solver = Keys::generate().public_key();
        link(&fx, &solver).await;
        fx.store
            .apply_watch(
                &solver,
                "58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a",
                &[Conversation {
                    party: Party::Buyer,
                    sign_pubkey: Keys::generate().public_key(),
                }],
                10,
            )
            .await
            .unwrap();

        let text = fx.commands.status(CHAT).await;

        assert_eq!(
            text,
            format!(
                "🔗 *Linked solver keys*\n\n• `{}` — watching 1 dispute",
                solver.to_bech32().unwrap()
            )
        );
    }

    #[tokio::test]
    async fn status_and_unlink_say_when_nothing_is_linked() {
        let fx = fixture().await;

        assert!(fx.commands.status(CHAT).await.starts_with("No solver key"));
        assert!(fx.commands.unlink(CHAT).await.starts_with("No solver key"));
    }

    #[tokio::test]
    async fn unlink_removes_the_chats_keys() {
        let fx = fixture().await;
        let solver = Keys::generate().public_key();
        link(&fx, &solver).await;

        let text = fx.commands.unlink(CHAT).await;

        assert_eq!(
            text,
            "🔓 Unlinked 1 solver key\\. You will no longer get dispute chat notifications here\\."
        );
        assert_eq!(fx.store.chat_of(&solver).await.unwrap(), None);
    }
}
