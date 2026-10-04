//! Session writer lifecycle, journal creation and transactional finalization.
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, bail, ensure};
use orbit_agent::events::AgentEvent;
use orbit_model::types::ChatMessage;
use rusqlite::params;

use super::{SessionWriter, now_millis};

impl SessionWriter {
    /// Move only this locked session. Historical messages and events remain intact.
    /// Write receipts from earlier workspaces must never be offered for undo here.
    pub async fn move_to(
        &self,
        directory: &Path,
        git_baseline: Option<serde_json::Value>,
    ) -> Result<crate::types::Session> {
        let project = self.store.resolve_project(directory).await?;
        let path = directory.canonicalize()?.to_string_lossy().into_owned();
        let id = self.id.clone();
        let baseline = git_baseline
            .map(|value| serde_json::to_string(&value))
            .transpose()?;

        self.store
            .access(move |conn| {
                let tx =
                    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let running: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM runs WHERE session_id=?1 AND status='running')",
                    [&id],
                    |row| row.get(0),
                )?;
                ensure!(!running, "cannot move a running session");

                tx.execute(
                    "UPDATE sessions SET project_id=?2,workspace=?3,
                     workspace_since_run=COALESCE((SELECT MAX(id) FROM runs WHERE session_id=?1),0),
                     updated_at=unixepoch() WHERE id=?1",
                    params![id, project.id, path],
                )?;
                tx.execute("DELETE FROM tui_file_grants WHERE session_id=?1", [&id])?;
                tx.execute(
                    "DELETE FROM session_workspace_baselines WHERE session_id=?1",
                    [&id],
                )?;
                tx.execute(
                    "DELETE FROM session_git_baselines WHERE session_id=?1",
                    [&id],
                )?;

                if let Some(payload) = baseline {
                    tx.execute(
                        "INSERT INTO session_git_baselines(session_id,payload) VALUES(?1,?2)",
                        params![id, payload],
                    )?;
                }

                tx.commit()?;
                Ok(())
            })
            .await?;

        self.store.session(&self.id).await
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
            .access(move |conn| {
                conn.execute(
                    "UPDATE sessions SET model=?2, reasoning_effort=?3, updated_at=unixepoch() WHERE id=?1",
                    params![id, model, reasoning_effort],
                )?;
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
            .access(move |conn| {
                conn.execute(
                    "UPDATE sessions SET title=?2, updated_at=unixepoch() WHERE id=?1",
                    params![id, title],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn begin(&mut self, prompt: String, model: String) -> Result<()> {
        ensure!(self.active_run.is_none(), "a run is already active");

        let id = self.id.clone();
        let run = self.store.access(move |conn| {
            let transaction = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            transaction.execute("INSERT INTO runs(session_id,prompt) VALUES(?1,?2)", params![id,prompt])?;
            let run = transaction.last_insert_rowid();
            let title: String = prompt.chars().take(80).collect();
            transaction.execute("UPDATE sessions SET model=?2, updated_at=unixepoch(), title=CASE WHEN title='' THEN ?3 ELSE title END WHERE id=?1", params![id,model,title])?;
            transaction.commit()?;

            Ok(run)
        }).await?;
        self.active_run = Some(run);
        self.active_started = Some(std::time::Instant::now());

        Ok(())
    }

    pub async fn append(&self, events: Vec<AgentEvent>) -> Result<()> {
        let run = self.active_run.context("no active run")?;

        self.store
            .access(move |conn| {
                let transaction =
                    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

                for event in events {
                    transaction.execute(
                        "INSERT INTO events(run_id,payload,created_at_ms) VALUES(?1,?2,?3)",
                        params![run, serde_json::to_string(&event)?, now_millis()],
                    )?;
                }

                transaction.commit()?;

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
        self.store
            .access(move |conn| {
                let transaction =
                    conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;

                for event in events {
                    transaction.execute(
                        "INSERT INTO events(run_id,payload,created_at_ms) VALUES(?1,?2,?3)",
                        params![run, serde_json::to_string(&event)?, now_millis()],
                    )?;
                }

                if let Some((parent, invocation, result)) = agent_result {
                    let updated = transaction.execute(
                        "UPDATE agent_invocations SET result=?4
                         WHERE id=?1 AND parent_session_id=?2 AND child_session_id=?3
                           AND result IS NULL",
                        params![invocation, parent, id, serde_json::to_string(&result)?],
                    )?;
                    ensure!(
                        updated == 1,
                        "subagent invocation is unavailable or already finished"
                    );
                }

                transaction.execute(
                    "UPDATE runs SET status=?2,elapsed_ms=?3 WHERE id=?1",
                    params![run, status, elapsed_ms],
                )?;

                if status == "cancelled" {
                    transaction.execute(
                        "UPDATE agent_invocations SET delivered=1 WHERE parent_run_id=?1",
                        [run],
                    )?;
                }
                if let Some(history) = history {
                    transaction.execute(
                        "UPDATE sessions SET history=?2 WHERE id=?1",
                        params![id, serde_json::to_string(&history)?],
                    )?;
                }

                transaction.execute(
                    "UPDATE sessions SET updated_at=unixepoch() WHERE id=?1",
                    [id],
                )?;
                transaction.commit()?;

                Ok(())
            })
            .await?;
        self.active_run = None;
        self.active_started = None;

        Ok(())
    }
}
