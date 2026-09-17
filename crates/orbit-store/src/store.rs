use std::{
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

use crate::types::{Run, Session, Snapshot};

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
                version <= 2,
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
                        CHECK(status IN ('running','completed','failed','cancelled','interrupted'))
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
            let mut statement = conn.prepare("SELECT s.id,COALESCE(p.path,s.workspace),s.model,s.demo,s.title,s.updated_at,s.project_id FROM sessions s LEFT JOIN projects p ON p.id=s.project_id ORDER BY s.updated_at DESC, s.rowid DESC")?;
            Ok(statement.query_map([], session_row)?.collect::<rusqlite::Result<Vec<_>>>()?)
        }).await
    }

    /// Read identity and display metadata without deserializing the event log.
    pub async fn session(&self, id: &str) -> Result<Session> {
        let id = id.to_owned();
        self.access(move |conn| {
            conn.query_row(
                "SELECT s.id,COALESCE(p.path,s.workspace),s.model,s.demo,s.title,s.updated_at,s.project_id FROM sessions s LEFT JOIN projects p ON p.id=s.project_id WHERE s.id=?1",
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
                    "SELECT s.id,COALESCE(p.path,s.workspace),s.model,s.demo,s.title,s.updated_at,s.project_id FROM sessions s LEFT JOIN projects p ON p.id=s.project_id WHERE s.id=?1",
                    [&id],
                    session_row,
                )
                .context("session not found")?;
            let history: String =
                transaction.query_row("SELECT history FROM sessions WHERE id=?1", [&id], |r| {
                    r.get(0)
                })?;
            let mut statement = transaction
                .prepare("SELECT id,prompt,status FROM runs WHERE session_id=?1 ORDER BY id")?;
            let rows = statement
                .query_map([&id], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut runs = Vec::new();
            for (run_id, prompt, status) in rows {
                let mut events = transaction
                    .prepare("SELECT payload FROM events WHERE run_id=?1 ORDER BY id")?;
                let payloads = events
                    .query_map([run_id], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                runs.push(Run {
                    prompt,
                    status,
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
        })
    }
}

impl SessionWriter {
    pub fn id(&self) -> &str {
        &self.id
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
        Ok(())
    }

    pub async fn append(&self, events: Vec<AgentEvent>) -> Result<()> {
        let run = self.active_run.context("no active run")?;
        self.store
            .access(move |conn| {
                let transaction = conn.transaction()?;
                for event in events {
                    transaction.execute(
                        "INSERT INTO events(run_id,payload) VALUES(?1,?2)",
                        params![run, serde_json::to_string(&event)?],
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
        let status = status.to_owned();
        let id = self.id.clone();
        self.store
            .access(move |conn| {
                let transaction = conn.transaction()?;
                for event in events {
                    transaction.execute(
                        "INSERT INTO events(run_id,payload) VALUES(?1,?2)",
                        params![run, serde_json::to_string(&event)?],
                    )?;
                }
                transaction.execute(
                    "UPDATE runs SET status=?2 WHERE id=?1",
                    params![run, status],
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
        Ok(())
    }
}

fn session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        workspace: row.get(1)?,
        model: row.get(2)?,
        demo: row.get(3)?,
        title: row.get(4)?,
        updated_at: row.get(5)?,
        project_id: row.get(6)?,
    })
}

pub(crate) fn private_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}
