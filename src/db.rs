//! SQLite storage for tracking Telegram message IDs per dispute.
//!
//! This allows updating or deleting messages when dispute status changes.

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;
use tracing::info;

/// Stores the mapping between dispute IDs and Telegram message IDs.
#[derive(Clone)]
pub struct DisputeMessageStore {
    pool: SqlitePool,
}

impl DisputeMessageStore {
    /// Initialize the database, creating the table if it doesn't exist.
    pub async fn new(db_path: &Path) -> Result<Self, sqlx::Error> {
        let options = SqliteConnectOptions::from_str(&format!("sqlite:{}", db_path.display()))?
            .create_if_missing(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;

        // Create table if not exists
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
        .execute(&pool)
        .await?;

        info!("Dispute message store initialized at {}", db_path.display());
        Ok(Self { pool })
    }

    /// Store a new dispute → message mapping.
    pub async fn insert(
        &self,
        dispute_id: &str,
        message_id: i32,
        chat_id: i64,
        status: &str,
    ) -> Result<(), sqlx::Error> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        sqlx::query(
            r#"
            INSERT INTO dispute_messages (dispute_id, message_id, chat_id, status, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(dispute_id) DO UPDATE SET
                message_id = excluded.message_id,
                status = excluded.status,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(dispute_id)
        .bind(message_id)
        .bind(chat_id)
        .bind(status)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(())
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

    /// Update the status for a dispute.
    pub async fn update_status(&self, dispute_id: &str, status: &str) -> Result<(), sqlx::Error> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        sqlx::query(
            r#"
            UPDATE dispute_messages SET status = ?, updated_at = ? WHERE dispute_id = ?
            "#,
        )
        .bind(status)
        .bind(now)
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
            .insert("dispute-123", 456, -100123, "initiated")
            .await
            .unwrap();

        // Get the message ID
        let result = store.get_message_id("dispute-123").await.unwrap();
        assert_eq!(result, Some((456, -100123)));

        // Update status
        store
            .update_status("dispute-123", "in-progress")
            .await
            .unwrap();

        // Delete
        store.delete("dispute-123").await.unwrap();
        let result = store.get_message_id("dispute-123").await.unwrap();
        assert_eq!(result, None);
    }
}
