//! What health monitoring keeps in `disputes.db`, so a restart neither
//! repeats nor loses anything:
//!
//! - open incidents, so an alert is not sent twice and its recovery is
//!   still reported;
//! - "resolved" messages not delivered yet, in a table of their own, so
//!   neither a restart nor the same rule opening again loses one;
//! - the highest `seq` accepted per pubkey, so a relay serving an older,
//!   unexpired revision after a restart is not believed (spec rule 2);
//! - the watchdog's own heartbeat `seq` and `created_at`, which must keep
//!   rising across restarts and clock steps.

use nostr_sdk::prelude::{EventId, PublicKey};
use sqlx::{Row, SqlitePool};

use super::event::Accepted;
use super::rules::{RuleId, Severity, Subject};
use super::tracker::{Change, Incident, Notice};

const SCHEMA: [&str; 4] = [
    r#"
    CREATE TABLE IF NOT EXISTS health_incidents (
        subject TEXT NOT NULL,
        rule TEXT NOT NULL,
        severity TEXT NOT NULL,
        started_at INTEGER NOT NULL,
        opened_at INTEGER NOT NULL,
        notified_at INTEGER,
        detail TEXT NOT NULL,
        PRIMARY KEY (subject, rule)
    )
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS health_pending_recoveries (
        subject TEXT NOT NULL,
        rule TEXT NOT NULL,
        severity TEXT NOT NULL,
        started_at INTEGER NOT NULL,
        opened_at INTEGER NOT NULL,
        notified_at INTEGER,
        detail TEXT NOT NULL,
        resolved_at INTEGER NOT NULL,
        resolution TEXT,
        PRIMARY KEY (subject, rule, resolved_at)
    )
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS health_accepted_seq (
        pubkey TEXT PRIMARY KEY,
        seq INTEGER NOT NULL,
        event_id TEXT NOT NULL
    )
    "#,
    r#"
    CREATE TABLE IF NOT EXISTS health_heartbeat (
        id INTEGER PRIMARY KEY CHECK (id = 1),
        seq INTEGER NOT NULL,
        created_at INTEGER NOT NULL
    )
    "#,
];

/// The last heartbeat the watchdog published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastHeartbeat {
    pub seq: u64,
    pub created_at: u64,
}

#[derive(Clone)]
pub struct HealthStore {
    pool: SqlitePool,
}

impl HealthStore {
    /// Opens the store on `pool`, creating its tables when missing.
    pub async fn new(pool: SqlitePool) -> Result<Self, sqlx::Error> {
        for statement in SCHEMA {
            sqlx::query(statement).execute(&pool).await?;
        }
        Ok(Self { pool })
    }

    /// The incidents open before a restart. Rows this version can't read
    /// (a rule renamed or removed) are skipped.
    pub async fn open_incidents(&self) -> Result<Vec<Incident>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT subject, rule, severity, started_at, opened_at, notified_at, detail \
             FROM health_incidents",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().filter_map(incident_from_row).collect())
    }

    /// The "resolved" messages not delivered before a restart.
    pub async fn pending_recoveries(&self) -> Result<Vec<Notice>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT subject, rule, severity, started_at, opened_at, notified_at, detail, \
             resolved_at, resolution FROM health_pending_recoveries ORDER BY resolved_at",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                Some(Notice::Resolved {
                    incident: incident_from_row(row)?,
                    at: row.get::<i64, _>("resolved_at").try_into().ok()?,
                    detail: row.get("resolution"),
                })
            })
            .collect())
    }

    /// Records an incident change from the tracker.
    pub async fn apply(&self, change: &Change) -> Result<(), sqlx::Error> {
        match change {
            Change::Upsert(incident) => {
                sqlx::query(
                    r#"
                    INSERT INTO health_incidents
                        (subject, rule, severity, started_at, opened_at, notified_at, detail)
                    VALUES (?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT(subject, rule) DO UPDATE SET
                        severity = excluded.severity,
                        started_at = excluded.started_at,
                        opened_at = excluded.opened_at,
                        notified_at = excluded.notified_at,
                        detail = excluded.detail
                    "#,
                )
                .bind(incident.subject.key())
                .bind(incident.rule.as_str())
                .bind(incident.severity.as_str())
                .bind(incident.started_at as i64)
                .bind(incident.opened_at as i64)
                .bind(incident.notified_at.map(|at| at as i64))
                .bind(&incident.detail)
                .execute(&self.pool)
                .await?;
            }
            Change::Delete(subject, rule) => {
                sqlx::query("DELETE FROM health_incidents WHERE subject = ? AND rule = ?")
                    .bind(subject.key())
                    .bind(rule.as_str())
                    .execute(&self.pool)
                    .await?;
            }
            Change::Resolve {
                incident,
                at,
                detail,
            } => {
                // One transaction: the incident is never both open and
                // pending, nor neither.
                let mut tx = self.pool.begin().await?;
                sqlx::query(
                    r#"
                    INSERT OR REPLACE INTO health_pending_recoveries
                        (subject, rule, severity, started_at, opened_at, notified_at, detail,
                         resolved_at, resolution)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                    "#,
                )
                .bind(incident.subject.key())
                .bind(incident.rule.as_str())
                .bind(incident.severity.as_str())
                .bind(incident.started_at as i64)
                .bind(incident.opened_at as i64)
                .bind(incident.notified_at.map(|at| at as i64))
                .bind(&incident.detail)
                .bind(*at as i64)
                .bind(detail)
                .execute(&mut *tx)
                .await?;
                sqlx::query("DELETE FROM health_incidents WHERE subject = ? AND rule = ?")
                    .bind(incident.subject.key())
                    .bind(incident.rule.as_str())
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
            }
            Change::Delivered { subject, rule, at } => {
                sqlx::query(
                    "DELETE FROM health_pending_recoveries \
                     WHERE subject = ? AND rule = ? AND resolved_at = ?",
                )
                .bind(subject.key())
                .bind(rule.as_str())
                .bind(*at as i64)
                .execute(&self.pool)
                .await?;
            }
        }
        Ok(())
    }

    /// The highest `seq` accepted from `pubkey`, and the event that had it.
    pub async fn accepted(&self, pubkey: &PublicKey) -> Result<Option<Accepted>, sqlx::Error> {
        let row = sqlx::query("SELECT seq, event_id FROM health_accepted_seq WHERE pubkey = ?")
            .bind(pubkey.to_hex())
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|row| {
            Some(Accepted {
                seq: row.get::<i64, _>("seq").try_into().ok()?,
                id: EventId::from_hex(row.get::<&str, _>("event_id")).ok()?,
            })
        }))
    }

    /// Records a newly accepted revision. Never lowers the stored `seq`.
    pub async fn accept(&self, pubkey: &PublicKey, accepted: Accepted) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO health_accepted_seq (pubkey, seq, event_id) VALUES (?, ?, ?)
            ON CONFLICT(pubkey) DO UPDATE SET seq = excluded.seq, event_id = excluded.event_id
            WHERE excluded.seq > health_accepted_seq.seq
            "#,
        )
        .bind(pubkey.to_hex())
        .bind(accepted.seq as i64)
        .bind(accepted.id.to_hex())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[cfg(test)]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn last_heartbeat(&self) -> Result<Option<LastHeartbeat>, sqlx::Error> {
        let row = sqlx::query("SELECT seq, created_at FROM health_heartbeat WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.and_then(|row| {
            Some(LastHeartbeat {
                seq: row.get::<i64, _>("seq").try_into().ok()?,
                created_at: row.get::<i64, _>("created_at").try_into().ok()?,
            })
        }))
    }

    pub async fn save_heartbeat(&self, last: LastHeartbeat) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO health_heartbeat (id, seq, created_at) VALUES (1, ?, ?)
            ON CONFLICT(id) DO UPDATE SET seq = excluded.seq, created_at = excluded.created_at
            "#,
        )
        .bind(last.seq as i64)
        .bind(last.created_at as i64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn incident_from_row(row: &sqlx::sqlite::SqliteRow) -> Option<Incident> {
    Some(Incident {
        subject: Subject::parse(row.get::<&str, _>("subject"))?,
        rule: RuleId::parse(row.get::<&str, _>("rule"))?,
        severity: Severity::parse(row.get::<&str, _>("severity"))?,
        started_at: row.get::<i64, _>("started_at").try_into().ok()?,
        opened_at: row.get::<i64, _>("opened_at").try_into().ok()?,
        notified_at: row
            .get::<Option<i64>, _>("notified_at")
            .and_then(|at| at.try_into().ok()),
        detail: row.get("detail"),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::db::DisputeMessageStore;
    use nostr_sdk::prelude::Keys;

    /// A store on a fresh database, kept alive by the returned directory.
    pub async fn store() -> (tempfile::TempDir, HealthStore) {
        let dir = tempfile::tempdir().unwrap();
        let disputes = DisputeMessageStore::new(&dir.path().join("disputes.db"))
            .await
            .unwrap();
        let store = HealthStore::new(disputes.pool()).await.unwrap();
        (dir, store)
    }

    fn incident(notified_at: Option<u64>) -> Incident {
        Incident {
            subject: Subject::Mostro(Keys::generate().public_key()),
            rule: RuleId::BlockLag,
            severity: Severity::Critical,
            started_at: 100,
            opened_at: 200,
            notified_at,
            detail: "LND is 136 blocks behind".into(),
        }
    }

    #[tokio::test]
    async fn open_incidents_survive_a_restart_until_deleted() {
        let (_dir, store) = store().await;
        let pending = incident(None);
        let delivered = Incident {
            notified_at: Some(300),
            ..pending.clone()
        };

        store.apply(&Change::Upsert(pending)).await.unwrap();
        store
            .apply(&Change::Upsert(delivered.clone()))
            .await
            .unwrap();
        assert_eq!(
            store.open_incidents().await.unwrap(),
            vec![delivered.clone()]
        );

        store
            .apply(&Change::Delete(delivered.subject, delivered.rule))
            .await
            .unwrap();
        assert!(store.open_incidents().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_pending_recovery_survives_the_same_rule_opening_again() {
        let (_dir, store) = store().await;
        let incident = incident(Some(300));
        let (subject, rule) = (incident.subject.clone(), incident.rule);
        store
            .apply(&Change::Upsert(incident.clone()))
            .await
            .unwrap();

        store
            .apply(&Change::Resolve {
                incident: incident.clone(),
                at: 900,
                detail: Some("back".into()),
            })
            .await
            .unwrap();
        assert!(store.open_incidents().await.unwrap().is_empty());

        // The rule opens again while Telegram still hasn't taken the
        // recovery: both are kept.
        let reopened = Incident {
            started_at: 950,
            opened_at: 1000,
            notified_at: None,
            ..incident.clone()
        };
        store
            .apply(&Change::Upsert(reopened.clone()))
            .await
            .unwrap();

        assert_eq!(
            store.open_incidents().await.unwrap(),
            vec![reopened.clone()]
        );
        assert_eq!(
            store.pending_recoveries().await.unwrap(),
            vec![Notice::Resolved {
                incident,
                at: 900,
                detail: Some("back".into()),
            }]
        );

        store
            .apply(&Change::Delivered {
                subject,
                rule,
                at: 900,
            })
            .await
            .unwrap();
        assert!(store.pending_recoveries().await.unwrap().is_empty());
        assert_eq!(store.open_incidents().await.unwrap(), vec![reopened]);
    }

    #[tokio::test]
    async fn the_accepted_seq_persists_and_never_goes_down() {
        let (_dir, store) = store().await;
        let pubkey = Keys::generate().public_key();
        let newer = Accepted {
            seq: 9,
            id: EventId::from_byte_array([0; 32]),
        };
        let older = Accepted {
            seq: 4,
            id: EventId::from_byte_array([1; 32]),
        };

        assert_eq!(store.accepted(&pubkey).await.unwrap(), None);
        store.accept(&pubkey, newer).await.unwrap();
        store.accept(&pubkey, older).await.unwrap();

        assert_eq!(store.accepted(&pubkey).await.unwrap(), Some(newer));
    }

    #[tokio::test]
    async fn the_heartbeat_state_round_trips() {
        let (_dir, store) = store().await;
        let last = LastHeartbeat {
            seq: 1_792_000_000_123,
            created_at: 1_792_000_000,
        };

        store.save_heartbeat(last).await.unwrap();

        assert_eq!(store.last_heartbeat().await.unwrap(), Some(last));
    }
}
