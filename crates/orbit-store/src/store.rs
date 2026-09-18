use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use orbit_agent::events::AgentEvent;
use orbit_model::types::ChatMessage;
use rusqlite::{Connection, params};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::types::{
    MemoryCategory, ProjectMemory, ProjectTask, Run, Session, Snapshot, TaskStatus, WorkspaceChange,
};

#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
    path: PathBuf,
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
            connection.busy_timeout(Duration::from_secs(3))?;
            connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
            connection.execute_batch("BEGIN IMMEDIATE;")?;
            let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            ensure!(
                version <= 7,
                "database schema {version} is newer than this Orbit supports"
            );
            if version == 0 {
                connection.execute_batch(
                    "CREATE TABLE IF NOT EXISTS sessions (
                    id TEXT PRIMARY KEY, workspace TEXT NOT NULL, model TEXT NOT NULL,
                    demo INTEGER NOT NULL, title TEXT NOT NULL DEFAULT '',
                    updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
                    history TEXT NOT NULL DEFAULT '[]'
                );
                CREATE TABLE IF NOT EXISTS runs (
                    id INTEGER PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
                    prompt TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'running'
                        CHECK(status IN ('running','completed','failed','cancelled','interrupted')),
                    elapsed_ms INTEGER
                );
                CREATE INDEX IF NOT EXISTS runs_session ON runs(session_id, id);
                CREATE TABLE IF NOT EXISTS events (
                    id INTEGER PRIMARY KEY, run_id INTEGER NOT NULL REFERENCES runs(id),
                    payload TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS events_run ON events(run_id, id);
                PRAGMA user_version = 1;",
                )?;
            }
            if version < 2 {
                crate::projects::migrate(&connection)?;
            }
            if version == 0 {
                connection.execute_batch("PRAGMA user_version=3;")?;
            } else if version < 3 {
                connection.execute_batch(
                    "ALTER TABLE runs ADD COLUMN elapsed_ms INTEGER;
                     PRAGMA user_version=3;",
                )?;
            }
            if version < 4 {
                connection.execute_batch(
                    "ALTER TABLE sessions ADD COLUMN reasoning_effort TEXT;
                     PRAGMA user_version=4;",
                )?;
            }
            if version < 5 {
                connection.execute_batch(
                    "CREATE TABLE project_tasks (
                        id INTEGER PRIMARY KEY,
                        project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                        parent_id INTEGER REFERENCES project_tasks(id) ON DELETE SET NULL,
                        title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 512),
                        details TEXT NOT NULL DEFAULT '' CHECK(length(details) <= 8192),
                        status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','in_progress','blocked','completed','cancelled')),
                        priority INTEGER NOT NULL DEFAULT 0 CHECK(priority BETWEEN -100 AND 100),
                        created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                        updated_at INTEGER NOT NULL DEFAULT (unixepoch())
                    );
                    CREATE INDEX project_tasks_project_status ON project_tasks(project_id,status,priority DESC,id);
                    CREATE TABLE project_memories (
                        id INTEGER PRIMARY KEY,
                        project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                        category TEXT NOT NULL CHECK(category IN ('constraint','decision','fact','preference')),
                        title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 512),
                        content TEXT NOT NULL CHECK(length(content) BETWEEN 1 AND 8192),
                        source_session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
                        active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
                        created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                        updated_at INTEGER NOT NULL DEFAULT (unixepoch())
                    );
                    CREATE INDEX project_memories_project_active ON project_memories(project_id,active,updated_at DESC,id DESC);
                    PRAGMA user_version=5;",
                )?;
            }
            if version < 6 {
                connection.execute_batch(
                    "ALTER TABLE events ADD COLUMN created_at_ms INTEGER;
                     PRAGMA user_version=6;",
                )?;
            }
            if version < 7 {
                connection.execute_batch(
                    "CREATE TABLE workspace_undos (
                        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                        run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
                        call_id TEXT NOT NULL,
                        path TEXT NOT NULL,
                        undone_at INTEGER NOT NULL DEFAULT (unixepoch()),
                        PRIMARY KEY(session_id,run_id,call_id,path)
                    );
                    CREATE INDEX workspace_undos_session ON workspace_undos(session_id,run_id);
                    PRAGMA user_version=7;",
                )?;
            }
            connection.execute_batch("COMMIT;")?;
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
        self.access(|conn| {
            let mut statement = conn.prepare("SELECT s.id,COALESCE(p.path,s.workspace),s.model,s.reasoning_effort,s.demo,s.title,s.updated_at,s.project_id FROM sessions s LEFT JOIN projects p ON p.id=s.project_id ORDER BY s.updated_at DESC, s.rowid DESC")?;
            Ok(statement.query_map([], session_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
        }).await
    }

    /// Read identity and display metadata without deserializing the event log.
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
        self.access(move |conn| {
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
                let payloads = events
                    .query_map([run_id], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                runs.push(Run {
                    prompt,
                    status,
                    elapsed_ms,
                    events: payloads
                        .iter()
                        .map(|p| serde_json::from_str(p))
                        .collect::<serde_json::Result<_>>()?,
                });
            }
            Ok(Snapshot {
                session,
                history: serde_json::from_str(&history)?,
                runs,
            })
        })
        .await
    }

    /// Return only actual successful `write_file` / `edit_file` commits. This
    /// reads the central event ledger rather than Git, so it remains useful in
    /// non-Git directories and does not mix in pre-existing user changes.
    pub async fn workspace_changes(&self, session_id: &str) -> Result<Vec<WorkspaceChange>> {
        let session_id = session_id.to_owned();
        self.access(move |conn| {
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
                 WHERE r.session_id=?1 ORDER BY r.id,e.id",
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

    /// Mark an exact write receipt as restored after `/undo` commits. This is
    /// kept separate from agent events because local commands are not model
    /// turns, while preserving the original append-only write audit.
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
            file.try_lock()
                .context("session is already open in another process")?;
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
            let transaction = conn.transaction()?;
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
            let changed = conn.execute(
                "UPDATE project_tasks
                 SET title=COALESCE(?3,title), details=COALESCE(?4,details),
                     status=COALESCE(?5,status), priority=COALESCE(?6,priority), updated_at=unixepoch()
                 WHERE id=?1 AND project_id=?2",
                params![id, project_id, title, details, status.map(TaskStatus::as_str), priority],
            )?;
            ensure!(changed == 1, "task not found in this project");
            conn.query_row(
                "SELECT id,project_id,parent_id,title,details,status,priority,created_at,updated_at FROM project_tasks WHERE id=?1",
                [id],
                task_row,
            ).map_err(Into::into)
        }).await
    }

    pub async fn memories(
        &self,
        project_id: &str,
        include_inactive: bool,
    ) -> Result<Vec<ProjectMemory>> {
        let project_id = project_id.to_owned();
        self.access(move |conn| {
            let mut statement = conn.prepare(
                "SELECT id,project_id,category,title,content,source_session_id,active,created_at,updated_at
                 FROM project_memories WHERE project_id=?1 AND (?2 OR active=1)
                 ORDER BY active DESC, updated_at DESC, id DESC",
            )?;
            statement
                .query_map(params![project_id, include_inactive], memory_row)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        }).await
    }

    /// Search active project memories locally. This deliberately uses a small,
    /// transparent lexical ranker instead of sending project facts to an
    /// embedding service. Exact phrase matches rank first, followed by
    /// individual query terms; ties preserve most recently updated records.
    pub async fn search_memories(
        &self,
        project_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ProjectMemory>> {
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
            .memories(project_id, false)
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

    pub async fn remember(
        &self,
        project_id: &str,
        category: MemoryCategory,
        title: String,
        content: String,
        source_session_id: Option<String>,
    ) -> Result<ProjectMemory> {
        validate_memory_fields(&title, &content)?;
        let project_id = project_id.to_owned();
        self.access(move |conn| {
            let transaction = conn.transaction()?;
            let project_exists: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
                [&project_id],
                |row| row.get(0),
            )?;
            ensure!(project_exists, "project not found");
            transaction.execute(
                "INSERT INTO project_memories(project_id,category,title,content,source_session_id) VALUES(?1,?2,?3,?4,?5)",
                params![project_id, category.as_str(), title, content, source_session_id],
            )?;
            let id = transaction.last_insert_rowid();
            let memory = transaction.query_row(
                "SELECT id,project_id,category,title,content,source_session_id,active,created_at,updated_at FROM project_memories WHERE id=?1",
                [id],
                memory_row,
            )?;
            transaction.commit()?;
            Ok(memory)
        }).await
    }

    pub async fn set_memory_active(
        &self,
        project_id: &str,
        id: i64,
        active: bool,
    ) -> Result<ProjectMemory> {
        let project_id = project_id.to_owned();
        self.access(move |conn| {
            let changed = conn.execute(
                "UPDATE project_memories SET active=?3,updated_at=unixepoch() WHERE id=?1 AND project_id=?2",
                params![id, project_id, active],
            )?;
            ensure!(changed == 1, "memory not found in this project");
            conn.query_row(
                "SELECT id,project_id,category,title,content,source_session_id,active,created_at,updated_at FROM project_memories WHERE id=?1",
                [id],
                memory_row,
            ).map_err(Into::into)
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

fn memory_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectMemory> {
    let category: String = row.get(2)?;
    Ok(ProjectMemory {
        id: row.get(0)?,
        project_id: row.get(1)?,
        category: MemoryCategory::parse(&category).ok_or_else(|| rusqlite::Error::InvalidQuery)?,
        title: row.get(3)?,
        content: row.get(4)?,
        source_session_id: row.get(5)?,
        active: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

impl SessionWriter {
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

    pub async fn begin(&mut self, prompt: String, model: String) -> Result<()> {
        ensure!(self.active_run.is_none(), "a run is already active");
        let id = self.id.clone();
        let run = self.store.access(move |conn| {
            let transaction = conn.transaction()?;
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
                let transaction = conn.transaction()?;
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

    pub async fn finish(
        &mut self,
        status: &str,
        events: Vec<AgentEvent>,
        history: Option<Vec<ChatMessage>>,
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
                let transaction = conn.transaction()?;
                for event in events {
                    transaction.execute(
                        "INSERT INTO events(run_id,payload,created_at_ms) VALUES(?1,?2,?3)",
                        params![run, serde_json::to_string(&event)?, now_millis()],
                    )?;
                }
                transaction.execute(
                    "UPDATE runs SET status=?2,elapsed_ms=?3 WHERE id=?1",
                    params![run, status, elapsed_ms],
                )?;
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
