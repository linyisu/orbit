//! Workspace receipts, undo records and baselines, scoped to the session's current directory.
use crate::{
    entities::*,
    store::Store,
    types::{VerificationRecord, WorkspaceChange},
};
use anyhow::{Context, Result};
use orbit_agent::events::AgentEvent;
use sea_orm::*;
use std::collections::HashSet;

impl Store {
    /// Successful file-tool receipts from the event ledger, independent of Git state.
    pub async fn workspace_changes(&self, session_id: &str) -> Result<Vec<WorkspaceChange>> {
        let session_id = session_id.to_owned();

        self.read_access(move |conn| async move {
            let conn = conn.begin().await?;
            let undone = workspace_undos::Entity::find()
                .filter(workspace_undos::Column::SessionId.eq(&session_id))
                .all(&conn)
                .await?
                .into_iter()
                .map(|row| (row.run_id, row.call_id, row.path))
                .collect::<HashSet<_>>();
            let mut changes = Vec::new();

            for (run_id, run_status, payload) in workspace_events(&conn, &session_id).await? {
                let event: AgentEvent =
                    serde_json::from_str(&payload).context("stored event could not be decoded")?;

                if let AgentEvent::ToolFinished {
                    call,
                    changes: receipts,
                    ..
                } = event
                {
                    for change in receipts {
                        changes.push(WorkspaceChange {
                            run_id,
                            run_status: run_status.clone(),
                            call_id: call.id.clone(),
                            tool: call.function.name.clone(),
                            undone: undone.contains(&(
                                run_id,
                                call.id.clone(),
                                change.path.clone(),
                            )),
                            change,
                        });
                    }
                }
            }

            Ok(changes)
        })
        .await
    }

    /// Record a local undo separately from the immutable original write receipt.
    pub async fn record_workspace_undo(
        &self,
        session_id: &str,
        change: &WorkspaceChange,
    ) -> Result<()> {
        let session_id = session_id.to_owned();
        let run_id = change.run_id;
        let call_id = change.call_id.clone();
        let path = change.change.path.clone();

        self.write_access(move |conn| async move {
            workspace_undos::Entity::insert(workspace_undos::ActiveModel {
                session_id: Set(session_id),
                run_id: Set(run_id),
                call_id: Set(call_id),
                path: Set(path),
                ..Default::default()
            })
            .on_conflict(
                sea_query::OnConflict::columns([
                    workspace_undos::Column::SessionId,
                    workspace_undos::Column::RunId,
                    workspace_undos::Column::CallId,
                    workspace_undos::Column::Path,
                ])
                .do_nothing()
                .to_owned(),
            )
            .do_nothing()
            .exec(&conn)
            .await?;
            Ok(())
        })
        .await
    }

    /// Reconstruct successful `run_command` invocations from durable tool
    /// results. This deliberately excludes model prose and failed requests
    /// that never started execution.
    pub async fn verification_records(&self, session_id: &str) -> Result<Vec<VerificationRecord>> {
        let session_id = session_id.to_owned();

        self.read_access(move |conn| async move {
            let conn = conn.begin().await?;
            let mut records = Vec::new();
            let mut evidence_by_call = std::collections::HashMap::new();

            for (run_id, run_status, payload) in workspace_events(&conn, &session_id).await? {
                let event: AgentEvent =
                    serde_json::from_str(&payload).context("stored event could not be decoded")?;

                if let AgentEvent::VerificationRecorded { call_id, evidence } = event {
                    evidence_by_call.insert((run_id, call_id), evidence);
                    continue;
                }

                let AgentEvent::ToolFinished { call, content, .. } = event else {
                    continue;
                };
                if call.function.name != "run_command" {
                    continue;
                }

                let arguments: serde_json::Value =
                    serde_json::from_str(&call.function.arguments).unwrap_or_default();
                let result: serde_json::Value = serde_json::from_str(&content).unwrap_or_default();

                if result.get("exit_code").is_none() {
                    continue;
                }

                records.push(VerificationRecord {
                    run_id,
                    run_status,
                    command: arguments["command"].as_str().unwrap_or_default().to_owned(),
                    exit_code: result["exit_code"]
                        .as_i64()
                        .and_then(|code| i32::try_from(code).ok()),
                    timed_out: result["timed_out"].as_bool().unwrap_or(false),
                    cancelled: result["cancelled"].as_bool().unwrap_or(false),
                    elapsed_ms: result["elapsed_ms"].as_u64().unwrap_or(0),
                    evidence: evidence_by_call.remove(&(run_id, call.id)),
                });
            }

            Ok(records)
        })
        .await
    }

    pub async fn save_workspace_baseline(
        &self,
        session_id: &str,
        baseline: &orbit_runtime::verification::WorkspaceFingerprint,
    ) -> Result<()> {
        let id = session_id.to_owned();
        let payload = serde_json::to_string(baseline)?;
        self.write_access(move |conn| async move {
            session_workspace_baselines::Entity::insert(session_workspace_baselines::ActiveModel {
                session_id: Set(id),
                payload: Set(payload),
            })
            .on_conflict(
                sea_query::OnConflict::column(session_workspace_baselines::Column::SessionId)
                    .do_nothing()
                    .to_owned(),
            )
            .do_nothing()
            .exec(&conn)
            .await?;
            Ok(())
        })
        .await
    }
    pub async fn workspace_baseline(
        &self,
        session_id: &str,
    ) -> Result<Option<orbit_runtime::verification::WorkspaceFingerprint>> {
        let id = session_id.to_owned();
        self.read_access(move |conn| async move {
            session_workspace_baselines::Entity::find_by_id(id)
                .one(&conn)
                .await?
                .map(|row| serde_json::from_str(&row.payload).map_err(Into::into))
                .transpose()
        })
        .await
    }
    pub async fn save_git_baseline(
        &self,
        session_id: &str,
        baseline: &serde_json::Value,
    ) -> Result<()> {
        let id = session_id.to_owned();
        let payload = serde_json::to_string(baseline)?;
        self.write_access(move |conn| async move {
            session_git_baselines::Entity::insert(session_git_baselines::ActiveModel {
                session_id: Set(id),
                payload: Set(payload),
                captured_at: Set(crate::database::now()),
            })
            .on_conflict(
                sea_query::OnConflict::column(session_git_baselines::Column::SessionId)
                    .update_columns([
                        session_git_baselines::Column::Payload,
                        session_git_baselines::Column::CapturedAt,
                    ])
                    .to_owned(),
            )
            .exec(&conn)
            .await?;
            Ok(())
        })
        .await
    }
    pub async fn git_baseline(&self, session_id: &str) -> Result<Option<serde_json::Value>> {
        let id = session_id.to_owned();
        self.read_access(move |conn| async move {
            session_git_baselines::Entity::find_by_id(id)
                .one(&conn)
                .await?
                .map(|row| serde_json::from_str(&row.payload).map_err(Into::into))
                .transpose()
        })
        .await
    }
}

async fn workspace_events<C: ConnectionTrait>(
    conn: &C,
    id: &str,
) -> Result<Vec<(i64, String, String)>> {
    let Some(boundary) = sessions::Entity::find_by_id(id)
        .select_only()
        .column(sessions::Column::WorkspaceSinceRun)
        .into_tuple::<i64>()
        .one(conn)
        .await?
    else {
        return Ok(Vec::new());
    };
    Ok(events::Entity::find()
        .select_only()
        .column(runs::Column::Id)
        .column(runs::Column::Status)
        .column(events::Column::Payload)
        .join(JoinType::InnerJoin, events::Relation::Run.def())
        .filter(runs::Column::SessionId.eq(id))
        .filter(runs::Column::Id.gt(boundary))
        .order_by_asc(runs::Column::Id)
        .order_by_asc(events::Column::Id)
        .into_tuple()
        .all(conn)
        .await?)
}
