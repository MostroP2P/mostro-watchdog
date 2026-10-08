//! SQLite storage for tracking Telegram message IDs per dispute.
//!
//! This allows updating or deleting messages when dispute status changes.
//! It also keeps what Serbero last reported about each dispute and which of
//! its updates were already relayed, so restarts and relay redeliveries
//! never repeat an alert, and which disputes a solver took over from it.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;
use tracing::info;

/// Stores the mapping between dispute IDs and Telegram message IDs.
#[derive(Clone)]
pub struct DisputeMessageStore {
    pool: SqlitePool,
}

/// A dispute's Telegram message, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    pub message_id: i32,
    pub chat_id: i64,
    /// The kind-38386 status the message shows.
    pub status: String,
    /// The alert text (MarkdownV2) without Serbero's line, to redraw the
    /// message when Serbero's state changes. `None` for messages stored
    /// before this column existed.
    pub text: Option<String>,
}

/// What Serbero last reported about a dispute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerberoState {
    /// The canonical header subject, e.g. `handed off: conflicting_claims`.
    pub subject: String,
    /// When Serbero wrote it (event `created_at`, Unix seconds).
    pub created_at: i64,
}

/// A dispute's newest kind-38386 status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedStatus {
    pub status: String,
    /// The event's `created_at`, Unix seconds.
    pub created_at: i64,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

impl DisputeMessageStore {
    /// Initialize the database, creating or migrating its tables.
    pub async fn new(db_path: &Path) -> Result<Self, sqlx::Error> {
        let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", db_path.display()))?
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        migrate(&pool).await?;

        info!("Dispute message store initialized at {}", db_path.display());
        Ok(Self { pool })
    }

    /// Store a new dispute → message mapping, with the alert `text` sent
    /// (without Serbero's line).
    pub async fn insert(
        &self,
        dispute_id: &str,
        message_id: i32,
        chat_id: i64,
        status: &str,
        text: &str,
    ) -> Result<(), sqlx::Error> {
        let now = now_secs();

        sqlx::query(
            r#"
            INSERT INTO dispute_messages
                (dispute_id, message_id, chat_id, status, message_text, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(dispute_id) DO UPDATE SET
                message_id = excluded.message_id,
                status = excluded.status,
                message_text = excluded.message_text,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(dispute_id)
        .bind(message_id)
        .bind(chat_id)
        .bind(status)
        .bind(text)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// The connection pool, for stores that keep their own tables in the
    /// same database.
    pub fn pool(&self) -> SqlitePool {
        self.pool.clone()
    }

    /// Get the message ID for a dispute.
    pub async fn get_message_id(
        &self,
        dispute_id: &str,
    ) -> Result<Option<(i32, i64)>, sqlx::Error> {
        let result: Option<(i32, i64)> = sqlx::query_as(
            r#"
            SELECT message_id, chat_id FROM dispute_messages WHERE dispute_id = ?
            "#,
        )
        .bind(dispute_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(result)
    }

    /// The stored message for a dispute.
    pub async fn get_message(
        &self,
        dispute_id: &str,
    ) -> Result<Option<StoredMessage>, sqlx::Error> {
        let row: Option<(i32, i64, String, Option<String>)> = sqlx::query_as(
            r#"
            SELECT message_id, chat_id, status, message_text
            FROM dispute_messages WHERE dispute_id = ?
            "#,
        )
        .bind(dispute_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(
            row.map(|(message_id, chat_id, status, text)| StoredMessage {
                message_id,
                chat_id,
                status,
                text,
            }),
        )
    }

    /// Update the status for a dispute, with the alert `text` now shown
    /// (without Serbero's line).
    pub async fn update_status(
        &self,
        dispute_id: &str,
        status: &str,
        text: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            UPDATE dispute_messages SET status = ?, message_text = ?, updated_at = ?
            WHERE dispute_id = ?
            "#,
        )
        .bind(status)
        .bind(text)
        .bind(now_secs())
        .bind(dispute_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Delete a dispute record (after cooperative cancellation).
    pub async fn delete(&self, dispute_id: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            DELETE FROM dispute_messages WHERE dispute_id = ?
            "#,
        )
        .bind(dispute_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// What Serbero last reported about a dispute.
    pub async fn serbero_state(
        &self,
        dispute_id: &str,
    ) -> Result<Option<SerberoState>, sqlx::Error> {
        let row: Option<(String, i64)> = sqlx::query_as(
            r#"
            SELECT subject, event_created_at FROM serbero_states WHERE dispute_id = ?
            "#,
        )
        .bind(dispute_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(subject, created_at)| SerberoState {
            subject,
            created_at,
        }))
    }

    /// Records what Serbero last reported about a dispute.
    pub async fn save_serbero_state(
        &self,
        dispute_id: &str,
        state: &SerberoState,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO serbero_states (dispute_id, subject, event_created_at, updated_at)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(dispute_id) DO UPDATE SET
                subject = excluded.subject,
                event_created_at = excluded.event_created_at,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(dispute_id)
        .bind(&state.subject)
        .bind(state.created_at)
        .bind(now_secs())
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Records a dispute's kind-38386 status unless a newer one is known.
    /// Kept for every event, whatever the alert toggles: a late Serbero
    /// handoff must see that the dispute already ended.
    pub async fn record_dispute_status(
        &self,
        dispute_id: &str,
        status: &str,
        event_created_at: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO dispute_statuses (dispute_id, status, event_created_at)
            VALUES (?, ?, ?)
            ON CONFLICT(dispute_id) DO UPDATE SET
                status = excluded.status,
                event_created_at = excluded.event_created_at
            WHERE excluded.event_created_at >= dispute_statuses.event_created_at
            "#,
        )
        .bind(dispute_id)
        .bind(status)
        .bind(event_created_at)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// The newest kind-38386 status seen for a dispute.
    pub async fn dispute_status(&self, dispute_id: &str) -> Result<Option<String>, sqlx::Error> {
        Ok(self.recorded_status(dispute_id).await?.map(|r| r.status))
    }

    /// The newest kind-38386 status seen for a dispute, with its event time.
    pub async fn recorded_status(
        &self,
        dispute_id: &str,
    ) -> Result<Option<RecordedStatus>, sqlx::Error> {
        let row: Option<(String, i64)> = sqlx::query_as(
            "SELECT status, event_created_at FROM dispute_statuses WHERE dispute_id = ?",
        )
        .bind(dispute_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|(status, created_at)| RecordedStatus { status, created_at }))
    }

    /// Records that a solver took the dispute over from Serbero. Returns
    /// whether this is news: `false` when a takeover was already recorded.
    pub async fn record_takeover(
        &self,
        dispute_id: &str,
        event_created_at: i64,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"
            INSERT OR IGNORE INTO serbero_takeovers
                (dispute_id, event_created_at, recorded_at)
            VALUES (?, ?, ?)
            "#,
        )
        .bind(dispute_id)
        .bind(event_created_at)
        .bind(now_secs())
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    /// Whether a solver took the dispute over from Serbero.
    pub async fn taken_over(&self, dispute_id: &str) -> Result<bool, sqlx::Error> {
        let (count,): (i64,) =
            sqlx::query_as("SELECT COUNT(*) FROM serbero_takeovers WHERE dispute_id = ?")
                .bind(dispute_id)
                .fetch_one(&self.pool)
                .await?;

        Ok(count > 0)
    }

    /// Whether this Serbero header was already relayed for the dispute.
    pub async fn serbero_header_handled(
        &self,
        dispute_id: &str,
        subject: &str,
    ) -> Result<bool, sqlx::Error> {
        let (count,): (i64,) = sqlx::query_as(
            r#"
            SELECT COUNT(*) FROM serbero_headers WHERE dispute_id = ? AND subject = ?
            "#,
        )
        .bind(dispute_id)
        .bind(subject)
        .fetch_one(&self.pool)
        .await?;

        Ok(count > 0)
    }

    /// Records that this Serbero header was relayed for the dispute.
    pub async fn mark_serbero_header_handled(
        &self,
        dispute_id: &str,
        subject: &str,
        created_at: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT OR IGNORE INTO serbero_headers
                (dispute_id, subject, event_created_at, handled_at)
            VALUES (?, ?, ?, ?)
            "#,
        )
        .bind(dispute_id)
        .bind(subject)
        .bind(created_at)
        .bind(now_secs())
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}

#[cfg(test)]
impl DisputeMessageStore {
    /// Closes the pool, as a stopped process would.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

/// Creates missing tables and columns. Safe on new databases and on
/// databases written by any earlier version.
async fn migrate(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    // The table as released before Serbero alerts.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS dispute_messages (
            dispute_id TEXT PRIMARY KEY NOT NULL,
            message_id INTEGER NOT NULL,
            chat_id INTEGER NOT NULL,
            status TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Added for Serbero alerts: the alert text without Serbero's line, so the
    // message can be redrawn when Serbero's state changes. Older rows keep
    // NULL.
    let (has_text,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM pragma_table_info('dispute_messages') WHERE name = 'message_text'",
    )
    .fetch_one(pool)
    .await?;
    if has_text == 0 {
        sqlx::query("ALTER TABLE dispute_messages ADD COLUMN message_text TEXT")
            .execute(pool)
            .await?;
    }

    // Serbero's latest state per dispute, kept apart from the messages: a
    // dispute may have a state before the watchdog sent it any alert.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS serbero_states (
            dispute_id TEXT PRIMARY KEY NOT NULL,
            subject TEXT NOT NULL,
            event_created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await?;

    // The newest status of every dispute, whatever the alert toggles, and
    // kept when a cooperative cancel deletes the dispute's message.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS dispute_statuses (
            dispute_id TEXT PRIMARY KEY NOT NULL,
            status TEXT NOT NULL,
            event_created_at INTEGER NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Serbero headers already relayed, so a redelivered or re-fetched DM
    // never alerts twice. Not tied to the messages table: deleting a
    // dispute's message must not make its handoff alert send again.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS serbero_headers (
            dispute_id TEXT NOT NULL,
            subject TEXT NOT NULL,
            event_created_at INTEGER NOT NULL,
            handled_at INTEGER NOT NULL,
            PRIMARY KEY (dispute_id, subject)
        )
        "#,
    )
    .execute(pool)
    .await?;

    // Disputes a solver took over from Serbero, so the takeover message goes
    // out once and Serbero's line stops asking for a solver.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS serbero_takeovers (
            dispute_id TEXT PRIMARY KEY NOT NULL,
            event_created_at INTEGER NOT NULL,
            recorded_at INTEGER NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_dispute_message_store() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let store = DisputeMessageStore::new(&db_path).await.unwrap();

        // Insert a new dispute
        store
            .insert("dispute-123", 456, -100123, "initiated", "alert")
            .await
            .unwrap();

        // Get the message ID
        let result = store.get_message_id("dispute-123").await.unwrap();
        assert_eq!(result, Some((456, -100123)));

        // Update status
        store
            .update_status("dispute-123", "in-progress", "alert")
            .await
            .unwrap();

        // Delete
        store.delete("dispute-123").await.unwrap();
        let result = store.get_message_id("dispute-123").await.unwrap();
        assert_eq!(result, None);
    }

    /// The schema released before Serbero alerts (v0.3.0).
    const SCHEMA_BEFORE_SERBERO: &str = r#"
        CREATE TABLE dispute_messages (
            dispute_id TEXT PRIMARY KEY NOT NULL,
            message_id INTEGER NOT NULL,
            chat_id INTEGER NOT NULL,
            status TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )
    "#;

    async fn create_old_database(path: &Path) {
        let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", path.display()))
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(SCHEMA_BEFORE_SERBERO)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO dispute_messages VALUES ('old-dispute', 77, -100123, 'in-progress', 1, 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    }

    #[tokio::test]
    async fn a_dispute_keeps_its_newest_status() {
        let dir = tempfile::tempdir().unwrap();
        let store = DisputeMessageStore::new(&dir.path().join("d.db"))
            .await
            .unwrap();

        store
            .record_dispute_status("d1", "in-progress", 10)
            .await
            .unwrap();
        store
            .record_dispute_status("d1", "settled", 20)
            .await
            .unwrap();
        // A relay replays an older revision later.
        store
            .record_dispute_status("d1", "in-progress", 15)
            .await
            .unwrap();

        assert_eq!(
            store.dispute_status("d1").await.unwrap().as_deref(),
            Some("settled")
        );
        assert_eq!(store.dispute_status("d2").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_dispute_status_is_recorded_with_its_event_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = DisputeMessageStore::new(&dir.path().join("d.db"))
            .await
            .unwrap();

        store
            .record_dispute_status("d1", "in-progress", 10)
            .await
            .unwrap();

        assert_eq!(
            store.recorded_status("d1").await.unwrap(),
            Some(RecordedStatus {
                status: "in-progress".into(),
                created_at: 10,
            })
        );
        assert_eq!(store.recorded_status("d2").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_takeover_is_recorded_once_and_survives_restarts() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("t.db");
        let store = DisputeMessageStore::new(&db_path).await.unwrap();

        assert!(!store.taken_over("d1").await.unwrap());
        assert!(store.record_takeover("d1", 20).await.unwrap());
        // A second takeover of the same dispute is not new.
        assert!(!store.record_takeover("d1", 30).await.unwrap());
        store.pool.close().await;

        let store = DisputeMessageStore::new(&db_path).await.unwrap();
        assert!(store.taken_over("d1").await.unwrap());
        assert!(!store.taken_over("d2").await.unwrap());
    }

    #[tokio::test]
    async fn migrates_a_database_created_before_serbero_alerts() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("disputes.db");
        create_old_database(&db_path).await;

        let store = DisputeMessageStore::new(&db_path).await.unwrap();

        // Existing rows survive, without a stored text.
        assert_eq!(
            store.get_message("old-dispute").await.unwrap(),
            Some(StoredMessage {
                message_id: 77,
                chat_id: -100123,
                status: "in-progress".into(),
                text: None,
            })
        );
        // The new column and tables are usable.
        store
            .update_status("old-dispute", "settled", "settled alert")
            .await
            .unwrap();
        let state = SerberoState {
            subject: "mediating".into(),
            created_at: 5,
        };
        store
            .save_serbero_state("old-dispute", &state)
            .await
            .unwrap();
        store
            .mark_serbero_header_handled("old-dispute", "mediating", 5)
            .await
            .unwrap();
        store.pool.close().await;

        // Opening the migrated database again changes nothing.
        let store = DisputeMessageStore::new(&db_path).await.unwrap();
        let message = store.get_message("old-dispute").await.unwrap().unwrap();
        assert_eq!(message.status, "settled");
        assert_eq!(message.text.as_deref(), Some("settled alert"));
        assert_eq!(
            store.serbero_state("old-dispute").await.unwrap(),
            Some(state)
        );
    }

    #[tokio::test]
    async fn stores_the_alert_text_with_the_message() {
        let dir = tempdir().unwrap();
        let store = DisputeMessageStore::new(&dir.path().join("t.db"))
            .await
            .unwrap();

        store
            .insert("d1", 10, -1, "initiated", "new dispute")
            .await
            .unwrap();
        assert_eq!(
            store.get_message("d1").await.unwrap(),
            Some(StoredMessage {
                message_id: 10,
                chat_id: -1,
                status: "initiated".into(),
                text: Some("new dispute".into()),
            })
        );

        store
            .update_status("d1", "in-progress", "taken")
            .await
            .unwrap();
        let message = store.get_message("d1").await.unwrap().unwrap();
        assert_eq!(message.status, "in-progress");
        assert_eq!(message.text.as_deref(), Some("taken"));
        assert_eq!(store.get_message("unknown").await.unwrap(), None);
    }

    #[tokio::test]
    async fn keeps_the_latest_serbero_state_per_dispute() {
        let dir = tempdir().unwrap();
        let store = DisputeMessageStore::new(&dir.path().join("t.db"))
            .await
            .unwrap();
        let mediating = SerberoState {
            subject: "mediating".into(),
            created_at: 10,
        };
        let handed_off = SerberoState {
            subject: "handed off: flood".into(),
            created_at: 20,
        };

        assert_eq!(store.serbero_state("d1").await.unwrap(), None);
        store.save_serbero_state("d1", &mediating).await.unwrap();
        store.save_serbero_state("d1", &handed_off).await.unwrap();
        store.save_serbero_state("d2", &mediating).await.unwrap();

        assert_eq!(store.serbero_state("d1").await.unwrap(), Some(handed_off));
        assert_eq!(store.serbero_state("d2").await.unwrap(), Some(mediating));
    }

    #[tokio::test]
    async fn relayed_serbero_headers_are_remembered_across_restarts() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("t.db");
        let store = DisputeMessageStore::new(&db_path).await.unwrap();

        assert!(!store
            .serbero_header_handled("d1", "handed off: flood")
            .await
            .unwrap());
        store
            .mark_serbero_header_handled("d1", "handed off: flood", 20)
            .await
            .unwrap();
        // Marking twice is harmless.
        store
            .mark_serbero_header_handled("d1", "handed off: flood", 20)
            .await
            .unwrap();
        store.pool.close().await;

        let store = DisputeMessageStore::new(&db_path).await.unwrap();
        assert!(store
            .serbero_header_handled("d1", "handed off: flood")
            .await
            .unwrap());
        assert!(!store
            .serbero_header_handled("d1", "mediating")
            .await
            .unwrap());
        assert!(!store
            .serbero_header_handled("d2", "handed off: flood")
            .await
            .unwrap());
        store.pool.close().await;
    }

    #[tokio::test]
    async fn deleting_a_dispute_message_keeps_its_relayed_headers() {
        // A cooperative cancel deletes the dispute's message; a redelivered
        // handoff for it must still not be sent again.
        let dir = tempdir().unwrap();
        let store = DisputeMessageStore::new(&dir.path().join("t.db"))
            .await
            .unwrap();
        store.insert("d1", 10, -1, "initiated", "x").await.unwrap();
        store
            .mark_serbero_header_handled("d1", "mediation could not start", 3)
            .await
            .unwrap();

        store.delete("d1").await.unwrap();

        assert!(store
            .serbero_header_handled("d1", "mediation could not start")
            .await
            .unwrap());
    }
}
