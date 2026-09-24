use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, ensure};
use orbit_model::types::{Attachment, ChatMessage, MAX_IMAGE_BYTES, image_mime};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};

use crate::session::{CanonicalMessage, Run, Session, Snapshot};

const SCHEMA_VERSION: i64 = 6;
const MAX_SESSION_OUTPUT_BYTES: i64 = 128 * 1024 * 1024;

#[derive(Debug)]
pub struct CommandOutputPage {
    pub content: Vec<u8>,
    pub offset: usize,
    pub retained_bytes: usize,
    pub total_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryPage {
    pub message_id: i64,
    pub sha256: String,
    pub content: String,
    pub offset: usize,
    pub total_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryMatch {
    pub message_id: i64,
    pub sha256: String,
    pub excerpt: String,
}

#[derive(Clone, Debug)]
pub struct CommandEvidence {
    pub command: String,
    pub input_snapshot_id: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug)]
pub struct VerificationRecord {
    pub output_id: String,
    pub command: String,
    pub input_snapshot_id: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub elapsed_ms: u64,
}

#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
    path: PathBuf,
}

/// The file lock prevents concurrent runs from writing to one session.
/// SQLite transactions protect each individual log update.
pub struct SessionWriter {
    store: Store,
    id: String,
    _lock: File,
    active_run: Option<i64>,
}

impl Store {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = std::path::absolute(path)?;
        let database = path.clone();
        let connection = tokio::task::spawn_blocking(move || -> Result<Connection> {
            let parent = database.parent().context("database path has no parent")?;
            std::fs::create_dir_all(parent)?;
            private_file(&database)?;

            let connection = Connection::open(&database)?;
            connection.busy_timeout(Duration::from_secs(3))?;
            let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            ensure!(
                (0..=SCHEMA_VERSION).contains(&version),
                "database schema {version} is incompatible with orbit-main schema {SCHEMA_VERSION}; select a different --database path"
            );
            if version == 0 {
                let tables: i64 = connection.query_row(
                    "SELECT count(*) FROM sqlite_master
                     WHERE type='table' AND name NOT LIKE 'sqlite_%'",
                    [],
                    |row| row.get(0),
                )?;
                ensure!(
                    tables == 0,
                    "database already contains an unrecognized schema; select a different --database path"
                );
            }
            connection.execute_batch(
                "PRAGMA foreign_keys = ON;
                 PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = FULL;",
            )?;
            if version == 0 {
                connection.execute_batch(
                    "BEGIN IMMEDIATE;
                     CREATE TABLE sessions (
                         id TEXT PRIMARY KEY,
                         workspace TEXT NOT NULL,
                         model TEXT NOT NULL,
                         title TEXT NOT NULL DEFAULT '',
                         created_at_ms INTEGER NOT NULL,
                         updated_at_ms INTEGER NOT NULL
                     );
                     CREATE INDEX sessions_workspace_updated
                         ON sessions(workspace, updated_at_ms DESC, id DESC);
                     CREATE TABLE runs (
                         id INTEGER PRIMARY KEY,
                         session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                         status TEXT NOT NULL CHECK(status IN ('running', 'completed', 'failed', 'cancelled', 'interrupted')),
                         started_at_ms INTEGER NOT NULL,
                         finished_at_ms INTEGER
                     );
                     CREATE INDEX runs_session ON runs(session_id, id);
                     CREATE TABLE messages (
                         id INTEGER PRIMARY KEY,
                         session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                         run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
                         payload TEXT NOT NULL,
                         sha256 TEXT NOT NULL
                     );
                     CREATE INDEX messages_session ON messages(session_id, id);
                     PRAGMA user_version = 1;
                     COMMIT;",
                )?;
            }

            if version < 2 {
                connection.execute_batch(
                    "BEGIN IMMEDIATE;
                     CREATE TABLE command_outputs (
                         id TEXT PRIMARY KEY,
                         session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                         created_at_ms INTEGER NOT NULL,
                         stdout BLOB NOT NULL,
                         stderr BLOB NOT NULL,
                         stdout_total_bytes INTEGER NOT NULL,
                         stderr_total_bytes INTEGER NOT NULL
                     );
                     CREATE INDEX command_outputs_session_created
                         ON command_outputs(session_id, created_at_ms, id);
                     PRAGMA user_version = 2;
                     COMMIT;",
                )?;
            }

            if version < 3 {
                connection.execute_batch(
                    "BEGIN IMMEDIATE;
                     CREATE TABLE command_evidence (
                         output_id TEXT PRIMARY KEY REFERENCES command_outputs(id) ON DELETE CASCADE,
                         command TEXT NOT NULL,
                         input_snapshot_id TEXT NOT NULL,
                         exit_code INTEGER,
                         timed_out INTEGER NOT NULL,
                         elapsed_ms INTEGER NOT NULL
                     );
                     PRAGMA user_version = 3;
                     COMMIT;",
                )?;
            }

            if version < 4 {
                connection.execute_batch(
                    "BEGIN IMMEDIATE;
                     CREATE TABLE context_checkpoints (
                         session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                         through_message INTEGER NOT NULL,
                         prefix_sha256 TEXT NOT NULL,
                         payload TEXT NOT NULL
                     );
                     CREATE INDEX messages_session_digest
                         ON messages(session_id, sha256);
                     PRAGMA user_version = 4;
                     COMMIT;",
                )?;
            }

            if version < 5 {
                connection.execute_batch(
                    "BEGIN IMMEDIATE;
                     CREATE TABLE project_tasks (
                         id INTEGER PRIMARY KEY,
                         workspace TEXT NOT NULL,
                         parent_id INTEGER REFERENCES project_tasks(id) ON DELETE SET NULL,
                         title TEXT NOT NULL,
                         details TEXT NOT NULL DEFAULT '',
                         status TEXT NOT NULL DEFAULT 'pending'
                             CHECK(status IN ('pending','in_progress','blocked','completed','cancelled')),
                         priority INTEGER NOT NULL DEFAULT 0,
                         created_at_ms INTEGER NOT NULL,
                         updated_at_ms INTEGER NOT NULL
                     );
                     CREATE INDEX project_tasks_workspace
                         ON project_tasks(workspace, status, priority DESC, id);
                     CREATE TABLE session_memories (
                         id INTEGER PRIMARY KEY,
                         session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                         category TEXT NOT NULL
                             CHECK(category IN ('constraint','decision','fact','preference')),
                         title TEXT NOT NULL,
                         content TEXT NOT NULL,
                         active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
                         created_at_ms INTEGER NOT NULL,
                         updated_at_ms INTEGER NOT NULL
                     );
                     CREATE INDEX session_memories_session
                         ON session_memories(session_id, active, updated_at_ms DESC, id DESC);
                     PRAGMA user_version = 5;
                     COMMIT;",
                )?;
            }

            if version < 6 {
                connection.execute_batch(
                    "BEGIN IMMEDIATE;
                     ALTER TABLE sessions ADD COLUMN reasoning_effort TEXT;
                     PRAGMA user_version = 6;
                     COMMIT;",
                )?;
            }

            Ok(connection)
        })
        .await??;

        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            path,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn save_image(&self, session_id: &str, source: &Path) -> Result<Attachment> {
        let source = source.to_path_buf();
        let name = source
            .file_name()
            .and_then(|value| value.to_str())
            .context("image filename is not valid UTF-8")?
            .to_owned();
        let bytes = tokio::task::spawn_blocking(move || {
            let metadata = std::fs::metadata(&source)
                .with_context(|| format!("failed to inspect image {}", source.display()))?;
            ensure!(metadata.is_file(), "image path is not a regular file");
            ensure!(
                metadata.len() <= MAX_IMAGE_BYTES as u64,
                "image exceeds 20 MiB"
            );
            std::fs::read(&source)
                .with_context(|| format!("failed to read image {}", source.display()))
        })
        .await??;
        self.save_image_bytes(session_id, name, bytes).await
    }

    pub async fn save_image_bytes(
        &self,
        session_id: &str,
        name: String,
        bytes: Vec<u8>,
    ) -> Result<Attachment> {
        validate_session_id(session_id)?;
        self.session(session_id).await?;
        ensure!(!name.trim().is_empty(), "image name must be nonempty");
        ensure!(bytes.len() <= MAX_IMAGE_BYTES, "image exceeds 20 MiB");

        let attachment_root = self.path.with_extension("attachments");
        let root = attachment_root.join(session_id);
        tokio::task::spawn_blocking(move || {
            let mime_type = image_mime(&bytes).context("unsupported image format")?;
            let format = match mime_type {
                "image/png" => image::ImageFormat::Png,
                "image/jpeg" => image::ImageFormat::Jpeg,
                "image/gif" => image::ImageFormat::Gif,
                "image/webp" => image::ImageFormat::WebP,
                _ => unreachable!("image_mime returns only supported formats"),
            };
            let mut reader = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format);
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(8192);
            limits.max_image_height = Some(8192);
            limits.max_alloc = Some(128 * 1024 * 1024);
            reader.limits(limits);
            reader.decode().context("image could not be decoded")?;
            let sha256 = Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            if !attachment_root.exists() {
                std::fs::create_dir(&attachment_root)?;
            }
            let attachment_root_metadata = std::fs::symlink_metadata(&attachment_root)?;
            ensure!(
                attachment_root_metadata.file_type().is_dir(),
                "attachment root is not safe"
            );
            if !root.exists() {
                std::fs::create_dir(&root)?;
            }
            let root_metadata = std::fs::symlink_metadata(&root)?;
            ensure!(
                root_metadata.file_type().is_dir(),
                "attachment directory is not safe"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&attachment_root, std::fs::Permissions::from_mode(0o700))?;
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
            }
            let path = root.join(format!("{sha256}.img"));
            if !path.exists() {
                let mut temporary = tempfile::NamedTempFile::new_in(&root)?;
                temporary.write_all(&bytes)?;
                temporary.as_file().sync_all()?;
                match temporary.persist_noclobber(&path) {
                    Ok(_) => {}
                    Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error.error.into()),
                }
                File::open(&root)?.sync_all()?;
            }

            let attachment = Attachment::Image {
                name,
                mime_type: mime_type.to_owned(),
                path,
                sha256,
            };
            attachment.image()?;
            Ok(attachment)
        })
        .await?
    }

    pub(crate) async fn access<T, F>(&self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    {
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = connection
                .lock()
                .map_err(|_| anyhow::anyhow!("database lock poisoned"))?;
            operation(&mut guard)
        })
        .await?
    }

    pub async fn create(&self, workspace: &Path, model: &str) -> Result<SessionWriter> {
        let workspace = workspace.canonicalize()?.to_string_lossy().into_owned();
        let model = model.to_owned();
        let id = self
            .access(move |connection| {
                let transaction = connection.transaction()?;
                let id: String =
                    transaction
                        .query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))?;
                let now = now_millis();
                transaction.execute(
                    "INSERT INTO sessions(id, workspace, model, created_at_ms, updated_at_ms)
                     VALUES(?1, ?2, ?3, ?4, ?4)",
                    params![id, workspace, model, now],
                )?;
                transaction.commit()?;
                Ok(id)
            })
            .await?;

        self.acquire(&id).await
    }

    pub async fn acquire(&self, id: &str) -> Result<SessionWriter> {
        validate_session_id(id)?;
        self.session(id).await?;

        let lock_dir = self.path.with_extension("session-locks");
        let lock_id = id.to_owned();
        let lock = tokio::task::spawn_blocking(move || -> Result<File> {
            std::fs::create_dir_all(&lock_dir)?;
            let file = private_file(&lock_dir.join(lock_id))?;
            file.try_lock().map_err(|error| match error {
                std::fs::TryLockError::WouldBlock => {
                    anyhow::anyhow!("session is already open in another Orbit process")
                }
                std::fs::TryLockError::Error(error) => anyhow::Error::new(error),
            })?;
            Ok(file)
        })
        .await??;

        let session_id = id.to_owned();
        self.access(move |connection| {
            connection.execute(
                "UPDATE runs SET status='interrupted', finished_at_ms=?2
                 WHERE session_id=?1 AND status='running'",
                params![session_id, now_millis()],
            )?;
            Ok(())
        })
        .await?;

        Ok(SessionWriter {
            store: self.clone(),
            id: id.to_owned(),
            _lock: lock,
            active_run: None,
        })
    }

    pub async fn session(&self, id: &str) -> Result<Session> {
        validate_session_id(id)?;
        let id = id.to_owned();
        self.access(move |connection| {
            connection
                .query_row(
                    "SELECT id, workspace, model, title, created_at_ms, updated_at_ms, reasoning_effort
                     FROM sessions WHERE id=?1",
                    [id],
                    session_row,
                )
                .optional()?
                .context("session not found")
        })
        .await
    }

    pub async fn rename_session(&self, id: &str, title: &str) -> Result<()> {
        validate_session_id(id)?;
        let title = title.trim();
        ensure!(!title.is_empty(), "session title must not be empty");
        ensure!(
            title.chars().count() <= 120,
            "session title must be at most 120 characters"
        );
        let id = id.to_owned();
        let title = title.to_owned();
        self.access(move |connection| {
            let changed = connection.execute(
                "UPDATE sessions SET title=?2, updated_at_ms=?3 WHERE id=?1",
                params![id, title, now_millis()],
            )?;
            ensure!(changed == 1, "session not found");
            Ok(())
        })
        .await
    }

    pub async fn set_session_model(&self, id: &str, model: &str) -> Result<()> {
        self.set_session_profile(id, model, None).await
    }

    pub async fn set_session_profile(
        &self,
        id: &str,
        model: &str,
        reasoning_effort: Option<&str>,
    ) -> Result<()> {
        validate_session_id(id)?;
        let model = model.trim();
        ensure!(!model.is_empty(), "session model must not be empty");
        let id = id.to_owned();
        let model = model.to_owned();
        let reasoning_effort = reasoning_effort.map(str::to_owned);
        self.access(move |connection| {
            let changed = connection.execute(
                "UPDATE sessions SET model=?2, reasoning_effort=?3, updated_at_ms=?4 WHERE id=?1",
                params![id, model, reasoning_effort, now_millis()],
            )?;
            ensure!(changed == 1, "session not found");
            Ok(())
        })
        .await
    }

    pub async fn list(&self, workspace: &Path) -> Result<Vec<Session>> {
        let workspace = workspace.canonicalize()?.to_string_lossy().into_owned();
        self.access(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id, workspace, model, title, created_at_ms, updated_at_ms, reasoning_effort
                 FROM sessions WHERE workspace=?1 ORDER BY updated_at_ms DESC, id DESC",
            )?;
            let sessions = statement
                .query_map([workspace], session_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(sessions)
        })
        .await
    }

    pub async fn load(&self, id: &str) -> Result<Snapshot> {
        validate_session_id(id)?;
        let id = id.to_owned();
        self.access(move |connection| {
            let session = connection
                .query_row(
                    "SELECT id, workspace, model, title, created_at_ms, updated_at_ms, reasoning_effort
                     FROM sessions WHERE id=?1",
                    [&id],
                    session_row,
                )
                .optional()?
                .context("session not found")?;
            let mut statement = connection.prepare(
                "SELECT id, run_id, payload, sha256 FROM messages
                 WHERE session_id=?1 ORDER BY id",
            )?;
            let messages = statement
                .query_map([&id], canonical_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .map(decode_message)
                .collect::<Result<Vec<_>>>()?;
            let mut statement = connection.prepare(
                "SELECT id, status, started_at_ms, finished_at_ms FROM runs
                 WHERE session_id=?1 ORDER BY id",
            )?;
            let runs = statement
                .query_map([&id], run_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;

            Ok(Snapshot {
                session,
                runs,
                messages,
            })
        })
        .await
    }

    /// Save bounded command evidence under the canonical session ID.
    /// Oldest command results are evicted once the per-session quota is reached.
    pub async fn save_command_output(
        &self,
        session_id: &str,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        stdout_total_bytes: usize,
        stderr_total_bytes: usize,
        evidence: Option<CommandEvidence>,
    ) -> Result<String> {
        validate_session_id(session_id)?;
        ensure!(
            stdout.len() <= 16 * 1024 * 1024 && stderr.len() <= 16 * 1024 * 1024,
            "command output exceeds retention limit"
        );
        if let Some(evidence) = &evidence {
            ensure!(
                evidence.command.len() <= 16 * 1024,
                "command evidence exceeds 16 KiB"
            );
            ensure!(
                evidence.input_snapshot_id.len() == 64
                    && evidence
                        .input_snapshot_id
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit()),
                "invalid workspace fingerprint"
            );
        }
        let session_id = session_id.to_owned();
        self.access(move |connection| {
            let transaction = connection.transaction()?;
            let id: String =
                transaction.query_row("SELECT lower(hex(randomblob(16)))", [], |row| row.get(0))?;
            transaction.execute(
                "INSERT INTO command_outputs (
                    id, session_id, created_at_ms, stdout, stderr,
                    stdout_total_bytes, stderr_total_bytes
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    id,
                    session_id,
                    now_millis(),
                    stdout,
                    stderr,
                    stdout_total_bytes as i64,
                    stderr_total_bytes as i64
                ],
            )?;

            if let Some(evidence) = evidence {
                transaction.execute(
                    "INSERT INTO command_evidence (
                        output_id, command, input_snapshot_id, exit_code, timed_out, elapsed_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        id,
                        evidence.command,
                        evidence.input_snapshot_id,
                        evidence.exit_code,
                        evidence.timed_out,
                        evidence.elapsed_ms as i64
                    ],
                )?;
            }

            loop {
                let bytes: i64 = transaction.query_row(
                    "SELECT COALESCE(SUM(length(stdout) + length(stderr)), 0)
                     FROM command_outputs WHERE session_id=?1",
                    [&session_id],
                    |row| row.get(0),
                )?;
                if bytes <= MAX_SESSION_OUTPUT_BYTES {
                    break;
                }
                transaction.execute(
                    "DELETE FROM command_outputs WHERE id = (
                        SELECT id FROM command_outputs WHERE session_id=?1
                        ORDER BY created_at_ms, rowid LIMIT 1
                    )",
                    [&session_id],
                )?;
            }

            transaction.commit()?;
            Ok(id)
        })
        .await
    }

    pub async fn verification_records(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<VerificationRecord>> {
        validate_session_id(session_id)?;
        ensure!(
            (1..=100).contains(&limit),
            "verification limit must be 1–100"
        );
        let session_id = session_id.to_owned();
        self.access(move |connection| {
            let mut statement = connection.prepare(
                "SELECT o.id, e.command, e.input_snapshot_id, e.exit_code,
                        e.timed_out, e.elapsed_ms
                 FROM command_outputs AS o
                 JOIN command_evidence AS e ON e.output_id = o.id
                 WHERE o.session_id=?1 ORDER BY o.created_at_ms DESC, o.rowid DESC
                 LIMIT ?2",
            )?;
            statement
                .query_map(params![session_id, limit as i64], |row| {
                    Ok(VerificationRecord {
                        output_id: row.get(0)?,
                        command: row.get(1)?,
                        input_snapshot_id: row.get(2)?,
                        exit_code: row.get(3)?,
                        timed_out: row.get(4)?,
                        elapsed_ms: row.get::<_, i64>(5)? as u64,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn read_command_output(
        &self,
        session_id: &str,
        output_id: &str,
        stream: &str,
        offset: usize,
        limit: usize,
    ) -> Result<CommandOutputPage> {
        validate_session_id(session_id)?;
        validate_session_id(output_id)?;
        ensure!(
            matches!(stream, "stdout" | "stderr"),
            "invalid output stream"
        );
        ensure!(
            (1..=8192).contains(&limit),
            "output page limit must be 1–8192"
        );
        ensure!(offset <= 16 * 1024 * 1024, "output offset is out of range");
        let column = stream.to_owned();
        let session_id = session_id.to_owned();
        let output_id = output_id.to_owned();
        self.access(move |connection| {
            let sql = format!(
                "SELECT substr({column}, ?3, ?4), length({column}), {column}_total_bytes
                 FROM command_outputs WHERE id=?1 AND session_id=?2"
            );
            let (content, retained_bytes, total_bytes): (Vec<u8>, i64, i64) = connection
                .query_row(
                    &sql,
                    params![output_id, session_id, offset as i64 + 1, limit as i64],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?
                .context("command output not found in this session")?;
            Ok(CommandOutputPage {
                content,
                offset,
                retained_bytes: retained_bytes as usize,
                total_bytes: total_bytes as usize,
            })
        })
        .await
    }

    pub async fn save_context_checkpoint(
        &self,
        session_id: &str,
        through_message: usize,
        prefix_sha256: &str,
        payload: &str,
    ) -> Result<()> {
        validate_session_id(session_id)?;
        ensure!(
            through_message > 0,
            "checkpoint must cover at least one message"
        );
        ensure!(
            prefix_sha256.len() == 64 && prefix_sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid checkpoint fingerprint"
        );
        ensure!(payload.len() <= 64 * 1024, "checkpoint exceeds size limit");
        let session_id = session_id.to_owned();
        let prefix_sha256 = prefix_sha256.to_owned();
        let payload = payload.to_owned();
        self.access(move |connection| {
            connection.execute(
                "INSERT INTO context_checkpoints(session_id, through_message, prefix_sha256, payload)
                 VALUES(?1, ?2, ?3, ?4)
                 ON CONFLICT(session_id) DO UPDATE SET
                     through_message=excluded.through_message,
                     prefix_sha256=excluded.prefix_sha256,
                     payload=excluded.payload",
                params![session_id, through_message as i64, prefix_sha256, payload],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn load_context_checkpoint(&self, session_id: &str) -> Result<Option<String>> {
        validate_session_id(session_id)?;
        let session_id = session_id.to_owned();
        self.access(move |connection| {
            connection
                .query_row(
                    "SELECT payload FROM context_checkpoints WHERE session_id=?1",
                    [session_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(Into::into)
        })
        .await
    }

    pub async fn read_history(
        &self,
        session_id: &str,
        sha256: &str,
        offset: usize,
        limit: usize,
    ) -> Result<HistoryPage> {
        validate_session_id(session_id)?;
        ensure!(
            sha256.len() == 64 && sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "invalid history digest"
        );
        ensure!(
            (1..=8192).contains(&limit),
            "history limit must be 1–8192 bytes"
        );
        let session_id = session_id.to_owned();
        let sha256 = sha256.to_owned();
        self.access(move |connection| {
            let record = connection
                .query_row(
                    "SELECT id, run_id, payload, sha256 FROM messages
                     WHERE session_id=?1 AND sha256=?2 ORDER BY id LIMIT 1",
                    params![session_id, sha256],
                    canonical_row,
                )
                .optional()?
                .context("history message not found in this session")?;
            let message_id = record.0;
            let digest = record.3.clone();
            let payload = serde_json::to_string(&decode_message(record)?.message)?;
            ensure!(
                offset <= payload.len(),
                "history offset exceeds message length"
            );
            ensure!(
                payload.is_char_boundary(offset),
                "history offset is not a UTF-8 boundary"
            );
            let mut end = offset.saturating_add(limit).min(payload.len());
            while !payload.is_char_boundary(end) {
                end -= 1;
            }
            Ok(HistoryPage {
                message_id,
                sha256: digest,
                content: payload[offset..end].to_owned(),
                offset,
                total_bytes: payload.len(),
            })
        })
        .await
    }

    pub async fn search_history(
        &self,
        session_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<HistoryMatch>> {
        validate_session_id(session_id)?;
        ensure!(
            !query.trim().is_empty() && query.len() <= 200,
            "history query must be 1–200 bytes"
        );
        ensure!(
            (1..=20).contains(&limit),
            "history search limit must be 1–20"
        );
        let escaped = query
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let query = query.to_owned();
        let session_id = session_id.to_owned();
        self.access(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id, run_id, payload, sha256 FROM messages
                 WHERE session_id=?1 AND payload LIKE ?2 ESCAPE '\\' ORDER BY id DESC LIMIT ?3",
            )?;
            let records = statement
                .query_map(params![session_id, pattern, limit as i64], canonical_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            records
                .into_iter()
                .map(|record| {
                    let message_id = record.0;
                    let sha256 = record.3.clone();
                    let payload = record.2.clone();
                    decode_message(record)?;
                    let position = payload.find(&query).unwrap_or(0);
                    let mut start = position.saturating_sub(100);
                    while !payload.is_char_boundary(start) {
                        start -= 1;
                    }
                    let mut end = position
                        .saturating_add(query.len() + 180)
                        .min(payload.len());
                    while !payload.is_char_boundary(end) {
                        end -= 1;
                    }
                    Ok(HistoryMatch {
                        message_id,
                        sha256,
                        excerpt: payload[start..end].to_owned(),
                    })
                })
                .collect()
        })
        .await
    }

    pub async fn canonical_messages(
        &self,
        id: &str,
        after_id: Option<i64>,
        limit: usize,
    ) -> Result<Vec<CanonicalMessage>> {
        ensure!((1..=1000).contains(&limit), "message limit must be 1–1000");
        validate_session_id(id)?;
        let id = id.to_owned();
        self.access(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id, run_id, payload, sha256 FROM messages
                 WHERE session_id=?1 AND id>?2 ORDER BY id LIMIT ?3",
            )?;
            statement
                .query_map(
                    params![id, after_id.unwrap_or(0), limit as i64],
                    canonical_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .map(decode_message)
                .collect()
        })
        .await
    }
}

impl SessionWriter {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub async fn begin(&mut self, prompt: String, model: String) -> Result<()> {
        self.begin_user(ChatMessage::user(prompt), model).await
    }

    pub async fn begin_user(&mut self, user: ChatMessage, model: String) -> Result<()> {
        ensure!(self.active_run.is_none(), "a run is already active");
        let ChatMessage::User { content, .. } = &user else {
            anyhow::bail!("run must begin with a user message");
        };
        let title: String = content.chars().take(80).collect();
        let id = self.id.clone();
        let run = self
            .store
            .access(move |connection| {
                let transaction = connection.transaction()?;
                transaction.execute(
                    "INSERT INTO runs(session_id, status, started_at_ms)
                     VALUES(?1, 'running', ?2)",
                    params![id, now_millis()],
                )?;
                let run = transaction.last_insert_rowid();
                append_message(&transaction, &id, run, &user)?;
                transaction.execute(
                    "UPDATE sessions SET model=?2, updated_at_ms=?3,
                     title=CASE WHEN title='' THEN ?4 ELSE title END WHERE id=?1",
                    params![id, model, now_millis(), title],
                )?;
                transaction.commit()?;
                Ok(run)
            })
            .await?;
        self.active_run = Some(run);
        Ok(())
    }

    pub async fn append(&self, message: ChatMessage) -> Result<()> {
        let run = self.active_run.context("no active run")?;
        let id = self.id.clone();
        self.store
            .access(move |connection| {
                let transaction = connection.transaction()?;
                let status: String = transaction.query_row(
                    "SELECT status FROM runs WHERE id=?1 AND session_id=?2",
                    params![run, id],
                    |row| row.get(0),
                )?;
                ensure!(status == "running", "run is no longer active");
                append_message(&transaction, &id, run, &message)?;
                transaction.execute(
                    "UPDATE sessions SET updated_at_ms=?2 WHERE id=?1",
                    params![id, now_millis()],
                )?;
                transaction.commit()?;
                Ok(())
            })
            .await
    }

    pub async fn finish(&mut self, status: &str) -> Result<()> {
        ensure!(
            matches!(status, "completed" | "failed" | "cancelled"),
            "invalid terminal status"
        );
        let run = self.active_run.context("no active run")?;
        let id = self.id.clone();
        let status = status.to_owned();
        self.store
            .access(move |connection| {
                let transaction = connection.transaction()?;
                let updated = transaction.execute(
                    "UPDATE runs SET status=?3, finished_at_ms=?4
                     WHERE id=?1 AND session_id=?2 AND status='running'",
                    params![run, id, status, now_millis()],
                )?;
                ensure!(updated == 1, "run is no longer active");
                transaction.commit()?;
                Ok(())
            })
            .await?;
        self.active_run = None;
        Ok(())
    }
}

fn append_message(
    transaction: &Transaction<'_>,
    session_id: &str,
    run_id: i64,
    message: &ChatMessage,
) -> Result<()> {
    let payload = serde_json::to_string(message)?;
    transaction.execute(
        "INSERT INTO messages(session_id, run_id, payload, sha256) VALUES(?1, ?2, ?3, ?4)",
        params![session_id, run_id, payload, digest(&payload)],
    )?;
    Ok(())
}

fn canonical_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, i64, String, String)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn decode_message(record: (i64, i64, String, String)) -> Result<CanonicalMessage> {
    let (id, run_id, payload, expected) = record;
    ensure!(
        digest(&payload) == expected,
        "canonical message {id} failed integrity check"
    );
    Ok(CanonicalMessage {
        id,
        run_id,
        message: serde_json::from_str(&payload)?,
    })
}

fn digest(payload: &str) -> String {
    Sha256::digest(payload.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        workspace: row.get(1)?,
        model: row.get(2)?,
        title: row.get(3)?,
        created_at_ms: row.get(4)?,
        updated_at_ms: row.get(5)?,
        reasoning_effort: row.get(6)?,
    })
}

fn run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Run> {
    Ok(Run {
        id: row.get(0)?,
        status: row.get(1)?,
        started_at_ms: row.get(2)?,
        finished_at_ms: row.get(3)?,
    })
}

fn validate_session_id(id: &str) -> Result<()> {
    ensure!(
        id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "invalid session ID"
    );
    Ok(())
}

fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

pub(crate) fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
