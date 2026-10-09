//! Aggregate payload sizes in SQLite, without materializing large history bodies.
use crate::{
    entities::*,
    store::{SessionStorageUsage, StorageFootprint, Store},
};
use anyhow::Result;
use sea_orm::sea_query::Expr;
use sea_orm::{
    sea_query::{Alias, Func, IntoColumnRef, SimpleExpr},
    *,
};

fn bytes(column: impl IntoColumnRef) -> SimpleExpr {
    Func::cust(Alias::new("length"))
        .arg(Expr::col(column).cast_as(Alias::new("BLOB")))
        .into()
}

impl Store {
    pub async fn session_storage_usage(&self) -> Result<Vec<SessionStorageUsage>> {
        self.read_access(|conn| async move {
            let tx = conn.begin().await?;
            let rows: Vec<(String, String, i64)> = sessions::Entity::find()
                .select_only()
                .column(sessions::Column::Id)
                .column(sessions::Column::Title)
                .column_as(bytes(sessions::Column::History), "bytes")
                .into_tuple()
                .all(&tx)
                .await?;
            let mut usage: std::collections::HashMap<String, SessionStorageUsage> = rows
                .into_iter()
                .map(|(id, title, size)| {
                    (
                        id.clone(),
                        SessionStorageUsage {
                            id,
                            title,
                            payload_bytes: size.max(0) as u64,
                            attachment_bytes: 0,
                        },
                    )
                })
                .collect();
            let mut add = |rows: Vec<(String, i64)>| {
                for (id, bytes) in rows {
                    if let Some(row) = usage.get_mut(&id) {
                        row.payload_bytes += bytes.max(0) as u64;
                    }
                }
            };
            macro_rules! direct {
                ($entity:ident, $payload:ident) => {
                    add($entity::Entity::find()
                        .select_only()
                        .column($entity::Column::SessionId)
                        .column_as(Expr::expr(bytes($entity::Column::$payload)).sum(), "bytes")
                        .group_by($entity::Column::SessionId)
                        .into_tuple()
                        .all(&tx)
                        .await?);
                };
            }
            direct!(runs, Prompt);
            direct!(session_messages, Payload);
            direct!(compaction_progress, Payload);
            direct!(session_memories, Content);
            direct!(session_git_baselines, Payload);
            direct!(session_workspace_baselines, Payload);
            direct!(side_questions, Payload);
            direct!(user_inputs, Payload);
            macro_rules! related {
                ($entity:ident, $parent:ident, $relation:ident) => {
                    add($entity::Entity::find()
                        .select_only()
                        .column($parent::Column::SessionId)
                        .column_as(
                            Expr::expr(bytes(($entity::Entity, $entity::Column::Payload))).sum(),
                            "bytes",
                        )
                        .join(JoinType::InnerJoin, $entity::Relation::$relation.def())
                        .group_by($parent::Column::SessionId)
                        .into_tuple()
                        .all(&tx)
                        .await?);
                };
            }
            related!(events, runs, Run);
            related!(execution_checkpoints, runs, Run);
            related!(context_archive_messages, context_archives, Archive);
            let attachment_sizes: Vec<(String, i64)> = session_attachments::Entity::find()
                .select_only()
                .column(session_attachments::Column::SessionId)
                .column_as(
                    Expr::col(session_attachments::Column::ByteSize).sum(),
                    "bytes",
                )
                .group_by(session_attachments::Column::SessionId)
                .into_tuple()
                .all(&tx)
                .await?;
            for (id, bytes) in attachment_sizes {
                if let Some(row) = usage.get_mut(&id) {
                    row.attachment_bytes = bytes.max(0) as u64;
                }
            }
            let mut rows = usage.into_values().collect::<Vec<_>>();
            rows.sort_by(|a, b| {
                b.payload_bytes
                    .cmp(&a.payload_bytes)
                    .then_with(|| a.id.cmp(&b.id))
            });
            tx.commit().await?;
            Ok(rows)
        })
        .await
    }

    /// Physical files and logical payloads are deliberately separate. SQLite
    /// pages/indexes/free space cannot be apportioned accurately per session.
    pub async fn storage_footprint(&self) -> Result<StorageFootprint> {
        let database = self.path().to_owned();
        tokio::task::spawn_blocking(move || {
            let sidecar = |suffix: &str| {
                let mut path = database.as_os_str().to_os_string();
                path.push(suffix);
                std::path::PathBuf::from(path)
            };
            Ok(StorageFootprint {
                database_bytes: disk_bytes(&database)?,
                wal_bytes: disk_bytes(&sidecar("-wal"))?,
                shared_memory_bytes: disk_bytes(&sidecar("-shm"))?,
                attachment_bytes: disk_bytes(&database.with_extension("attachments"))?,
                old_command_output_bytes: disk_bytes(&database.with_extension("command-output"))?,
                temporary_command_output_bytes: disk_bytes(
                    &orbit_runtime::execution::output::CommandOutputs::root(),
                )?,
            })
        })
        .await?
    }
}

fn disk_bytes(path: &std::path::Path) -> Result<u64> {
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Ok(0);
    }
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    let mut bytes = 0u64;
    if metadata.is_dir() {
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            bytes = bytes.saturating_add(disk_bytes(&entry?.path())?);
        }
    }
    Ok(bytes)
}
