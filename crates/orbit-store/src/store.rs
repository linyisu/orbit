mod writer;

use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use orbit_agent::events::AgentEvent;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::types::{
    MemoryCategory, ProjectPlugin, ProjectTask, Run, Session, SessionMemory, Snapshot, TaskStatus,
    VerificationRecord, WorkspaceChange,
};

const CONNECTION_BUSY_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
    path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionStorageUsage {
    pub id: String,
    pub title: String,
    /// Stored payload bytes, excluding SQLite indexes and unused pages.
    pub payload_bytes: u64,
}

// Holding the file keeps a process-scoped writer lease, including between runs.
// Readers can inspect a live session without acquiring this lease.
pub struct SessionWriter {
    store: Store,
    id: String,
    _lock: File,
    active_run: Option<i64>,
    active_started: Option<std::time::Instant>,
}

impl Store {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = std::path::absolute(path)?;

        tokio::task::spawn_blocking(move || {
            let parent = path.parent().context("database has no parent")?;
            std::fs::create_dir_all(parent)?;
            private_file(&path)?;
            let connection = Connection::open(&path)?;
            connection.busy_timeout(CONNECTION_BUSY_TIMEOUT)?;
            connection.execute_batch(
                "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA synchronous=FULL;",
            )?;
            crate::schema::migrate(&connection)?;

            Ok(Self {
                connection: Arc::new(Mutex::new(connection)),
                path: path.canonicalize()?,
            })
        })
        .await?
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) async fn access<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let connection = self.connection.clone();

        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| anyhow::anyhow!("database lock poisoned"))?;
            operation(&mut connection)
        })
        .await?
    }

    /// Long history scans use a separate WAL reader so they do not hold the
    /// process-wide writer connection while decoding potentially large logs.
    pub(crate) async fn read_access<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let path = self.path.clone();

        tokio::task::spawn_blocking(move || {
            let mut connection = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            connection.busy_timeout(CONNECTION_BUSY_TIMEOUT)?;

            operation(&mut connection)
        })
        .await?
    }

    pub async fn create(
        &self,
        workspace: String,
        model: String,
        demo: bool,
    ) -> Result<SessionWriter> {
        let project = self.resolve_project(&workspace).await?;
        let id = self
            .access(move |conn| {
                let id: String =
                    conn.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?;
                conn.execute(
                    "INSERT INTO sessions(id,workspace,model,demo,project_id) VALUES(?1,?2,?3,?4,?5)",
                    params![id, workspace, model, demo, project.id],
                )?;

                Ok(id)
            })
            .await?;

        self.acquire(&id).await
    }

    pub async fn list(&self) -> Result<Vec<Session>> {
        self.list_sessions(false).await
    }

    /// Conversations enter history when their first run begins, including failed runs.
    pub async fn list_resumable(&self) -> Result<Vec<Session>> {
        self.list_sessions(true).await
    }

    async fn list_sessions(&self, started_only: bool) -> Result<Vec<Session>> {
        self.access(move |conn| {
            let mut statement = conn.prepare(
                "SELECT s.id,COALESCE(p.path,s.workspace),s.model,s.reasoning_effort,
                        s.demo,s.title,s.updated_at,s.project_id
                 FROM sessions s LEFT JOIN projects p ON p.id=s.project_id
                 WHERE NOT EXISTS(SELECT 1 FROM agent_sessions a WHERE a.session_id=s.id)
                   AND (?1 = 0 OR EXISTS(SELECT 1 FROM runs r WHERE r.session_id=s.id))
                 ORDER BY s.updated_at DESC, s.rowid DESC",
            )?;

            Ok(statement
                .query_map([started_only], session_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    pub async fn session_storage_usage(&self) -> Result<Vec<SessionStorageUsage>> {
        self.read_access(|conn| {
            let mut statement = conn.prepare(
                "SELECT s.id,s.title,
                    length(CAST(s.history AS BLOB))
                    + COALESCE((SELECT sum(length(CAST(r.prompt AS BLOB))) FROM runs r WHERE r.session_id=s.id),0)
                    + COALESCE((SELECT sum(length(CAST(e.payload AS BLOB))) FROM events e JOIN runs r ON r.id=e.run_id WHERE r.session_id=s.id),0)
                    + COALESCE((SELECT sum(length(CAST(m.payload AS BLOB))) FROM session_messages m WHERE m.session_id=s.id),0)
                    + COALESCE((SELECT sum(length(CAST(am.payload AS BLOB))) FROM context_archive_messages am JOIN context_archives a ON a.id=am.archive_id WHERE a.session_id=s.id),0)
                    + COALESCE((SELECT sum(length(CAST(c.payload AS BLOB))) FROM execution_checkpoints c JOIN runs r ON r.id=c.run_id WHERE r.session_id=s.id),0)
                    + COALESCE((SELECT sum(length(CAST(p.payload AS BLOB))) FROM compaction_progress p WHERE p.session_id=s.id),0)
                    + COALESCE((SELECT sum(length(CAST(m.content AS BLOB))) FROM session_memories m WHERE m.session_id=s.id),0)
                    + COALESCE((SELECT length(CAST(b.payload AS BLOB)) FROM session_git_baselines b WHERE b.session_id=s.id),0)
                    + COALESCE((SELECT length(CAST(b.payload AS BLOB)) FROM session_workspace_baselines b WHERE b.session_id=s.id),0)
                    + COALESCE((SELECT sum(length(CAST(q.payload AS BLOB))) FROM side_questions q WHERE q.session_id=s.id),0)
                    + COALESCE((SELECT sum(length(CAST(u.payload AS BLOB))) FROM user_inputs u WHERE u.session_id=s.id),0)
                 FROM sessions s ORDER BY 3 DESC,s.id",
            )?;

            statement
                .query_map([], |row| {
                    Ok(SessionStorageUsage {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        payload_bytes: row.get::<_, i64>(2)?.max(0) as u64,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn session(&self, id: &str) -> Result<Session> {
        let id = id.to_owned();

        self.access(move |conn| {
            conn.query_row(
                "SELECT s.id,COALESCE(p.path,s.workspace),s.model,s.reasoning_effort,s.demo,s.title,s.updated_at,s.project_id FROM sessions s LEFT JOIN projects p ON p.id=s.project_id WHERE s.id=?1",
                [&id], session_row,
            ).context("session not found")
        }).await
    }

    pub async fn load(&self, id: &str) -> Result<Snapshot> {
        let id = id.to_owned();

        self.read_access(move |conn| {
            // One read transaction prevents mixing a newly committed history with older events.
            let transaction = conn.transaction()?;
            let session = transaction
                .query_row(
                    "SELECT s.id,COALESCE(p.path,s.workspace),s.model,s.reasoning_effort,s.demo,s.title,s.updated_at,s.project_id FROM sessions s LEFT JOIN projects p ON p.id=s.project_id WHERE s.id=?1",
                    [&id],
                    session_row,
                )
                .context("session not found")?;
            let history: String =
                transaction.query_row("SELECT history FROM sessions WHERE id=?1", [&id], |r| {
                    r.get(0)
                })?;
            let mut statement = transaction
                .prepare("SELECT id,prompt,status,elapsed_ms FROM runs WHERE session_id=?1 ORDER BY id")?;
            let rows = statement
                .query_map([&id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<i64>>(3)?.map(|milliseconds| {
                            milliseconds.max(0) as u64
                        }),
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut runs = Vec::new();

            for (run_id, prompt, status, elapsed_ms) in rows {
                let mut events = transaction
                    .prepare("SELECT payload FROM events WHERE run_id=?1 ORDER BY id")?;
                let mut decoded = Vec::new();

                for payload in events.query_map([run_id], |r| r.get::<_, String>(0))? {
                    decoded.push(serde_json::from_str(&payload?)?);
                }

                // The first canonical message belongs to this exact run, even
                // after compaction has replaced the model's history projection.
                let record = transaction.query_row(
                    "SELECT id,run_id,payload,sha256 FROM session_messages WHERE run_id=?1 ORDER BY ordinal LIMIT 1",
                    [run_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                ).optional()?;
                let message = record.map(|(id, run, payload, hash)|
                    crate::session_log::decode(id, run, payload, hash)
                ).transpose()?;
                let images = match message.map(|record| record.message) {
                    Some(orbit_model::types::ChatMessage::User { content, attachments }) if content == prompt =>
                        attachments.into_iter().map(|attachment| match attachment {
                            orbit_model::types::Attachment::Image { name, path, .. } =>
                                orbit_protocol::MessageImage { name, path: path.to_string_lossy().into_owned() },
                        }).collect(),
                    _ => Vec::new(),
                };
                runs.push(Run {
                    images,
                    prompt,
                    status,
                    elapsed_ms,
                    events: decoded,
                });
            }

            let checkpoint: Option<String> = transaction
                .query_row(
                    "SELECT c.payload FROM runs r
                     JOIN execution_checkpoints c ON c.run_id=r.id
                     WHERE r.session_id=?1
                       AND r.id>COALESCE((SELECT max(id) FROM runs
                                          WHERE session_id=?1 AND status='completed'),0)
                       AND r.status!='completed'
                     ORDER BY r.id DESC LIMIT 1",
                    [&id],
                    |row| row.get(0),
                )
                .optional()?;
            let mut effective_history = serde_json::from_str(&history)?;
            let recovery = if let Some(payload) = checkpoint {
                let checkpoint: orbit_agent::checkpoint::ExecutionCheckpoint =
                    serde_json::from_str(&payload)?;
                let (restored, unresolved_tools) = checkpoint.recovered_history();
                effective_history = restored;

                Some(crate::types::RecoveryInfo {
                    turns: checkpoint.turns,
                    tool_calls: checkpoint.tool_calls,
                    unresolved_tools,
                })
            } else {
                None
            };

            Ok(Snapshot {
                session,
                history: effective_history,
                runs,
                recovery,
            })
        })
        .await
    }

    /// Explicitly remove one closed conversation and its private sidecar data.
    /// No automatic retention policy deletes canonical history.
    pub async fn delete_session(&self, id: &str) -> Result<bool> {
        ensure!(
            id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid session ID"
        );

        let id = id.to_owned();
        let path = self.path.clone();
        let parent_id = id.clone();
        let parent_lease = tokio::task::spawn_blocking(move || -> Result<File> {
            let root = path.with_extension("session-locks");
            std::fs::create_dir_all(&root)?;
            ensure_real_directory_if_present(&root)?;
            let lock = private_file(&root.join(parent_id))?;
            lock.try_lock()
                .context("session is open; close it before deleting")?;
            Ok(lock)
        })
        .await??;
        let mut session_ids = self
            .subagents(&id)
            .await?
            .into_iter()
            .map(|a| a.id)
            .collect::<Vec<_>>();
        let lease_ids = session_ids.clone();
        let lock_root = self.path.with_extension("session-locks");
        let leases = tokio::task::spawn_blocking(move || -> Result<Vec<File>> {
            lease_ids
                .into_iter()
                .map(|session_id| {
                    let lock = private_file(&lock_root.join(session_id))?;
                    lock.try_lock()
                        .context("subagent is open; close it before deleting")?;
                    Ok(lock)
                })
                .collect()
        })
        .await??;

        session_ids.push(id.clone());

        let sidecar_path = self.path.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            for extension in ["workspaces", "command-output", "artifacts"] {
                let root = sidecar_path.with_extension(extension);
                ensure_real_directory_if_present(&root)?;
            }

            Ok(())
        })
        .await??;

        let session_id = id.clone();
        let delete_ids = session_ids.clone();
        let deleted = self
            .access(move |conn| {
                let transaction = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let exists: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
                    [&session_id],
                    |row| row.get(0),
                )?;

                if exists {
                    for id in delete_ids {
                        transaction.execute("DELETE FROM events WHERE run_id IN (SELECT id FROM runs WHERE session_id=?1)", [&id])?;
                        transaction.execute("DELETE FROM runs WHERE session_id=?1", [&id])?;
                        transaction.execute("DELETE FROM sessions WHERE id=?1", [&id])?;
                    }
                }

                transaction.commit()?;

                Ok(exists)
            })
            .await?;

        let cleanup_path = self.path.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            for id in session_ids {
                for extension in ["workspaces", "command-output", "artifacts"] {
                    let root = cleanup_path.with_extension(extension);

                    if !ensure_real_directory_if_present(&root)? {
                        continue;
                    }

                    let directory = root.join(&id);

                    match std::fs::symlink_metadata(&directory) {
                        Ok(metadata) if metadata.file_type().is_symlink() => {
                            std::fs::remove_file(&directory)?;
                        }
                        Ok(metadata) if metadata.is_dir() => {
                            std::fs::remove_dir_all(&directory)?;
                        }
                        Ok(_) => {
                            std::fs::remove_file(&directory)?;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }

                std::fs::remove_file(cleanup_path.with_extension("session-locks").join(id))?;
            }

            Ok(())
        })
        .await??;
        drop(leases);
        drop(parent_lease);

        Ok(deleted)
    }

    pub async fn reclaim_unused_space(&self) -> Result<()> {
        self.access(|conn| {
            conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
            Ok(())
        })
        .await
    }

    /// Successful file-tool receipts from the event ledger, independent of Git state.
    pub async fn workspace_changes(&self, session_id: &str) -> Result<Vec<WorkspaceChange>> {
        let session_id = session_id.to_owned();

        self.read_access(move |conn| {
            let undone = conn
                .prepare("SELECT run_id,call_id,path FROM workspace_undos WHERE session_id=?1")?
                .query_map([&session_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<HashSet<_>>>()?;
            let mut statement = conn.prepare(
                "SELECT r.id,r.status,e.payload
                 FROM runs r JOIN events e ON e.run_id=r.id
                 WHERE r.session_id=?1 AND r.id > (SELECT workspace_since_run FROM sessions WHERE id=?1) ORDER BY r.id,e.id",
            )?;
            let mut changes = Vec::new();

            for row in statement.query_map([session_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })? {
                let (run_id, run_status, payload) = row?;
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

        self.access(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO workspace_undos(session_id,run_id,call_id,path) VALUES(?1,?2,?3,?4)",
                params![session_id, run_id, call_id, path],
            )?;
            Ok(())
        })
        .await
    }

    /// Reconstruct successful `run_command` invocations from durable tool
    /// results. This deliberately excludes model prose and failed requests
    /// that never reached the sandbox.
    pub async fn verification_records(&self, session_id: &str) -> Result<Vec<VerificationRecord>> {
        let session_id = session_id.to_owned();

        self.read_access(move |conn| {
            let mut statement = conn.prepare(
                "SELECT r.id,r.status,e.payload
                 FROM runs r JOIN events e ON e.run_id=r.id
                 WHERE r.session_id=?1 AND r.id > (SELECT workspace_since_run FROM sessions WHERE id=?1) ORDER BY r.id,e.id",
            )?;
            let mut records = Vec::new();
            let mut evidence_by_call = std::collections::HashMap::new();

            for row in statement.query_map([session_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })? {
                let (run_id, run_status, payload) = row?;
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
        self.access(move |conn| {conn.execute("INSERT OR IGNORE INTO session_workspace_baselines(session_id,payload) VALUES(?1,?2)",params![id,payload])?;Ok(())}).await
    }

    pub async fn workspace_baseline(
        &self,
        session_id: &str,
    ) -> Result<Option<orbit_runtime::verification::WorkspaceFingerprint>> {
        let id = session_id.to_owned();

        self.access(move |conn| {
            let payload: Option<String> = conn
                .query_row(
                    "SELECT payload FROM session_workspace_baselines WHERE session_id=?1",
                    [id],
                    |r| r.get(0),
                )
                .optional()?;

            payload
                .map(|s| serde_json::from_str(&s))
                .transpose()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn save_git_baseline(
        &self,
        session_id: &str,
        baseline: &serde_json::Value,
    ) -> Result<()> {
        let session_id = session_id.to_owned();
        let payload = serde_json::to_string(baseline)?;

        self.access(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO session_git_baselines(session_id,payload) VALUES(?1,?2)",
                params![session_id, payload],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn git_baseline(&self, session_id: &str) -> Result<Option<serde_json::Value>> {
        let session_id = session_id.to_owned();

        self.access(move |conn| {
            let payload = conn
                .query_row(
                    "SELECT payload FROM session_git_baselines WHERE session_id=?1",
                    [session_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;

            payload
                .map(|payload| serde_json::from_str(&payload).map_err(Into::into))
                .transpose()
        })
        .await
    }

    pub async fn acquire(&self, id: &str) -> Result<SessionWriter> {
        ensure!(
            id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid session ID"
        );

        self.session(id).await?;
        let id = id.to_owned();
        let lock_dir = self.path.with_extension("session-locks");
        let lock_id = id.clone();
        let lock = tokio::task::spawn_blocking(move || -> Result<File> {
            std::fs::create_dir_all(&lock_dir)?;
            let file = private_file(&lock_dir.join(lock_id))?;
            file.try_lock().map_err(|error| match error {
                std::fs::TryLockError::WouldBlock => {
                    anyhow::anyhow!("该会话的写入锁正在被占用，当前服务暂时无法接入。请稍后重连。")
                }
                std::fs::TryLockError::Error(error) => {
                    anyhow::Error::new(error).context("无法获取会话锁")
                }
            })?;

            Ok(file)
        })
        .await??;
        let recovery_id = id.clone();
        self.access(move |conn| {
            conn.execute(
                "UPDATE runs SET status='interrupted' WHERE session_id=?1 AND status='running'",
                [recovery_id],
            )?;
            Ok(())
        })
        .await?;

        Ok(SessionWriter {
            store: self.clone(),
            id,
            _lock: lock,
            active_run: None,
            active_started: None,
        })
    }

    pub async fn tasks(&self, project_id: &str, include_closed: bool) -> Result<Vec<ProjectTask>> {
        let project_id = project_id.to_owned();

        self.access(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id,project_id,parent_id,title,details,status,priority,created_at,updated_at
                 FROM project_tasks WHERE project_id=?1
                 AND (?2 OR status NOT IN ('completed','cancelled'))
                 ORDER BY CASE status WHEN 'in_progress' THEN 0 WHEN 'blocked' THEN 1 ELSE 2 END,
                          priority DESC, id",
            )?;

            statement
                .query_map(params![project_id, include_closed], task_row)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn create_task(
        &self,
        project_id: &str,
        parent_id: Option<i64>,
        title: String,
        details: String,
        priority: i64,
    ) -> Result<ProjectTask> {
        validate_task_fields(&title, &details, priority)?;
        let project_id = project_id.to_owned();

        self.access(move |conn| {
            let transaction = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let project_exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
                [&project_id],
                |row| row.get(0),
            )?;

            ensure!(project_exists, "project not found");

            if let Some(parent_id) = parent_id {
                let valid_parent: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM project_tasks WHERE id=?1 AND project_id=?2)",
                    params![parent_id, project_id],
                    |row| row.get(0),
                )?;

                ensure!(valid_parent, "parent task does not belong to this project");
            }

            transaction.execute(
                "INSERT INTO project_tasks(project_id,parent_id,title,details,priority) VALUES(?1,?2,?3,?4,?5)",
                params![project_id, parent_id, title, details, priority],
            )?;
            let id = transaction.last_insert_rowid();
            let task = transaction.query_row(
                "SELECT id,project_id,parent_id,title,details,status,priority,created_at,updated_at FROM project_tasks WHERE id=?1",
                [id],
                task_row,
            )?;

            transaction.commit()?;

            Ok(task)
        })
        .await
    }

    pub async fn update_task(
        &self,
        project_id: &str,
        id: i64,
        title: Option<String>,
        details: Option<String>,
        status: Option<TaskStatus>,
        priority: Option<i64>,
    ) -> Result<ProjectTask> {
        if let Some(title) = &title {
            validate_task_fields(
                title,
                details.as_deref().unwrap_or(""),
                priority.unwrap_or(0),
            )?;
        }

        if let Some(details) = &details {
            ensure!(details.len() <= 8192, "task details exceed 8192 bytes");
        }

        if let Some(priority) = priority {
            ensure!(
                (-100..=100).contains(&priority),
                "task priority must be between -100 and 100"
            );
        }

        let project_id = project_id.to_owned();

        self.access(move |conn| {
            let transaction = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let changed = transaction.execute(
                "UPDATE project_tasks
                 SET title=COALESCE(?3,title), details=COALESCE(?4,details),
                     status=COALESCE(?5,status), priority=COALESCE(?6,priority), updated_at=unixepoch()
                 WHERE id=?1 AND project_id=?2",
                params![id, project_id, title, details, status.map(TaskStatus::as_str), priority],
            )?;

            ensure!(changed == 1, "task not found in this project");

            let task = transaction.query_row(
                "SELECT id,project_id,parent_id,title,details,status,priority,created_at,updated_at FROM project_tasks WHERE id=?1",
                [id],
                task_row,
            )?;

            transaction.commit()?;

            Ok(task)
        }).await
    }

    /// Enabled package declarations; execution still requires runtime integration.
    pub async fn project_plugins(&self, project_id: &str) -> Result<Vec<ProjectPlugin>> {
        let project_id = project_id.to_owned();

        self.access(move |conn| {
            let mut statement = conn.prepare(
                "SELECT project_id,marketplace_source,plugin_name,package_source,version,enabled_at,marketplace_revision
                 FROM project_plugins WHERE project_id=?1
                 ORDER BY enabled_at DESC, plugin_name, marketplace_source",
            )?;

            statement
                .query_map([project_id], project_plugin_row)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn enable_project_plugin(&self, plugin: ProjectPlugin) -> Result<ProjectPlugin> {
        validate_project_plugin(&plugin)?;

        self.access(move |conn| {
            let transaction = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let project_exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
                [&plugin.project_id],
                |row| row.get(0),
            )?;

            ensure!(project_exists, "project not found");

            transaction.execute(
                "INSERT INTO project_plugins(project_id,marketplace_source,plugin_name,package_source,version,marketplace_revision)
                 VALUES(?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(project_id,marketplace_source,plugin_name) DO UPDATE SET
                    package_source=excluded.package_source,
                    version=excluded.version,
                    marketplace_revision=excluded.marketplace_revision,
                    enabled_at=unixepoch()",
                params![
                    plugin.project_id,
                    plugin.marketplace_source,
                    plugin.plugin_name,
                    plugin.package_source,
                    plugin.version,
                    plugin.marketplace_revision,
                ],
            )?;
            let saved = transaction.query_row(
                "SELECT project_id,marketplace_source,plugin_name,package_source,version,enabled_at,marketplace_revision
                 FROM project_plugins WHERE project_id=?1 AND marketplace_source=?2 AND plugin_name=?3",
                params![plugin.project_id, plugin.marketplace_source, plugin.plugin_name],
                project_plugin_row,
            )?;

            transaction.commit()?;

            Ok(saved)
        })
        .await
    }

    /// Disable a package for a project without removing its central marketplace
    /// cache. Returning false means this package was not enabled.
    pub async fn disable_project_plugin(
        &self,
        project_id: &str,
        marketplace_source: &str,
        plugin_name: &str,
    ) -> Result<bool> {
        validate_project_plugin_key(project_id, marketplace_source, plugin_name)?;
        let project_id = project_id.to_owned();
        let marketplace_source = marketplace_source.to_owned();
        let plugin_name = plugin_name.to_owned();

        self.access(move |conn| {
            Ok(conn.execute(
                "DELETE FROM project_plugins WHERE project_id=?1 AND marketplace_source=?2 AND plugin_name=?3",
                params![project_id, marketplace_source, plugin_name],
            )? == 1)
        })
        .await
    }

    pub async fn session_memories(
        &self,
        session_id: &str,
        include_inactive: bool,
    ) -> Result<Vec<SessionMemory>> {
        let session_id = session_id.to_owned();

        self.access(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id,session_id,category,title,content,active,created_at,updated_at
                 FROM session_memories WHERE session_id=?1 AND (?2 OR active=1)
                 ORDER BY active DESC, updated_at DESC, id DESC",
            )?;

            statement
                .query_map(params![session_id, include_inactive], memory_row)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
        .await
    }

    /// Local lexical search: exact phrases, then terms, with newest records breaking ties.
    pub async fn search_session_memories(
        &self,
        session_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SessionMemory>> {
        let query = query.trim();
        ensure!(!query.is_empty(), "memory search query must not be empty");
        ensure!(query.len() <= 512, "memory search query exceeds 512 bytes");
        ensure!(
            (1..=20).contains(&limit),
            "memory search limit must be between 1 and 20"
        );

        let phrase = query.to_lowercase();
        let terms = phrase
            .split(|character: char| !character.is_alphanumeric())
            .filter(|term| term.chars().count() >= 2)
            .collect::<Vec<_>>();
        let mut matches = self
            .session_memories(session_id, false)
            .await?
            .into_iter()
            .filter_map(|memory| {
                let title = memory.title.to_lowercase();
                let content = memory.content.to_lowercase();
                let mut score = 0_u32;

                if title.contains(&phrase) {
                    score += 100;
                }

                if content.contains(&phrase) {
                    score += 60;
                }

                for term in &terms {
                    if title.contains(term) {
                        score += 10;
                    }

                    if content.contains(term) {
                        score += 5;
                    }
                }

                (score > 0).then_some((score, memory))
            })
            .collect::<Vec<_>>();

        matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| right.updated_at.cmp(&left.updated_at))
                .then_with(|| right.id.cmp(&left.id))
        });

        Ok(matches
            .into_iter()
            .take(limit)
            .map(|(_, memory)| memory)
            .collect())
    }

    pub async fn remember_in_session(
        &self,
        session_id: &str,
        category: MemoryCategory,
        title: String,
        content: String,
    ) -> Result<SessionMemory> {
        validate_memory_fields(&title, &content)?;
        let session_id = session_id.to_owned();

        self.access(move |conn| {
            let transaction = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let session_exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1)",
                [&session_id],
                |row| row.get(0),
            )?;

            ensure!(session_exists, "session not found");

            transaction.execute(
                "INSERT INTO session_memories(session_id,category,title,content) VALUES(?1,?2,?3,?4)",
                params![session_id, category.as_str(), title, content],
            )?;
            let id = transaction.last_insert_rowid();
            let memory = transaction.query_row(
                "SELECT id,session_id,category,title,content,active,created_at,updated_at FROM session_memories WHERE id=?1",
                [id],
                memory_row,
            )?;

            transaction.commit()?;

            Ok(memory)
        }).await
    }

    pub async fn set_session_memory_active(
        &self,
        session_id: &str,
        id: i64,
        active: bool,
    ) -> Result<SessionMemory> {
        let session_id = session_id.to_owned();

        self.access(move |conn| {
            let transaction = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let changed = transaction.execute(
                "UPDATE session_memories SET active=?3,updated_at=unixepoch() WHERE id=?1 AND session_id=?2",
                params![id, session_id, active],
            )?;
            ensure!(changed == 1, "memory not found in this session");

            let memory = transaction.query_row(
                "SELECT id,session_id,category,title,content,active,created_at,updated_at FROM session_memories WHERE id=?1",
                [id],
                memory_row,
            )?;

            transaction.commit()?;

            Ok(memory)
        }).await
    }
}

fn validate_task_fields(title: &str, details: &str, priority: i64) -> Result<()> {
    ensure!(!title.trim().is_empty(), "task title must not be empty");
    ensure!(title.len() <= 512, "task title exceeds 512 bytes");
    ensure!(details.len() <= 8192, "task details exceed 8192 bytes");
    ensure!(
        (-100..=100).contains(&priority),
        "task priority must be between -100 and 100"
    );

    Ok(())
}

fn validate_project_plugin(plugin: &ProjectPlugin) -> Result<()> {
    validate_project_plugin_key(
        &plugin.project_id,
        &plugin.marketplace_source,
        &plugin.plugin_name,
    )?;
    ensure!(
        !plugin.package_source.trim().is_empty(),
        "plugin package source must not be empty"
    );
    ensure!(
        plugin.package_source.len() <= 4096,
        "plugin package source exceeds 4096 bytes"
    );

    if let Some(version) = &plugin.version {
        ensure!(
            !version.trim().is_empty(),
            "plugin version must not be empty"
        );
        ensure!(version.len() <= 256, "plugin version exceeds 256 bytes");
    }

    if let Some(revision) = &plugin.marketplace_revision {
        ensure!(
            revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "marketplace revision must be a Git commit ID"
        );
    }

    Ok(())
}

fn validate_project_plugin_key(
    project_id: &str,
    marketplace_source: &str,
    plugin_name: &str,
) -> Result<()> {
    ensure!(
        !project_id.trim().is_empty(),
        "project id must not be empty"
    );
    ensure!(project_id.len() <= 256, "project id exceeds 256 bytes");
    ensure!(
        !marketplace_source.trim().is_empty(),
        "marketplace source must not be empty"
    );
    ensure!(
        marketplace_source.len() <= 4096,
        "marketplace source exceeds 4096 bytes"
    );
    ensure!(
        !plugin_name.trim().is_empty(),
        "plugin name must not be empty"
    );
    ensure!(plugin_name.len() <= 256, "plugin name exceeds 256 bytes");

    Ok(())
}

fn validate_memory_fields(title: &str, content: &str) -> Result<()> {
    ensure!(!title.trim().is_empty(), "memory title must not be empty");
    ensure!(title.len() <= 512, "memory title exceeds 512 bytes");
    ensure!(
        !content.trim().is_empty(),
        "memory content must not be empty"
    );
    ensure!(content.len() <= 8192, "memory content exceeds 8192 bytes");

    Ok(())
}

fn task_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectTask> {
    let status: String = row.get(5)?;

    Ok(ProjectTask {
        id: row.get(0)?,
        project_id: row.get(1)?,
        parent_id: row.get(2)?,
        title: row.get(3)?,
        details: row.get(4)?,
        status: TaskStatus::parse(&status).ok_or_else(|| rusqlite::Error::InvalidQuery)?,
        priority: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn project_plugin_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectPlugin> {
    Ok(ProjectPlugin {
        project_id: row.get(0)?,
        marketplace_source: row.get(1)?,
        plugin_name: row.get(2)?,
        package_source: row.get(3)?,
        version: row.get(4)?,
        enabled_at: row.get(5)?,
        marketplace_revision: row.get(6)?,
    })
}

fn memory_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionMemory> {
    let category: String = row.get(2)?;

    Ok(SessionMemory {
        id: row.get(0)?,
        session_id: row.get(1)?,
        category: MemoryCategory::parse(&category).ok_or_else(|| rusqlite::Error::InvalidQuery)?,
        title: row.get(3)?,
        content: row.get(4)?,
        active: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        workspace: row.get(1)?,
        model: row.get(2)?,
        reasoning_effort: row.get(3)?,
        demo: row.get(4)?,
        title: row.get(5)?,
        updated_at: row.get(6)?,
        project_id: row.get(7)?,
    })
}

fn ensure_real_directory_if_present(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_dir(),
                "private storage root is not a real directory: {}",
                path.display()
            );

            Ok(true)
        }

        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::Store;

    #[tokio::test]
    async fn a_long_history_reader_does_not_block_the_writer_connection() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("sessions.db"))
            .await
            .unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, proceed) = std::sync::mpsc::channel();
        let reader = {
            let store = store.clone();

            tokio::spawn(async move {
                store
                    .read_access(move |conn| {
                        let _: i64 = conn.query_row("SELECT 1", [], |row| row.get(0))?;
                        let _ = started.send(());
                        proceed.recv().unwrap();
                        Ok(())
                    })
                    .await
            })
        };

        ready.await.unwrap();

        let written = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            store.access(|conn| {
                conn.execute_batch("CREATE TABLE reader_concurrency(value INTEGER)")?;
                Ok(())
            }),
        )
        .await;

        release.send(()).unwrap();

        written.unwrap().unwrap();
        reader.await.unwrap().unwrap();
    }
}
