//! Read-only audit view over durable compaction events. No project-side files.
use crate::entities::*;
use anyhow::{Result, ensure};
use orbit_agent::memory::{CompactionRecord, SummaryProgress};
use sea_orm::sea_query::Expr;
use sea_orm::*;
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

        self.read_access(move |conn| async move {
            compaction_progress::Entity::find_by_id((session_id, fingerprint))
                .one(&conn)
                .await?
                .map(|row| serde_json::from_str(&row.payload).map_err(Into::into))
                .transpose()
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

        self.write_access(move |conn| async move {
            let tx = conn.begin().await?;
            if let Some(row) = compaction_progress::Entity::find_by_id((
                session_id.clone(),
                progress.source_fingerprint.clone(),
            ))
            .one(&tx)
            .await?
            {
                let old: SummaryProgress = serde_json::from_str(&row.payload)?;
                ensure!(
                    (progress.next_record, progress.next_byte_offset)
                        >= (old.next_record, old.next_byte_offset),
                    "compaction progress cannot move backwards"
                );
            }
            compaction_progress::Entity::insert(compaction_progress::ActiveModel {
                session_id: Set(session_id),
                source_fingerprint: Set(progress.source_fingerprint.clone()),
                payload: Set(serde_json::to_string(&progress)?),
                updated_at: Set(crate::database::now()),
            })
            .on_conflict(
                sea_query::OnConflict::columns([
                    compaction_progress::Column::SessionId,
                    compaction_progress::Column::SourceFingerprint,
                ])
                .update_columns([
                    compaction_progress::Column::Payload,
                    compaction_progress::Column::UpdatedAt,
                ])
                .to_owned(),
            )
            .exec(&tx)
            .await?;
            tx.commit().await?;
            Ok(())
        })
        .await
    }
    pub async fn compactions(&self, session_id: &str) -> Result<Vec<StoredCompaction>> {
        let id = session_id.to_owned();
        self.read_access(move |conn| async move {
            let tx = conn.begin().await?;
            ensure!(
                sessions::Entity::find_by_id(&id).count(&tx).await? > 0,
                "session not found"
            );
            let json = |function: &str| {
                sea_query::Func::cust(sea_query::Alias::new(function)).args([
                    Expr::col((events::Entity, events::Column::Payload)).into(),
                    Expr::value("$.CompactionRecorded.record"),
                ])
            };
            let rows: Vec<(i64, i64, String, String)> = events::Entity::find()
                .select_only()
                .column(events::Column::Id)
                .column(events::Column::RunId)
                .column(runs::Column::Status)
                .column_as(Expr::expr(json("json_extract")), "record")
                .join(JoinType::InnerJoin, events::Relation::Run.def())
                .filter(runs::Column::SessionId.eq(id))
                .filter(Expr::expr(json("json_type")).eq("object"))
                .order_by_asc(events::Column::Id)
                .into_tuple()
                .all(&tx)
                .await?;
            rows.into_iter()
                .map(|(event_id, run_id, run_status, record)| {
                    Ok(StoredCompaction {
                        event_id,
                        run_id,
                        run_status,
                        record: serde_json::from_str(&record)?,
                    })
                })
                .collect()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use orbit_agent::memory::TaskSummary;

    use super::*;

    #[tokio::test]
    async fn progress_is_durable_session_scoped_and_monotonic() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("orbit.db");
        let store = Store::open(&path).await.unwrap();
        let owner = store
            .create(directory.path().display().to_string(), "model".into())
            .await
            .unwrap();
        let foreign = store
            .create(directory.path().display().to_string(), "model".into())
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
