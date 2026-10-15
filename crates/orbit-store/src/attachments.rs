//! Session ownership is additive because the canonical log is append-only.
//! Cascading session deletion releases references; physical GC respects leases.
use crate::{entities::*, store::Store};
use anyhow::Result;
use orbit_model::types::{Attachment, ChatMessage};
use orbit_runtime::assets::AssetDirectory;
use sea_orm::*;
use std::{collections::HashSet, path::Path};

pub(crate) fn register<'a, 'image>(
    tx: &'a DatabaseTransaction,
    database: &Path,
    session: &str,
    attachments: impl IntoIterator<Item = &'image Attachment>,
) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
    let directory = AssetDirectory::for_database(database);
    let mut names = HashSet::new();
    let mut rows = Vec::new();
    for attachment in attachments {
        if attachment.is_temporary() {
            continue;
        }
        let Some(file_name) = directory.file_name(attachment.path()) else {
            continue;
        };
        if !names.insert(file_name.clone()) {
            continue;
        }
        let byte_size = std::fs::symlink_metadata(attachment.path())
            .ok()
            .filter(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
            .map_or(0, |metadata| metadata.len().min(i64::MAX as u64) as i64);
        rows.push(session_attachments::ActiveModel {
            session_id: Set(session.to_owned()),
            content_hash: Set(file_name
                .split_once('.')
                .unwrap()
                .0
                .to_owned()),
            file_name: Set(file_name),
            byte_size: Set(byte_size),
            ..Default::default()
        });
    }
    async move {
        for row in rows {
            session_attachments::Entity::insert(row)
                .on_conflict(
                    sea_query::OnConflict::columns([
                        session_attachments::Column::SessionId,
                        session_attachments::Column::FileName,
                    ])
                    .update_column(session_attachments::Column::ByteSize)
                    .to_owned(),
                )
                .exec_without_returning(tx)
                .await?;
        }
        Ok(())
    }
}

pub(crate) fn in_messages(messages: &[ChatMessage]) -> impl Iterator<Item = &Attachment> {
    messages
        .iter()
        .flat_map(|message| match message {
            ChatMessage::User { attachments, .. } => attachments.as_slice(),
            _ => &[],
        })
}

impl Store {
    /// Authorization survives presentation-cache eviction and lazy replay.
    pub async fn session_has_attachment(&self, session: &str, path: &str) -> Result<bool> {
        let Some(name) = AssetDirectory::for_database(self.path()).file_name(Path::new(path))
        else {
            return Ok(false);
        };
        let session = session.to_owned();
        self.read_access(move |conn| async move {
            Ok(session_attachments::Entity::find()
                .filter(session_attachments::Column::SessionId.eq(session))
                .filter(session_attachments::Column::FileName.eq(name))
                .count(&conn)
                .await?
                > 0)
        })
        .await
    }

    // Backfill before any collection. The durable marker makes reopening cheap;
    // a failed index transaction cannot authorize deletion of older attachments.
    pub(crate) async fn ensure_attachment_index(&self) -> Result<()> {
        let database = self.path().to_owned();
        self.write_access(move |conn| async move {
            let tx = conn.begin().await?;
            if storage_maintenance::Entity::find_by_id("attachment_index_v1")
                .one(&tx)
                .await?
                .is_some()
            {
                return Ok(());
            }
            // A fresh installation or a draft-only service has no historical
            // payload to index, and must not create domain/maintenance records.
            if sessions::Entity::find()
                .count(&tx)
                .await?
                == 0
            {
                return Ok(());
            }
            let mut sessions = sessions::Entity::find()
                .select_only()
                .column(sessions::Column::Id)
                .column(sessions::Column::History)
                .order_by_asc(sessions::Column::Id)
                .into_tuple::<(String, String)>()
                .paginate(&tx, 100);
            while let Some(rows) = sessions.fetch_and_next().await? {
                for (session, payload) in rows {
                    let messages: Vec<ChatMessage> = serde_json::from_str(&payload)?;
                    register(&tx, &database, &session, in_messages(&messages)).await?;
                }
            }
            let mut checkpoints = execution_checkpoints::Entity::find()
                .select_only()
                .column(runs::Column::SessionId)
                .column(execution_checkpoints::Column::Payload)
                .join(
                    JoinType::InnerJoin,
                    execution_checkpoints::Relation::Run.def(),
                )
                .order_by_asc(execution_checkpoints::Column::RunId)
                .into_tuple::<(String, String)>()
                .paginate(&tx, 100);
            while let Some(rows) = checkpoints.fetch_and_next().await? {
                for (session, payload) in rows {
                    let checkpoint: orbit_agent::checkpoint::ExecutionCheckpoint =
                        serde_json::from_str(&payload)?;
                    register(
                        &tx,
                        &database,
                        &session,
                        in_messages(&checkpoint.history).chain(checkpoint.attachments.iter()),
                    )
                    .await?;
                }
            }
            let mut messages = session_messages::Entity::find()
                .select_only()
                .column(session_messages::Column::SessionId)
                .column(session_messages::Column::Payload)
                .order_by_asc(session_messages::Column::Id)
                .into_tuple::<(String, String)>()
                .paginate(&tx, 100);
            while let Some(rows) = messages.fetch_and_next().await? {
                for (session, payload) in rows {
                    let message: ChatMessage = serde_json::from_str(&payload)?;
                    register(&tx, &database, &session, in_messages(&[message])).await?;
                }
            }
            let mut archives = context_archive_messages::Entity::find()
                .select_only()
                .column(context_archives::Column::SessionId)
                .column(context_archive_messages::Column::Payload)
                .join(
                    JoinType::InnerJoin,
                    context_archive_messages::Relation::Archive.def(),
                )
                .order_by_asc(context_archive_messages::Column::ArchiveId)
                .order_by_asc(context_archive_messages::Column::Ordinal)
                .into_tuple::<(String, String)>()
                .paginate(&tx, 100);
            while let Some(rows) = archives.fetch_and_next().await? {
                for (session, payload) in rows {
                    let message: ChatMessage = serde_json::from_str(&payload)?;
                    register(&tx, &database, &session, in_messages(&[message])).await?;
                }
            }
            storage_maintenance::ActiveModel {
                name: Set("attachment_index_v1".into()),
                ..Default::default()
            }
            .insert(&tx)
            .await?;
            tx.commit().await?;
            Ok(())
        })
        .await
    }

    /// Remove only immutable assets with no durable owner and no active lease.
    /// The database writer lease excludes concurrent ownership commits.
    pub async fn collect_attachment_garbage(&self) -> Result<u64> {
        let directory = AssetDirectory::for_database(self.path());
        if !directory.path().try_exists()? {
            return Ok(0);
        }
        self.write_access(move |conn| async move {
            let referenced: HashSet<String> = session_attachments::Entity::find()
                .select_only()
                .column(session_attachments::Column::FileName)
                .distinct()
                .into_tuple()
                .all(&conn)
                .await?
                .into_iter()
                .collect();
            tokio::task::spawn_blocking(move || -> Result<u64> {
                let lock = directory.lock()?;
                lock.prune_pending()?;
                let mut reclaimed = 0;
                for (name, size) in lock.files()? {
                    if !referenced.contains(&name) && lock.remove_if_unused(&name)? {
                        reclaimed += size;
                    }
                }
                Ok(reclaimed)
            })
            .await?
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_agent::checkpoint::ExecutionCheckpoint;

    fn image(database: &Path, content: &[u8]) -> Attachment {
        let (path, lease) = AssetDirectory::for_database(database)
            .import(content, "png")
            .unwrap();
        Attachment::Image {
            name: "upload.png".into(),
            mime_type: "image/png".into(),
            path,
            lease: Some(lease),
        }
    }

    #[tokio::test]
    async fn shared_ownership_survives_compaction_deletion_and_releases_only_after_last_reader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orbit.db");
        let store = Store::open(&path).await.unwrap();
        let mut first = store
            .create(dir.path().display().to_string(), "model".into())
            .await
            .unwrap();
        let second = store
            .create(dir.path().display().to_string(), "model".into())
            .await
            .unwrap();
        let first_id = first.id().to_owned();
        let second_id = second.id().to_owned();
        let attachment = image(&path, b"shared-image");
        let asset_path = attachment.path().to_owned();
        let message = ChatMessage::user_with_attachments("question", vec![attachment.clone()]);
        store
            .archive_context(&second_id, std::slice::from_ref(&message))
            .await
            .unwrap();
        first
            .begin("question".into(), "model".into())
            .await
            .unwrap();
        let journal = first.execution_journal().unwrap();
        journal.record_message(message.clone());
        journal
            .save(
                ExecutionCheckpoint {
                    history: vec![message],
                    turns: 0,
                    tool_calls: 0,
                    in_flight: vec![],
                    attachments: vec![],
                },
                None,
            )
            .await
            .unwrap();
        first
            .finish(
                &crate::types::RunCompletion::Completed {
                    history: vec![ChatMessage::user("compacted")],
                },
                vec![],
            )
            .await
            .unwrap();
        let usage = store
            .session_storage_usage()
            .await
            .unwrap();
        assert!(
            usage
                .iter()
                .all(|row| row.attachment_bytes == 12)
        );
        assert!(
            store
                .storage_footprint()
                .await
                .unwrap()
                .attachment_bytes
                >= 12
        );
        drop(journal);
        drop(first);
        drop(second);
        store
            .delete_session(&first_id)
            .await
            .unwrap();
        assert!(asset_path.exists());
        store
            .delete_session(&second_id)
            .await
            .unwrap();
        assert!(
            asset_path.exists(),
            "an upload or reader lease still owns the file"
        );
        drop(attachment);
        assert_eq!(
            store
                .collect_attachment_garbage()
                .await
                .unwrap(),
            12
        );
        assert!(!asset_path.exists());
        assert!(
            store
                .session_storage_usage()
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn backfill_indexes_previous_history_before_collecting_unowned_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orbit.db");
        let store = Store::open(&path).await.unwrap();
        let writer = store
            .create(dir.path().display().to_string(), "model".into())
            .await
            .unwrap();
        let id = writer.id().to_owned();
        let attachment = image(&path, b"old-image");
        let kept = attachment.path().to_owned();
        let orphan = image(&path, b"unused-image");
        let removed = orphan.path().to_owned();
        let message = ChatMessage::user_with_attachments("old history", vec![attachment.clone()]);
        let history = serde_json::to_string(&vec![message]).unwrap();
        let stored_id = id.clone();
        store
            .write_access(move |conn| async move {
                let tx = conn.begin().await?;
                sessions::Entity::update_many()
                    .col_expr(sessions::Column::History, sea_query::Expr::value(history))
                    .filter(sessions::Column::Id.eq(stored_id))
                    .exec(&tx)
                    .await?;
                storage_maintenance::Entity::delete_by_id("attachment_index_v1")
                    .exec(&tx)
                    .await?;
                tx.commit().await?;
                Ok(())
            })
            .await
            .unwrap();
        drop(attachment);
        drop(orphan);
        drop(writer);
        drop(store);
        let store = Store::open(&path).await.unwrap();
        assert!(kept.exists());
        assert!(!removed.exists());
        assert_eq!(
            store
                .session_storage_usage()
                .await
                .unwrap()[0]
                .attachment_bytes,
            9
        );
        store.delete_session(&id).await.unwrap();
        assert!(!kept.exists());
    }
}
