//! What solver notifications keep in `disputes.db`: linked solver keys and
//! their Telegram chats, pending link codes, watched conversations, `sent`
//! receipts and the events already handled.
//!
//! Every conversation and receipt belongs to the solver key that sent it:
//! a conversation's signing key is public (it signs every chat event), so
//! another linked key naming it must never take it over or silence it.

use nostr_sdk::prelude::{EventId, PublicKey};
use sqlx::{SqliteConnection, SqlitePool};

use super::protocol::{Conversation, Party};

/// Solver keys one Telegram chat may link.
pub const MAX_KEYS_PER_CHAT: i64 = 5;

/// Disputes one solver key may have watched at once.
pub const MAX_WATCHED_DISPUTES: i64 = 50;

/// `sent` receipts kept per solver key; more are dropped until old ones are
/// pruned.
pub const MAX_RECEIPTS_PER_SOLVER: i64 = 2000;

/// Scope of the handled Mostrix messages, which belong to no solver yet.
pub const MOSTRIX_SCOPE: &str = "mostrix";

/// A watched conversation and where to notify about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedConversation {
    pub dispute_id: String,
    pub solver_pubkey: String,
    pub party: Party,
    /// Chat events created before this (Unix seconds) are not notified.
    pub watched_since: i64,
    /// The Telegram chat linked to the solver.
    pub chat_id: i64,
}

/// A solver key linked to a Telegram chat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedSolver {
    pub solver_pubkey: String,
    pub watched_disputes: i64,
}

/// What redeeming a link code did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redeemed {
    /// The key is linked to this chat.
    Linked(i64),
    /// The chat already links [`MAX_KEYS_PER_CHAT`] other keys.
    TooManyKeys(i64),
    /// No such code, or it expired.
    UnknownCode,
}

/// What a `watch` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watched {
    Applied,
    /// A later `watch` or `unwatch` was applied, or the dispute ended after it.
    Stale,
    /// The key is not linked (any more).
    NotLinked,
    /// The key already watches [`MAX_WATCHED_DISPUTES`] other disputes.
    TooMany,
}

#[derive(Clone)]
pub struct SolverStore {
    pool: SqlitePool,
}

impl SolverStore {
    /// Opens the store on `pool`, creating its tables when missing.
    pub async fn new(pool: SqlitePool) -> Result<Self, sqlx::Error> {
        for statement in SCHEMA {
            sqlx::query(statement).execute(&pool).await?;
        }
        Ok(Self { pool })
    }

    /// Stores `code` for `chat_id`, replacing the chat's previous code and
    /// dropping expired ones.
    pub async fn create_link_code(
        &self,
        code: &str,
        chat_id: i64,
        expires_at: i64,
        now: i64,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM solver_link_codes WHERE chat_id = ? OR expires_at <= ?")
            .bind(chat_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO solver_link_codes (code, chat_id, expires_at) VALUES (?, ?, ?)")
            .bind(code)
            .bind(chat_id)
            .bind(expires_at)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }

    /// Uses up `code` and links `solver` to the chat that asked for it, in
    /// one step: a code is never burnt without linking.
    pub async fn redeem_link_code(
        &self,
        code: &str,
        solver: &PublicKey,
        now: i64,
    ) -> Result<Redeemed, sqlx::Error> {
        let solver = solver.to_hex();
        let mut tx = self.pool.begin().await?;
        let row: Option<(i64,)> = sqlx::query_as(
            "DELETE FROM solver_link_codes WHERE code = ? AND expires_at > ? RETURNING chat_id",
        )
        .bind(code)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((chat_id,)) = row else {
            return Ok(Redeemed::UnknownCode);
        };
        let (others,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM solver_links WHERE chat_id = ? AND solver_pubkey != ?",
        )
        .bind(chat_id)
        .bind(&solver)
        .fetch_one(&mut *tx)
        .await?;
        if others >= MAX_KEYS_PER_CHAT {
            tx.commit().await?;
            return Ok(Redeemed::TooManyKeys(chat_id));
        }
        sqlx::query(
            r#"
            INSERT INTO solver_links (solver_pubkey, chat_id, linked_at) VALUES (?, ?, ?)
            ON CONFLICT(solver_pubkey) DO UPDATE SET
                chat_id = excluded.chat_id,
                linked_at = excluded.linked_at
            "#,
        )
        .bind(&solver)
        .bind(chat_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Redeemed::Linked(chat_id))
    }

    /// The Telegram chat `solver` is linked to.
    pub async fn chat_of(&self, solver: &PublicKey) -> Result<Option<i64>, sqlx::Error> {
        chat_of(&self.pool, &solver.to_hex()).await
    }

    /// The solver keys linked to `chat_id`, oldest first.
    pub async fn links_of_chat(&self, chat_id: i64) -> Result<Vec<LinkedSolver>, sqlx::Error> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            r#"
            SELECT l.solver_pubkey,
                   (SELECT COUNT(*) FROM solver_watches w
                    WHERE w.solver_pubkey = l.solver_pubkey AND w.active = 1)
            FROM solver_links l WHERE l.chat_id = ? ORDER BY l.linked_at, l.solver_pubkey
            "#,
        )
        .bind(chat_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(solver_pubkey, watched_disputes)| LinkedSolver {
                solver_pubkey,
                watched_disputes,
            })
            .collect())
    }

    /// Unlinks every solver key of `chat_id` and stops watching their
    /// disputes. Returns how many keys were unlinked.
    pub async fn unlink_chat(&self, chat_id: i64) -> Result<u64, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let of_chat = "SELECT solver_pubkey FROM solver_links WHERE chat_id = ?";
        sqlx::query(&format!(
            "DELETE FROM solver_conversations WHERE solver_pubkey IN ({of_chat})"
        ))
        .bind(chat_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(&format!(
            "UPDATE solver_watches SET active = 0 WHERE solver_pubkey IN ({of_chat})"
        ))
        .bind(chat_id)
        .execute(&mut *tx)
        .await?;
        let unlinked = sqlx::query("DELETE FROM solver_links WHERE chat_id = ?")
            .bind(chat_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok(unlinked)
    }

    /// Follows `conversations` of the dispute for `solver`, replacing the
    /// ones it followed before. A conversation it already followed keeps
    /// the time it was first watched from, so a repeated `watch` drops no
    /// message.
    pub async fn apply_watch(
        &self,
        solver: &PublicKey,
        dispute_id: &str,
        conversations: &[Conversation],
        created_at: i64,
    ) -> Result<Watched, sqlx::Error> {
        let solver = solver.to_hex();
        let mut tx = self.pool.begin().await?;
        if chat_of(&mut *tx, &solver).await?.is_none() {
            return Ok(Watched::NotLinked);
        }
        if !is_newest(&mut tx, &solver, dispute_id, created_at).await?
            || ended_after(&mut tx, dispute_id, created_at).await?
        {
            return Ok(Watched::Stale);
        }
        if watched_elsewhere(&mut tx, &solver, dispute_id).await? >= MAX_WATCHED_DISPUTES {
            return Ok(Watched::TooMany);
        }
        let mut since = Vec::with_capacity(conversations.len());
        for conversation in conversations {
            let kept: Option<(i64,)> = sqlx::query_as(
                r#"
                SELECT watched_since FROM solver_conversations
                WHERE sign_pubkey = ? AND solver_pubkey = ? AND dispute_id = ?
                "#,
            )
            .bind(conversation.sign_pubkey.to_hex())
            .bind(&solver)
            .bind(dispute_id)
            .fetch_optional(&mut *tx)
            .await?;
            since.push(kept.map_or(created_at, |(kept,)| kept));
        }
        save_watch(&mut tx, &solver, dispute_id, true, created_at).await?;
        for (conversation, watched_since) in conversations.iter().zip(since) {
            sqlx::query(
                r#"
                INSERT OR REPLACE INTO solver_conversations
                    (sign_pubkey, solver_pubkey, dispute_id, party, watched_since)
                VALUES (?, ?, ?, ?, ?)
                "#,
            )
            .bind(conversation.sign_pubkey.to_hex())
            .bind(&solver)
            .bind(dispute_id)
            .bind(conversation.party.as_str())
            .bind(watched_since)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(Watched::Applied)
    }

    /// Stops following the dispute for `solver`, unless a later `watch` or
    /// `unwatch` was applied. Returns whether it applied.
    pub async fn apply_unwatch(
        &self,
        solver: &PublicKey,
        dispute_id: &str,
        created_at: i64,
    ) -> Result<bool, sqlx::Error> {
        let solver = solver.to_hex();
        let mut tx = self.pool.begin().await?;
        if !is_newest(&mut tx, &solver, dispute_id, created_at).await? {
            return Ok(false);
        }
        save_watch(&mut tx, &solver, dispute_id, false, created_at).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Stops following a resolved dispute for every solver; a `watch`
    /// written before `ended_at` no longer applies. Returns how many
    /// conversations were dropped.
    pub async fn end_dispute(&self, dispute_id: &str, ended_at: i64) -> Result<u64, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            r#"
            INSERT INTO solver_ended_disputes (dispute_id, ended_at) VALUES (?, ?)
            ON CONFLICT(dispute_id) DO UPDATE SET ended_at = MAX(ended_at, excluded.ended_at)
            "#,
        )
        .bind(dispute_id)
        .bind(ended_at)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE solver_watches SET active = 0 WHERE dispute_id = ?")
            .bind(dispute_id)
            .execute(&mut *tx)
            .await?;
        let dropped = sqlx::query("DELETE FROM solver_conversations WHERE dispute_id = ?")
            .bind(dispute_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok(dropped)
    }

    /// Every linked solver's watch of the conversation signed by
    /// `sign_pubkey`.
    pub async fn conversations(
        &self,
        sign_pubkey: &PublicKey,
    ) -> Result<Vec<WatchedConversation>, sqlx::Error> {
        let rows: Vec<ConversationRow> = sqlx::query_as(&format!(
            "{CONVERSATION_SELECT} WHERE c.sign_pubkey = ? ORDER BY c.solver_pubkey"
        ))
        .bind(sign_pubkey.to_hex())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().filter_map(watched).collect())
    }

    /// `solver`'s watch of the conversation signed by `sign_pubkey`.
    pub async fn conversation(
        &self,
        sign_pubkey: &PublicKey,
        solver: &str,
    ) -> Result<Option<WatchedConversation>, sqlx::Error> {
        let row: Option<ConversationRow> = sqlx::query_as(&format!(
            "{CONVERSATION_SELECT} WHERE c.sign_pubkey = ? AND c.solver_pubkey = ?"
        ))
        .bind(sign_pubkey.to_hex())
        .bind(solver)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.and_then(watched))
    }

    /// The signing keys of every watched conversation of a linked solver.
    pub async fn watched_sign_pubkeys(&self) -> Result<Vec<PublicKey>, sqlx::Error> {
        let rows: Vec<(String,)> = sqlx::query_as(
            r#"
            SELECT DISTINCT c.sign_pubkey FROM solver_conversations c
            JOIN solver_links l ON l.solver_pubkey = c.solver_pubkey
            ORDER BY c.sign_pubkey
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(hex,)| PublicKey::from_hex(&hex).ok())
            .collect())
    }

    /// The disputes a linked solver watches, each once.
    pub async fn watched_dispute_ids(&self) -> Result<Vec<String>, sqlx::Error> {
        let rows: Vec<(String,)> = sqlx::query_as(
            r#"
            SELECT DISTINCT c.dispute_id FROM solver_conversations c
            JOIN solver_links l ON l.solver_pubkey = c.solver_pubkey
            ORDER BY c.dispute_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    }

    /// Records that `solver` wrote chat event `event_id`. Returns `false`
    /// when the solver already holds [`MAX_RECEIPTS_PER_SOLVER`] receipts.
    pub async fn record_receipt(
        &self,
        event_id: &EventId,
        solver: &PublicKey,
        now: i64,
    ) -> Result<bool, sqlx::Error> {
        let solver = solver.to_hex();
        let (held,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM solver_receipts WHERE solver_pubkey = ?")
                .bind(&solver)
                .fetch_one(&self.pool)
                .await?;
        if held >= MAX_RECEIPTS_PER_SOLVER {
            return Ok(false);
        }
        sqlx::query(
            r#"
            INSERT OR IGNORE INTO solver_receipts (event_id, solver_pubkey, received_at)
            VALUES (?, ?, ?)
            "#,
        )
        .bind(event_id.to_hex())
        .bind(solver)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(true)
    }

    /// Whether `solver` said it wrote chat event `event_id`.
    pub async fn has_receipt(&self, event_id: &EventId, solver: &str) -> Result<bool, sqlx::Error> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM solver_receipts WHERE event_id = ? AND solver_pubkey = ?",
        )
        .bind(event_id.to_hex())
        .bind(solver)
        .fetch_one(&self.pool)
        .await?;
        Ok(count > 0)
    }

    /// Records that event `event_id` was handled in `scope`: a chat event
    /// notified or dropped for a solver (its key), or a Mostrix message
    /// applied ([`MOSTRIX_SCOPE`]).
    pub async fn mark_handled(
        &self,
        event_id: &EventId,
        scope: &str,
        now: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT OR IGNORE INTO solver_handled_events (event_id, scope, handled_at)
            VALUES (?, ?, ?)
            "#,
        )
        .bind(event_id.to_hex())
        .bind(scope)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Whether event `event_id` was already handled in `scope`.
    pub async fn is_handled(&self, event_id: &EventId, scope: &str) -> Result<bool, sqlx::Error> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM solver_handled_events WHERE event_id = ? AND scope = ?",
        )
        .bind(event_id.to_hex())
        .bind(scope)
        .fetch_one(&self.pool)
        .await?;
        Ok(count > 0)
    }

    /// Forgets receipts, handled events and ended disputes recorded before
    /// `before`, and expired link codes. `before` must lie past the catch-up
    /// window, so nothing forgotten can be delivered again.
    pub async fn prune(&self, before: i64, now: i64) -> Result<(), sqlx::Error> {
        let statements = [
            ("DELETE FROM solver_receipts WHERE received_at < ?", before),
            (
                "DELETE FROM solver_handled_events WHERE handled_at < ?",
                before,
            ),
            (
                "DELETE FROM solver_ended_disputes WHERE ended_at < ?",
                before,
            ),
            ("DELETE FROM solver_link_codes WHERE expires_at <= ?", now),
        ];
        for (statement, bound) in statements {
            sqlx::query(statement)
                .bind(bound)
                .execute(&self.pool)
                .await?;
        }
        Ok(())
    }
}

const CONVERSATION_SELECT: &str = r#"
    SELECT c.dispute_id, c.solver_pubkey, c.party, c.watched_since, l.chat_id
    FROM solver_conversations c
    JOIN solver_links l ON l.solver_pubkey = c.solver_pubkey
"#;

type ConversationRow = (String, String, String, i64, i64);

fn watched(
    (dispute_id, solver_pubkey, party, watched_since, chat_id): ConversationRow,
) -> Option<WatchedConversation> {
    Some(WatchedConversation {
        dispute_id,
        solver_pubkey,
        party: Party::parse(&party)?,
        watched_since,
        chat_id,
    })
}

async fn chat_of<'c, E>(executor: E, solver: &str) -> Result<Option<i64>, sqlx::Error>
where
    E: sqlx::Executor<'c, Database = sqlx::Sqlite>,
{
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT chat_id FROM solver_links WHERE solver_pubkey = ?")
            .bind(solver)
            .fetch_optional(executor)
            .await?;
    Ok(row.map(|(chat_id,)| chat_id))
}

/// Whether a `watch` or `unwatch` written at `created_at` is not older than
/// the last one applied for this dispute and solver.
async fn is_newest(
    tx: &mut SqliteConnection,
    solver: &str,
    dispute_id: &str,
    created_at: i64,
) -> Result<bool, sqlx::Error> {
    let stored: Option<(i64,)> = sqlx::query_as(
        "SELECT message_created_at FROM solver_watches WHERE dispute_id = ? AND solver_pubkey = ?",
    )
    .bind(dispute_id)
    .bind(solver)
    .fetch_optional(tx)
    .await?;
    Ok(stored.is_none_or(|(applied,)| created_at >= applied))
}

/// Whether the dispute was resolved at or after `created_at`.
async fn ended_after(
    tx: &mut SqliteConnection,
    dispute_id: &str,
    created_at: i64,
) -> Result<bool, sqlx::Error> {
    let ended: Option<(i64,)> =
        sqlx::query_as("SELECT ended_at FROM solver_ended_disputes WHERE dispute_id = ?")
            .bind(dispute_id)
            .fetch_optional(tx)
            .await?;
    Ok(ended.is_some_and(|(ended_at,)| ended_at >= created_at))
}

/// Disputes other than `dispute_id` that `solver` watches.
async fn watched_elsewhere(
    tx: &mut SqliteConnection,
    solver: &str,
    dispute_id: &str,
) -> Result<i64, sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*) FROM solver_watches
        WHERE solver_pubkey = ? AND active = 1 AND dispute_id != ?
        "#,
    )
    .bind(solver)
    .bind(dispute_id)
    .fetch_one(tx)
    .await?;
    Ok(count)
}

/// Saves the watch state and drops its conversations; a `watch` adds its own
/// right after.
async fn save_watch(
    tx: &mut SqliteConnection,
    solver: &str,
    dispute_id: &str,
    active: bool,
    created_at: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO solver_watches (dispute_id, solver_pubkey, active, message_created_at)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(dispute_id, solver_pubkey) DO UPDATE SET
            active = excluded.active,
            message_created_at = excluded.message_created_at
        "#,
    )
    .bind(dispute_id)
    .bind(solver)
    .bind(active)
    .bind(created_at)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM solver_conversations WHERE dispute_id = ? AND solver_pubkey = ?")
        .bind(dispute_id)
        .bind(solver)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// The tables, created when missing. Safe on every database.
const SCHEMA: &[&str] = &[
    // A solver key and the Telegram chat that linked it.
    r#"CREATE TABLE IF NOT EXISTS solver_links (
        solver_pubkey TEXT PRIMARY KEY NOT NULL,
        chat_id INTEGER NOT NULL,
        linked_at INTEGER NOT NULL
    )"#,
    // One pending `/link` code per chat.
    r#"CREATE TABLE IF NOT EXISTS solver_link_codes (
        code TEXT PRIMARY KEY NOT NULL,
        chat_id INTEGER NOT NULL,
        expires_at INTEGER NOT NULL
    )"#,
    // The last `watch` or `unwatch` applied per dispute and solver, to apply
    // redelivered or reordered messages in the right order.
    r#"CREATE TABLE IF NOT EXISTS solver_watches (
        dispute_id TEXT NOT NULL,
        solver_pubkey TEXT NOT NULL,
        active INTEGER NOT NULL,
        message_created_at INTEGER NOT NULL,
        PRIMARY KEY (dispute_id, solver_pubkey)
    )"#,
    // Per solver: two keys may watch the same conversation, and neither
    // affects the other.
    r#"CREATE TABLE IF NOT EXISTS solver_conversations (
        sign_pubkey TEXT NOT NULL,
        solver_pubkey TEXT NOT NULL,
        dispute_id TEXT NOT NULL,
        party TEXT NOT NULL,
        watched_since INTEGER NOT NULL,
        PRIMARY KEY (sign_pubkey, solver_pubkey)
    )"#,
    // Resolved disputes, so a `watch` caught up later does not revive one.
    r#"CREATE TABLE IF NOT EXISTS solver_ended_disputes (
        dispute_id TEXT PRIMARY KEY NOT NULL,
        ended_at INTEGER NOT NULL
    )"#,
    // A receipt only speaks for the solver that sent it.
    r#"CREATE TABLE IF NOT EXISTS solver_receipts (
        event_id TEXT NOT NULL,
        solver_pubkey TEXT NOT NULL,
        received_at INTEGER NOT NULL,
        PRIMARY KEY (event_id, solver_pubkey)
    )"#,
    "CREATE INDEX IF NOT EXISTS solver_receipts_by_solver ON solver_receipts (solver_pubkey)",
    r#"CREATE TABLE IF NOT EXISTS solver_handled_events (
        event_id TEXT NOT NULL,
        scope TEXT NOT NULL,
        handled_at INTEGER NOT NULL,
        PRIMARY KEY (event_id, scope)
    )"#,
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DisputeMessageStore;
    use nostr_sdk::prelude::Keys;

    const DISPUTE: &str = "58511141-6e3f-4b87-9c4a-1f2e3d4c5b6a";
    const CHAT: i64 = 4242;

    struct Fixture {
        _dir: tempfile::TempDir,
        store: SolverStore,
    }

    async fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let disputes = DisputeMessageStore::new(&dir.path().join("disputes.db"))
            .await
            .unwrap();
        let store = SolverStore::new(disputes.pool()).await.unwrap();
        Fixture { _dir: dir, store }
    }

    impl Fixture {
        /// Links `solver` to `chat_id` through a fresh code.
        async fn link(&self, solver: &PublicKey, chat_id: i64) -> Redeemed {
            let code = crate::solver::code::generate();
            self.store
                .create_link_code(&code, chat_id, 10_000, 1)
                .await
                .unwrap();
            self.store.redeem_link_code(&code, solver, 2).await.unwrap()
        }
    }

    fn conversation(party: Party) -> Conversation {
        Conversation {
            party,
            sign_pubkey: Keys::generate().public_key(),
        }
    }

    fn dispute(n: u32) -> String {
        format!("00000000-0000-4000-8000-{n:012}")
    }

    #[tokio::test]
    async fn a_link_code_works_once_for_the_chat_that_asked() {
        let fx = fixture().await;
        let solver = Keys::generate().public_key();
        fx.store
            .create_link_code("K7QM-2XPA", CHAT, 1600, 1000)
            .await
            .unwrap();

        let first = fx.store.redeem_link_code("K7QM-2XPA", &solver, 1001).await;
        let again = fx.store.redeem_link_code("K7QM-2XPA", &solver, 1002).await;

        assert_eq!(first.unwrap(), Redeemed::Linked(CHAT));
        assert_eq!(again.unwrap(), Redeemed::UnknownCode);
        assert_eq!(fx.store.chat_of(&solver).await.unwrap(), Some(CHAT));
    }

    #[tokio::test]
    async fn an_expired_or_replaced_code_does_not_work() {
        let fx = fixture().await;
        let solver = Keys::generate().public_key();
        fx.store
            .create_link_code("AAAA-AAAA", CHAT, 1600, 1000)
            .await
            .unwrap();
        fx.store
            .create_link_code("BBBB-BBBB", CHAT, 1700, 1100)
            .await
            .unwrap();

        let replaced = fx.store.redeem_link_code("AAAA-AAAA", &solver, 1101).await;
        let expired = fx.store.redeem_link_code("BBBB-BBBB", &solver, 1700).await;

        assert_eq!(replaced.unwrap(), Redeemed::UnknownCode);
        assert_eq!(expired.unwrap(), Redeemed::UnknownCode);
        assert_eq!(fx.store.chat_of(&solver).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_chat_links_at_most_five_keys() {
        let fx = fixture().await;
        for _ in 0..MAX_KEYS_PER_CHAT {
            let key = Keys::generate().public_key();
            assert_eq!(fx.link(&key, CHAT).await, Redeemed::Linked(CHAT));
        }

        let sixth = Keys::generate().public_key();

        assert_eq!(fx.link(&sixth, CHAT).await, Redeemed::TooManyKeys(CHAT));
        assert_eq!(fx.store.chat_of(&sixth).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_watch_follows_conversations_of_a_linked_solver() {
        let fx = fixture().await;
        let solver = Keys::generate().public_key();
        let buyer = conversation(Party::Buyer);
        fx.link(&solver, CHAT).await;

        let outcome = fx
            .store
            .apply_watch(&solver, DISPUTE, std::slice::from_ref(&buyer), 100)
            .await
            .unwrap();

        assert_eq!(outcome, Watched::Applied);
        assert_eq!(
            fx.store.conversations(&buyer.sign_pubkey).await.unwrap(),
            vec![WatchedConversation {
                dispute_id: DISPUTE.into(),
                solver_pubkey: solver.to_hex(),
                party: Party::Buyer,
                watched_since: 100,
                chat_id: CHAT,
            }]
        );
        assert_eq!(
            fx.store.watched_sign_pubkeys().await.unwrap(),
            vec![buyer.sign_pubkey]
        );
        assert_eq!(
            fx.store.links_of_chat(CHAT).await.unwrap(),
            vec![LinkedSolver {
                solver_pubkey: solver.to_hex(),
                watched_disputes: 1
            }]
        );
    }

    #[tokio::test]
    async fn watched_disputes_are_listed_once_each() {
        let fx = fixture().await;
        let (solver, other) = (Keys::generate().public_key(), Keys::generate().public_key());
        fx.link(&solver, CHAT).await;
        fx.link(&other, CHAT + 1).await;
        let both = [conversation(Party::Buyer), conversation(Party::Seller)];
        for key in [&solver, &other] {
            fx.store
                .apply_watch(key, DISPUTE, &both, 100)
                .await
                .unwrap();
        }
        let second = dispute(2);
        fx.store
            .apply_watch(&solver, &second, &[conversation(Party::Buyer)], 100)
            .await
            .unwrap();

        let ids = fx.store.watched_dispute_ids().await.unwrap();

        let mut expected = vec![second, DISPUTE.to_owned()];
        expected.sort();
        assert_eq!(ids, expected);
    }

    #[tokio::test]
    async fn an_unlinked_key_cannot_watch() {
        let fx = fixture().await;
        let solver = Keys::generate().public_key();

        let outcome = fx
            .store
            .apply_watch(&solver, DISPUTE, &[conversation(Party::Buyer)], 100)
            .await
            .unwrap();

        assert_eq!(outcome, Watched::NotLinked);
        assert!(fx.store.watched_sign_pubkeys().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn another_key_watching_the_same_conversation_takes_nothing() {
        let fx = fixture().await;
        let (solver, other) = (Keys::generate().public_key(), Keys::generate().public_key());
        let buyer = conversation(Party::Buyer);
        fx.link(&solver, CHAT).await;
        fx.link(&other, CHAT + 1).await;
        fx.store
            .apply_watch(&solver, DISPUTE, std::slice::from_ref(&buyer), 100)
            .await
            .unwrap();

        fx.store
            .apply_watch(&other, &dispute(9), std::slice::from_ref(&buyer), 200)
            .await
            .unwrap();

        let mine = fx
            .store
            .conversation(&buyer.sign_pubkey, &solver.to_hex())
            .await
            .unwrap()
            .expect("still watched");
        assert_eq!((mine.chat_id, mine.dispute_id.as_str()), (CHAT, DISPUTE));
        assert_eq!(
            fx.store
                .conversations(&buyer.sign_pubkey)
                .await
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn a_repeated_watch_keeps_when_the_conversation_was_first_watched() {
        let fx = fixture().await;
        let solver = Keys::generate().public_key();
        let buyer = conversation(Party::Buyer);
        fx.link(&solver, CHAT).await;
        fx.store
            .apply_watch(&solver, DISPUTE, std::slice::from_ref(&buyer), 100)
            .await
            .unwrap();

        fx.store
            .apply_watch(&solver, DISPUTE, std::slice::from_ref(&buyer), 500)
            .await
            .unwrap();

        let watched = fx
            .store
            .conversation(&buyer.sign_pubkey, &solver.to_hex())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(watched.watched_since, 100);
    }

    #[tokio::test]
    async fn a_new_watch_replaces_the_conversations() {
        let fx = fixture().await;
        let solver = Keys::generate().public_key();
        let (old, new) = (conversation(Party::Buyer), conversation(Party::Seller));
        fx.link(&solver, CHAT).await;
        fx.store
            .apply_watch(&solver, DISPUTE, std::slice::from_ref(&old), 100)
            .await
            .unwrap();

        fx.store
            .apply_watch(&solver, DISPUTE, std::slice::from_ref(&new), 200)
            .await
            .unwrap();

        assert!(fx
            .store
            .conversations(&old.sign_pubkey)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            fx.store
                .conversations(&new.sign_pubkey)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn a_key_watches_at_most_fifty_disputes() {
        let fx = fixture().await;
        let solver = Keys::generate().public_key();
        fx.link(&solver, CHAT).await;
        for n in 0..MAX_WATCHED_DISPUTES {
            let outcome = fx
                .store
                .apply_watch(
                    &solver,
                    &dispute(n as u32),
                    &[conversation(Party::Buyer)],
                    100,
                )
                .await
                .unwrap();
            assert_eq!(outcome, Watched::Applied);
        }

        let over = fx
            .store
            .apply_watch(&solver, &dispute(999), &[conversation(Party::Buyer)], 100)
            .await
            .unwrap();
        // A watched dispute can still be watched again.
        let again = fx
            .store
            .apply_watch(&solver, &dispute(0), &[conversation(Party::Buyer)], 200)
            .await
            .unwrap();

        assert_eq!(over, Watched::TooMany);
        assert_eq!(again, Watched::Applied);
    }

    #[tokio::test]
    async fn an_older_watch_never_undoes_a_later_unwatch() {
        // A catch-up redelivers the watch after the unwatch.
        let fx = fixture().await;
        let solver = Keys::generate().public_key();
        let buyer = conversation(Party::Buyer);
        fx.link(&solver, CHAT).await;
        fx.store
            .apply_watch(&solver, DISPUTE, std::slice::from_ref(&buyer), 100)
            .await
            .unwrap();
        assert!(fx.store.apply_unwatch(&solver, DISPUTE, 200).await.unwrap());

        let replayed = fx
            .store
            .apply_watch(&solver, DISPUTE, std::slice::from_ref(&buyer), 100)
            .await
            .unwrap();

        assert_eq!(replayed, Watched::Stale);
        assert!(fx
            .store
            .conversations(&buyer.sign_pubkey)
            .await
            .unwrap()
            .is_empty());
        assert!(!fx.store.apply_unwatch(&solver, DISPUTE, 150).await.unwrap());
    }

    #[tokio::test]
    async fn an_ended_dispute_is_not_revived_by_an_earlier_watch() {
        let fx = fixture().await;
        let solver = Keys::generate().public_key();
        let buyer = conversation(Party::Buyer);
        fx.link(&solver, CHAT).await;
        fx.store
            .apply_watch(&solver, DISPUTE, std::slice::from_ref(&buyer), 100)
            .await
            .unwrap();

        assert_eq!(fx.store.end_dispute(DISPUTE, 300).await.unwrap(), 1);
        let replayed = fx
            .store
            .apply_watch(&solver, DISPUTE, std::slice::from_ref(&buyer), 100)
            .await
            .unwrap();

        assert_eq!(replayed, Watched::Stale);
        assert!(fx.store.watched_sign_pubkeys().await.unwrap().is_empty());
        // A watch written later (a reopened dispute) applies.
        let later = fx
            .store
            .apply_watch(&solver, DISPUTE, std::slice::from_ref(&buyer), 400)
            .await
            .unwrap();
        assert_eq!(later, Watched::Applied);
    }

    #[tokio::test]
    async fn unlinking_a_chat_stops_watching_its_disputes() {
        let fx = fixture().await;
        let (solver, other) = (Keys::generate().public_key(), Keys::generate().public_key());
        let (mine, theirs) = (conversation(Party::Buyer), conversation(Party::Buyer));
        fx.link(&solver, CHAT).await;
        fx.link(&other, CHAT + 1).await;
        fx.store
            .apply_watch(&solver, DISPUTE, std::slice::from_ref(&mine), 100)
            .await
            .unwrap();
        fx.store
            .apply_watch(&other, DISPUTE, std::slice::from_ref(&theirs), 100)
            .await
            .unwrap();

        assert_eq!(fx.store.unlink_chat(CHAT).await.unwrap(), 1);

        assert_eq!(fx.store.chat_of(&solver).await.unwrap(), None);
        assert!(fx
            .store
            .conversations(&mine.sign_pubkey)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            fx.store.watched_sign_pubkeys().await.unwrap(),
            vec![theirs.sign_pubkey]
        );
    }

    #[tokio::test]
    async fn linking_again_moves_the_key_to_the_new_chat() {
        let fx = fixture().await;
        let solver = Keys::generate().public_key();

        fx.link(&solver, CHAT).await;
        fx.link(&solver, CHAT + 1).await;

        assert_eq!(fx.store.chat_of(&solver).await.unwrap(), Some(CHAT + 1));
        assert!(fx.store.links_of_chat(CHAT).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_receipt_speaks_only_for_its_solver() {
        let fx = fixture().await;
        let (solver, other) = (Keys::generate().public_key(), Keys::generate().public_key());
        let id = EventId::from_byte_array([1; 32]);

        assert!(fx.store.record_receipt(&id, &other, 100).await.unwrap());

        assert!(fx.store.has_receipt(&id, &other.to_hex()).await.unwrap());
        assert!(!fx.store.has_receipt(&id, &solver.to_hex()).await.unwrap());
    }

    #[tokio::test]
    async fn handled_events_are_kept_per_scope() {
        let fx = fixture().await;
        let id = EventId::from_byte_array([1; 32]);

        fx.store.mark_handled(&id, "solver-a", 100).await.unwrap();

        assert!(fx.store.is_handled(&id, "solver-a").await.unwrap());
        assert!(!fx.store.is_handled(&id, "solver-b").await.unwrap());
    }

    #[tokio::test]
    async fn old_records_are_pruned() {
        let fx = fixture().await;
        let solver = Keys::generate().public_key();
        let id = EventId::from_byte_array([1; 32]);
        let other = EventId::from_byte_array([2; 32]);
        fx.store.record_receipt(&id, &solver, 100).await.unwrap();
        fx.store
            .mark_handled(&id, MOSTRIX_SCOPE, 100)
            .await
            .unwrap();
        fx.store.record_receipt(&other, &solver, 300).await.unwrap();
        fx.store.end_dispute(DISPUTE, 100).await.unwrap();

        fx.store.prune(200, 200).await.unwrap();

        let key = solver.to_hex();
        assert!(!fx.store.has_receipt(&id, &key).await.unwrap());
        assert!(!fx.store.is_handled(&id, MOSTRIX_SCOPE).await.unwrap());
        assert!(fx.store.has_receipt(&other, &key).await.unwrap());
        // The tombstone is gone: an old watch would apply again.
        fx.link(&solver, CHAT).await;
        let outcome = fx
            .store
            .apply_watch(&solver, DISPUTE, &[conversation(Party::Buyer)], 150)
            .await
            .unwrap();
        assert_eq!(outcome, Watched::Applied);
    }
}
