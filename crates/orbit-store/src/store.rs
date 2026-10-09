use sea_orm::sea_query::Expr;
mod writer;

use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
};

use crate::{database::Database, entities::*};
use anyhow::{Context, Result, ensure};
use sea_orm::*;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use crate::types::{Run, Session, Snapshot};

#[derive(Clone)]
pub struct Store {
    connection: Database,
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
        let path = tokio::task::spawn_blocking(move || -> Result<PathBuf> {
            std::fs::create_dir_all(path.parent().context("database has no parent")?)?;
            private_file(&path)?;
            Ok(path.canonicalize()?)
        })
        .await??;
        let connection = Database::open(&path).await?;
        Ok(Self { connection, path })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) async fn write_access<T, F: std::future::Future<Output = Result<T>>>(
        &self,
        operation: impl FnOnce(DatabaseConnection) -> F,
    ) -> Result<T> {
        self.connection.write(operation).await
    }
    pub(crate) async fn read_access<T, F: std::future::Future<Output = Result<T>>>(
        &self,
        operation: impl FnOnce(DatabaseConnection) -> F,
    ) -> Result<T> {
        self.connection.read(operation).await
    }

    pub async fn create(&self, workspace: String, model: String) -> Result<SessionWriter> {
        let session = self.draft(workspace, model).await?;
        self.persist_draft(&session, &[]).await
    }

    /// Allocate a stable identity without inserting a conversation or acquiring a writer.
    pub async fn draft(&self, workspace: String, model: String) -> Result<Session> {
        let project = self.project_at(&workspace).await?;
        Ok(Session {
            id: crate::database::new_id()?,
            workspace,
            model,
            project_id: project.map(|p| p.id).unwrap_or_default(),
            title: String::new(),
            reasoning_effort: None,
            updated_at: crate::database::now(),
        })
    }

    /// Commit a draft's identity and selected profile when it first gains content.
    pub async fn persist_draft(
        &self,
        session: &Session,
        history: &[orbit_model::types::ChatMessage],
    ) -> Result<SessionWriter> {
        let history = serde_json::to_string(history)?;
        let mut session = session.clone();
        session.project_id = self.resolve_project(&session.workspace).await?.id;
        let id = session.id.clone();
        self.write_access(move |conn| async move {
            sessions::ActiveModel {
                id: Set(session.id),
                workspace: Set(session.workspace),
                model: Set(session.model),
                project_id: Set(Some(session.project_id)),
                title: Set(session.title),
                reasoning_effort: Set(session.reasoning_effort),
                history: Set(history),
                ..Default::default()
            }
            .insert(&conn)
            .await?;
            Ok(())
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
        self.read_access(move |conn| async move {
            let mut query = session_summaries().filter(
                sessions::Column::Id.not_in_subquery(
                    agent_sessions::Entity::find()
                        .select_only()
                        .column(agent_sessions::Column::SessionId)
                        .into_query(),
                ),
            );
            if started_only {
                query = query.filter(
                    sessions::Column::Id.in_subquery(
                        runs::Entity::find()
                            .select_only()
                            .column(runs::Column::SessionId)
                            .into_query(),
                    ),
                );
            }
            query
                .order_by_desc(sessions::Column::UpdatedAt)
                .order_by_desc(Expr::col((
                    sessions::Entity,
                    sea_query::Alias::new("rowid"),
                )))
                .into_model::<SessionSummary>()
                .all(&conn)
                .await?
                .into_iter()
                .map(SessionSummary::into_session)
                .collect()
        })
        .await
    }

    pub async fn session(&self, id: &str) -> Result<Session> {
        let id = id.to_owned();
        self.read_access(move |conn| async move {
            session_summaries()
                .filter(sessions::Column::Id.eq(id))
                .into_model::<SessionSummary>()
                .one(&conn)
                .await?
                .context("session not found")?
                .into_session()
        })
        .await
    }

    pub async fn load(&self, id: &str) -> Result<Snapshot> {
        let id = id.to_owned();

        self.read_access(move |conn| async move {
            let transaction = conn.begin().await?;
            let (row, project) = sessions::Entity::find_by_id(&id)
                .find_also_related(projects::Entity)
                .one(&transaction)
                .await?
                .context("session not found")?;
            let history = row.history.clone();
            let session = session_model(row, project)?;
            let rows = runs::Entity::find()
                .filter(runs::Column::SessionId.eq(&id))
                .order_by_asc(runs::Column::Id)
                .all(&transaction)
                .await?;
            let last_completed = rows
                .iter()
                .filter(|r| r.status == "completed")
                .map(|r| r.id)
                .max()
                .unwrap_or(0);
            let mut run_events = crate::event_log::load_by_session(&transaction, &id).await?;
            let mut first_messages = session_messages::Entity::find()
                .filter(session_messages::Column::SessionId.eq(&id))
                .filter(session_messages::Column::Ordinal.eq(0))
                .all(&transaction)
                .await?
                .into_iter()
                .map(|message| (message.run_id, message))
                .collect::<std::collections::HashMap<_, _>>();
            let mut runs = Vec::new();
            for row in rows {
                let run_id = row.id;
                let prompt = row.prompt;
                let status = row.status;
                let elapsed_ms = row.elapsed_ms.map(|ms| ms.max(0) as u64);
                let decoded = run_events
                    .remove(&run_id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|event| {
                        serde_json::from_str::<orbit_agent::events::AgentEvent>(&event.payload)
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                let message = first_messages
                    .remove(&run_id)
                    .map(|row| {
                        crate::session_log::decode(row.id, row.run_id, row.payload, row.sha256)
                    })
                    .transpose()?;
                let images = match message.map(|record| record.message) {
                    Some(orbit_model::types::ChatMessage::User {
                        content,
                        attachments,
                    }) if content == prompt => attachments
                        .into_iter()
                        .map(|attachment| match attachment {
                            orbit_model::types::Attachment::Image { name, path, .. } => {
                                orbit_protocol::MessageImage {
                                    name,
                                    path: path.to_string_lossy().into_owned(),
                                }
                            }
                        })
                        .collect(),
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

            let checkpoint = execution_checkpoints::Entity::find()
                .join(
                    JoinType::InnerJoin,
                    execution_checkpoints::Relation::Run.def(),
                )
                .filter(runs::Column::SessionId.eq(&id))
                .filter(runs::Column::Id.gt(last_completed))
                .filter(runs::Column::Status.ne("completed"))
                .order_by_desc(runs::Column::Id)
                .one(&transaction)
                .await?
                .map(|row| row.payload);
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

            transaction.commit().await?;
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
            ensure_real_directory_if_present(&sidecar_path.with_extension("command-output"))?;

            Ok(())
        })
        .await??;

        let session_id = id.clone();
        let delete_ids = session_ids.clone();
        let deleted = self
            .write_access(move |conn| async move {
                let transaction = conn.begin().await?;
                let exists = sessions::Entity::find_by_id(&session_id)
                    .count(&transaction)
                    .await?
                    > 0;
                if exists {
                    for id in delete_ids {
                        events::Entity::delete_many()
                            .filter(
                                events::Column::RunId.in_subquery(
                                    runs::Entity::find()
                                        .select_only()
                                        .column(runs::Column::Id)
                                        .filter(runs::Column::SessionId.eq(&id))
                                        .into_query(),
                                ),
                            )
                            .exec(&transaction)
                            .await?;
                        runs::Entity::delete_many()
                            .filter(runs::Column::SessionId.eq(&id))
                            .exec(&transaction)
                            .await?;
                        sessions::Entity::delete_by_id(id)
                            .exec(&transaction)
                            .await?;
                    }
                }
                transaction.commit().await?;
                Ok(exists)
            })
            .await?;

        let cleanup_path = self.path.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let output_root = cleanup_path.with_extension("command-output");
            let has_output = ensure_real_directory_if_present(&output_root)?;
            for id in session_ids {
                if has_output {
                    let directory = output_root.join(&id);
                    match std::fs::symlink_metadata(&directory) {
                        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
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
        self.connection.reclaim().await
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
        self.write_access(move |conn| async move {
            runs::Entity::update_many()
                .col_expr(runs::Column::Status, Expr::value("interrupted"))
                .filter(runs::Column::SessionId.eq(recovery_id))
                .filter(runs::Column::Status.eq("running"))
                .exec(&conn)
                .await?;
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
}

// Summary reads must not fetch the potentially large model-history projection.
#[derive(FromQueryResult)]
struct SessionSummary {
    id: String,
    workspace: String,
    model: String,
    reasoning_effort: Option<String>,
    title: String,
    updated_at: i64,
    project_id: Option<String>,
}
impl SessionSummary {
    fn into_session(self) -> Result<Session> {
        Ok(Session {
            id: self.id,
            workspace: self.workspace,
            model: self.model,
            reasoning_effort: self.reasoning_effort,
            title: self.title,
            updated_at: self.updated_at,
            project_id: self.project_id.context("session has no project")?,
        })
    }
}
fn session_summaries() -> Select<sessions::Entity> {
    sessions::Entity::find()
        .select_only()
        .columns([
            sessions::Column::Id,
            sessions::Column::Model,
            sessions::Column::ReasoningEffort,
            sessions::Column::Title,
            sessions::Column::UpdatedAt,
            sessions::Column::ProjectId,
        ])
        .column_as(
            Expr::expr(sea_query::Func::coalesce([
                Expr::col((projects::Entity, projects::Column::Path)).into(),
                Expr::col((sessions::Entity, sessions::Column::Workspace)).into(),
            ])),
            "workspace",
        )
        .join(JoinType::LeftJoin, sessions::Relation::Project.def())
}

fn session_model(row: sessions::Model, project: Option<projects::Model>) -> Result<Session> {
    Ok(Session {
        id: row.id,
        workspace: project.and_then(|p| p.path).unwrap_or(row.workspace),
        model: row.model,
        reasoning_effort: row.reasoning_effort,
        title: row.title,
        updated_at: row.updated_at,
        project_id: row.project_id.context("session has no project")?,
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

pub(crate) fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_long_history_reader_does_not_block_the_writer_connection() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("sessions.db"))
            .await
            .unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, proceed) = tokio::sync::oneshot::channel();
        let reader = {
            let store = store.clone();

            tokio::spawn(async move {
                store
                    .read_access(move |conn| async move {
                        let tx = conn.begin().await?;
                        sessions::Entity::find().all(&tx).await?;
                        let _ = started.send(());
                        proceed.await.unwrap();
                        tx.commit().await?;
                        Ok(())
                    })
                    .await
            })
        };

        ready.await.unwrap();

        let written = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            store.write_access(|conn| async move {
                projects::ActiveModel {
                    id: Set(crate::database::new_id()?),
                    ..Default::default()
                }
                .insert(&conn)
                .await?;
                Ok(())
            }),
        )
        .await;

        release.send(()).unwrap();

        written.unwrap().unwrap();
        reader.await.unwrap().unwrap();
    }
}
