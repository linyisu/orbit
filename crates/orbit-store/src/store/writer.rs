//! Session writer lifecycle, journal creation and transactional finalization.
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use crate::{database::now, entities::*};
use anyhow::{Context, Result, bail, ensure};
use orbit_agent::events::AgentEvent;
use orbit_model::types::ChatMessage;
use sea_orm::*;

use super::SessionWriter;

impl SessionWriter {
    /// Move only this locked session. Historical messages and events remain intact.
    /// Write receipts from earlier workspaces must never be offered for undo here.
    pub async fn move_to(
        &self,
        directory: &Path,
        git_baseline: Option<serde_json::Value>,
    ) -> Result<crate::types::Session> {
        // A worktree changes a child's execution directory, not the logical
        // project whose task board it shares with its parent.
        let project_id = if self.store.is_subagent(&self.id).await? {
            self.store.session(&self.id).await?.project_id
        } else {
            self.store.resolve_project(directory).await?.id
        };
        let path = directory.canonicalize()?.to_string_lossy().into_owned();
        let id = self.id.clone();
        let baseline = git_baseline
            .map(|value| serde_json::to_string(&value))
            .transpose()?;

        self.store
            .write_access(move |conn| async move {
                let tx = conn.begin().await?;
                let running = runs::Entity::find()
                    .filter(runs::Column::SessionId.eq(&id))
                    .filter(runs::Column::Status.eq("running"))
                    .count(&tx)
                    .await?
                    > 0;
                ensure!(!running, "cannot move a running session");
                let last = runs::Entity::find()
                    .filter(runs::Column::SessionId.eq(&id))
                    .order_by_desc(runs::Column::Id)
                    .one(&tx)
                    .await?
                    .map_or(0, |run| run.id);
                sessions::Entity::update_many()
                    .set(sessions::ActiveModel {
                        project_id: Set(Some(project_id.clone())),
                        workspace: Set(path),
                        workspace_since_run: Set(last),
                        updated_at: Set(now()),
                        ..Default::default()
                    })
                    .filter(sessions::Column::Id.eq(&id))
                    .exec(&tx)
                    .await?;
                crate::tasks::release_unfinished(&tx, &id, Some(&project_id)).await?;
                tui_file_grants::Entity::delete_many()
                    .filter(tui_file_grants::Column::SessionId.eq(&id))
                    .exec(&tx)
                    .await?;
                session_workspace_baselines::Entity::delete_by_id(&id)
                    .exec(&tx)
                    .await?;
                session_git_baselines::Entity::delete_by_id(&id)
                    .exec(&tx)
                    .await?;
                if let Some(payload) = baseline {
                    session_git_baselines::ActiveModel {
                        session_id: Set(id),
                        payload: Set(payload),
                        ..Default::default()
                    }
                    .insert(&tx)
                    .await?;
                }

                tx.commit().await?;
                Ok(())
            })
            .await?;

        self.store.session(&self.id).await
    }

    pub async fn append_context(&self, message: ChatMessage) -> Result<()> {
        ensure!(
            self.active_run.is_none(),
            "cannot append idle context during a run"
        );
        let id = self.id.clone();
        let database = self.store.path().to_owned();
        self.store
            .write_access(move |conn| async move {
                let tx = conn.begin().await?;
                let row = sessions::Entity::find_by_id(&id)
                    .one(&tx)
                    .await?
                    .context("session not found")?;
                let mut history: Vec<ChatMessage> = serde_json::from_str(&row.history)?;
                history.push(message);
                crate::attachments::register(
                    &tx,
                    &database,
                    &id,
                    crate::attachments::in_messages(&history),
                )
                .await?;
                let mut row = row.into_active_model();
                row.history = Set(serde_json::to_string(&history)?);
                row.updated_at = Set(now());
                row.update(&tx).await?;
                tx.commit().await?;
                Ok(())
            })
            .await
    }

    pub fn active_run_id(&self) -> Option<i64> {
        self.active_run
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub async fn set_model_profile(
        &self,
        model: String,
        reasoning_effort: Option<String>,
    ) -> Result<()> {
        let id = self.id.clone();

        self.store
            .write_access(move |conn| async move {
                sessions::Entity::update_many()
                    .set(sessions::ActiveModel {
                        model: Set(model),
                        reasoning_effort: Set(reasoning_effort),
                        updated_at: Set(now()),
                        ..Default::default()
                    })
                    .filter(sessions::Column::Id.eq(id))
                    .exec(&conn)
                    .await?;
                Ok(())
            })
            .await
    }

    /// Change only the user-visible label. The event log and stable session ID
    /// remain untouched, so `/resume` and project association keep working.
    pub async fn set_title(&self, title: String) -> Result<()> {
        ensure!(!title.trim().is_empty(), "session title must not be empty");
        ensure!(
            title.chars().count() <= 120,
            "session title exceeds 120 characters"
        );

        let id = self.id.clone();

        self.store
            .write_access(move |conn| async move {
                sessions::Entity::update_many()
                    .set(sessions::ActiveModel {
                        title: Set(title),
                        updated_at: Set(now()),
                        ..Default::default()
                    })
                    .filter(sessions::Column::Id.eq(id))
                    .exec(&conn)
                    .await?;
                Ok(())
            })
            .await
    }

    pub async fn begin(&mut self, prompt: String, model: String) -> Result<()> {
        ensure!(self.active_run.is_none(), "a run is already active");

        let id = self.id.clone();
        let run = self
            .store
            .write_access(move |conn| async move {
                let transaction = conn.begin().await?;
                let run = runs::ActiveModel {
                    session_id: Set(id.clone()),
                    prompt: Set(prompt.clone()),
                    ..Default::default()
                }
                .insert(&transaction)
                .await?
                .id;
                let row = sessions::Entity::find_by_id(&id)
                    .one(&transaction)
                    .await?
                    .context("session not found")?;
                let empty = row.title.is_empty();
                let mut row = row.into_active_model();
                row.model = Set(model);
                row.updated_at = Set(now());
                if empty {
                    row.title = Set(prompt.chars().take(80).collect());
                }
                row.update(&transaction).await?;
                transaction.commit().await?;

                Ok(run)
            })
            .await?;

        self.active_run = Some(run);
        self.active_started = Some(std::time::Instant::now());

        Ok(())
    }

    pub async fn append(&self, events: Vec<AgentEvent>) -> Result<()> {
        let run = self.active_run.context("no active run")?;

        self.store
            .write_access(move |conn| async move {
                let transaction = conn.begin().await?;

                crate::event_log::append(&transaction, run, events).await?;

                transaction.commit().await?;

                Ok(())
            })
            .await
    }

    pub fn execution_journal(&self) -> Result<Arc<dyn orbit_agent::checkpoint::ExecutionJournal>> {
        Ok(Arc::new(crate::execution::Journal {
            store: self.store.clone(),
            run: self.active_run.context("no active run")?,
            session: self.id.clone(),
            commit_lock: tokio::sync::Mutex::new(()),
            pending: Mutex::new(Vec::new()),
            pending_messages: Mutex::new(Vec::new()),
        }))
    }

    pub async fn finish(
        &mut self,
        status: &str,
        events: Vec<AgentEvent>,
        history: Option<Vec<ChatMessage>>,
    ) -> Result<()> {
        self.finish_with_result(status, events, history, None).await
    }

    pub async fn finish_subagent(
        &mut self,
        status: &str,
        history: Option<Vec<ChatMessage>>,
        parent: String,
        invocation: i64,
        result: serde_json::Value,
    ) -> Result<()> {
        self.finish_with_result(
            status,
            Vec::new(),
            history,
            Some((parent, invocation, result)),
        )
        .await
    }

    async fn finish_with_result(
        &mut self,
        status: &str,
        events: Vec<AgentEvent>,
        history: Option<Vec<ChatMessage>>,
        agent_result: Option<(String, i64, serde_json::Value)>,
    ) -> Result<()> {
        ensure!(
            matches!(status, "completed" | "failed" | "cancelled"),
            "invalid terminal status"
        );
        ensure!(
            (status == "completed") == history.is_some(),
            "only completed runs may update model history"
        );
        let Some(run) = self.active_run else {
            bail!("no active run")
        };

        let elapsed_ms = self
            .active_started
            .map(|started| started.elapsed().as_millis().min(i64::MAX as u128) as i64);
        let status = status.to_owned();
        let id = self.id.clone();
        let database = self.store.path().to_owned();
        self.store
            .write_access(move |conn| async move {
                let transaction = conn.begin().await?;

                crate::event_log::append(&transaction, run, events).await?;

                if let Some((parent, invocation, result)) = agent_result {
                    crate::tasks::release_unfinished(&transaction, &id, None).await?;
                    let updated = agent_invocations::Entity::update_many()
                        .col_expr(
                            agent_invocations::Column::Result,
                            sea_query::Expr::value(serde_json::to_string(&result)?),
                        )
                        .filter(agent_invocations::Column::Id.eq(invocation))
                        .filter(agent_invocations::Column::ParentSessionId.eq(parent))
                        .filter(agent_invocations::Column::ChildSessionId.eq(&id))
                        .filter(agent_invocations::Column::Result.is_null())
                        .exec(&transaction)
                        .await?
                        .rows_affected;

                    ensure!(
                        updated == 1,
                        "subagent invocation is unavailable or already finished"
                    );
                }

                runs::Entity::update_many()
                    .set(runs::ActiveModel {
                        status: Set(status.clone()),
                        elapsed_ms: Set(elapsed_ms),
                        ..Default::default()
                    })
                    .filter(runs::Column::Id.eq(run))
                    .exec(&transaction)
                    .await?;
                if status == "cancelled" {
                    agent_invocations::Entity::update_many()
                        .col_expr(
                            agent_invocations::Column::Delivered,
                            sea_query::Expr::value(true),
                        )
                        .filter(agent_invocations::Column::ParentRunId.eq(run))
                        .exec(&transaction)
                        .await?;
                }
                let mut row = sessions::ActiveModel {
                    updated_at: Set(now()),
                    ..Default::default()
                };
                if let Some(history) = history {
                    crate::attachments::register(
                        &transaction,
                        &database,
                        &id,
                        crate::attachments::in_messages(&history),
                    )
                    .await?;
                    row.history = Set(serde_json::to_string(&history)?);
                }
                sessions::Entity::update_many()
                    .set(row)
                    .filter(sessions::Column::Id.eq(id))
                    .exec(&transaction)
                    .await?;
                transaction.commit().await?;

                Ok(())
            })
            .await?;
        self.active_run = None;
        self.active_started = None;

        Ok(())
    }
}
