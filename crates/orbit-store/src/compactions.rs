//! Read-only audit view over durable compaction events. No project-side files.
use anyhow::{Result, ensure};
use orbit_agent::memory::{CompactionRecord, SummaryProgress};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

use crate::store::Store;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredCompaction {
    pub event_id: i64,
    pub run_id: i64,
    /// Only completed runs publish their final model context. A record in a
    /// failed/interrupted run is still useful evidence, not a resume checkpoint.
    pub run_status: String,
    pub record: CompactionRecord,
}

impl Store {
    pub async fn load_compaction_progress(
        &self,
        session_id: &str,
        fingerprint: &str,
    ) -> Result<Option<SummaryProgress>> {
        let session_id = session_id.to_owned();
        let fingerprint = fingerprint.to_owned();

        self.access(move |conn| {
            let payload: Option<String> = conn
                .query_row(
                    "SELECT payload FROM compaction_progress WHERE session_id=?1 AND source_fingerprint=?2",
                    rusqlite::params![session_id, fingerprint],
                    |row| row.get(0),
                )
                .optional()?;

            payload.map(|value| serde_json::from_str(&value).map_err(Into::into)).transpose()
        })
        .await
    }

    pub async fn save_compaction_progress(
        &self,
        session_id: &str,
        progress: &SummaryProgress,
    ) -> Result<()> {
        ensure!(
            progress.version == 1
                && progress.source_fingerprint.len() == 64
                && progress
                    .source_fingerprint
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit()),
            "invalid compaction progress identity"
        );

        let session_id = session_id.to_owned();
        let progress = progress.clone();

        self.access(move |conn| {
            let transaction = conn.transaction()?;
            let old: Option<String> = transaction
                .query_row(
                    "SELECT payload FROM compaction_progress WHERE session_id=?1 AND source_fingerprint=?2",
                    rusqlite::params![session_id, progress.source_fingerprint],
                    |row| row.get(0),
                )
                .optional()?;

            if let Some(old) = old {
                let old: SummaryProgress = serde_json::from_str(&old)?;
                ensure!(
                    (progress.next_record, progress.next_byte_offset)
                        >= (old.next_record, old.next_byte_offset),
                    "compaction progress cannot move backwards"
                );
            }

            transaction.execute(
                "INSERT INTO compaction_progress(session_id,source_fingerprint,payload)
                 VALUES(?1,?2,?3) ON CONFLICT(session_id,source_fingerprint)
                 DO UPDATE SET payload=excluded.payload,updated_at=unixepoch()",
                rusqlite::params![
                    session_id,
                    progress.source_fingerprint,
                    serde_json::to_string(&progress)?
                ],
            )?;
            transaction.commit()?;

            Ok(())
        })
        .await
    }

    pub async fn compactions(&self, session_id: &str) -> Result<Vec<StoredCompaction>> {
        let id = session_id.to_owned();

        self.access(move |conn| {
            conn.query_row("SELECT id FROM sessions WHERE id=?1", [&id], |_| Ok(()))?;
            let mut query = conn.prepare("SELECT e.id,e.run_id,r.status,json_extract(e.payload,'$.CompactionRecorded.record') FROM events e JOIN runs r ON r.id=e.run_id WHERE r.session_id=?1 AND json_type(e.payload,'$.CompactionRecorded.record')='object' ORDER BY e.id")?;
            let rows = query.query_map([id], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?;
            let mut records = Vec::new();

            for row in rows {
                let (event_id, run_id, run_status, record) = row?;
                records.push(StoredCompaction { event_id, run_id, run_status, record: serde_json::from_str(&record)? });
            }

            Ok(records)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_agent::memory::TaskSummary;

    #[tokio::test]
    async fn progress_is_durable_session_scoped_and_monotonic() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("orbit.db");
        let store = Store::open(&path).await.unwrap();
        let owner = store
            .create(
                directory.path().display().to_string(),
                "model".into(),
                false,
            )
            .await
            .unwrap();
        let foreign = store
            .create(
                directory.path().display().to_string(),
                "model".into(),
                false,
            )
            .await
            .unwrap();
        let progress = SummaryProgress {
            version: 1,
            source_fingerprint: "a".repeat(64),
            next_record: 2,
            next_byte_offset: 10,
            summary: TaskSummary {
                goal: "Continue the task".into(),
                constraints: vec![],
                decisions: vec![],
                completed: vec![],
                remaining: vec![],
                verification: vec![],
                uncertainty: vec![],
            },
            requests: 1,
            input_tokens: 1000,
        };
        store
            .save_compaction_progress(owner.id(), &progress)
            .await
            .unwrap();
        let reopened = Store::open(path).await.unwrap();

        assert_eq!(
            reopened
                .load_compaction_progress(owner.id(), &progress.source_fingerprint)
                .await
                .unwrap(),
            Some(progress.clone())
        );
        assert!(
            reopened
                .load_compaction_progress(foreign.id(), &progress.source_fingerprint)
                .await
                .unwrap()
                .is_none()
        );

        let mut backwards = progress;
        backwards.next_byte_offset = 0;

        assert!(
            reopened
                .save_compaction_progress(owner.id(), &backwards)
                .await
                .is_err()
        );
    }
}
