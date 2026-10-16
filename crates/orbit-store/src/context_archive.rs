//! Immutable pre-compaction snapshots. Reads always require the session ID;
//! knowing an archive ID never grants access to another conversation.
use crate::entities::*;
use anyhow::{Result, ensure};
use orbit_model::types::ChatMessage;
use sea_orm::sea_query::Expr;
use sea_orm::sea_query::{Alias, Func, SimpleExpr};
use sea_orm::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::store::Store;

const ARCHIVE_LIST_LIMIT: usize = 20;

impl Store {
    /// Discover the newest snapshots in this conversation without loading
    /// message bodies. IDs from another conversation are never listed.
    pub async fn list_context_archives(
        &self,
        session_id: &str,
        before_archive: Option<i64>,
    ) -> Result<Value> {
        ensure!(
            before_archive.is_none_or(|id| id > 0),
            "invalid history archive cursor"
        );
        let session_id = session_id.to_owned();

        self.read_access(move |conn| async move {
            let tx = conn.begin().await?;
            let mut select = context_archives::Entity::find()
                .filter(context_archives::Column::SessionId.eq(session_id));
            if let Some(before) = before_archive {
                select = select.filter(context_archives::Column::Id.lt(before));
            }
            let rows: Vec<(i64, i64, i64)> = select
                .select_only()
                .column(context_archives::Column::Id)
                .column(context_archives::Column::CreatedAt)
                .column_as(
                    Expr::col((
                        context_archive_messages::Entity,
                        context_archive_messages::Column::Ordinal,
                    ))
                    .count(),
                    "message_count",
                )
                .join(
                    JoinType::LeftJoin,
                    context_archive_messages::Relation::Archive
                        .def()
                        .rev(),
                )
                .group_by(context_archives::Column::Id)
                .group_by(context_archives::Column::CreatedAt)
                .order_by_desc(context_archives::Column::Id)
                .limit((ARCHIVE_LIST_LIMIT + 1) as u64)
                .into_tuple()
                .all(&tx)
                .await?;
            let mut archives = Vec::with_capacity(rows.len());
            for (id, created_at, count) in rows {
                archives.push(json!({
                    "archive_id": id,
                    "created_at": created_at,
                    "message_count": count
                }));
            }
            let has_more = archives.len() > ARCHIVE_LIST_LIMIT;
            archives.truncate(ARCHIVE_LIST_LIMIT);
            let next = if has_more {
                archives
                    .last()
                    .map(|archive| archive["archive_id"].clone())
            } else {
                None
            };
            let message = if archives.is_empty() {
                if before_archive.is_some() {
                    "No older archives in this conversation."
                } else {
                    "No pre-compaction archives exist in this conversation yet. Read_history only \
                        reads saved compaction snapshots; do not guess archive IDs."
                }
            } else {
                "Use an archive_id from this list to search or read saved original messages."
            };

            Ok(json!({
                "source_kind": "untrusted_session_history",
                "archives": archives,
                "next_before_archive": next,
                "message": message,
            }))
        })
        .await
    }

    /// Restore the complete snapshot for a retry of a previously incomplete
    /// compaction. The archive ID alone never grants cross-session access.
    pub async fn load_context_archive(
        &self,
        session_id: &str,
        archive_id: i64,
    ) -> Result<Vec<ChatMessage>> {
        ensure!(archive_id > 0, "invalid history archive ID");

        let session_id = session_id.to_owned();

        self.read_access(move |conn| async move {
            let tx = conn.begin().await?;
            ensure!(
                context_archives::Entity::find_by_id(archive_id)
                    .filter(context_archives::Column::SessionId.eq(session_id))
                    .one(&tx)
                    .await?
                    .is_some(),
                "history archive not found in this session"
            );
            context_archive_messages::Entity::find()
                .filter(context_archive_messages::Column::ArchiveId.eq(archive_id))
                .order_by_asc(context_archive_messages::Column::Ordinal)
                .all(&tx)
                .await?
                .into_iter()
                .map(|row| serde_json::from_str(&row.payload).map_err(Into::into))
                .collect()
        })
        .await
    }

    pub async fn archive_context(&self, session_id: &str, messages: &[ChatMessage]) -> Result<i64> {
        let session_id = session_id.to_owned();
        let database = self.path().to_owned();
        let attachments = crate::attachments::in_messages(messages)
            .cloned()
            .collect::<Vec<_>>();
        let messages = messages
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()?;
        let fingerprint = Sha256::digest(serde_json::to_vec(&messages)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();

        self.write_access(move |conn| async move {
            let tx = conn.begin().await?;
            crate::attachments::register(&tx, &database, &session_id, attachments.iter()).await?;
            if let Some(row) = context_archives::Entity::find()
                .filter(context_archives::Column::SessionId.eq(&session_id))
                .filter(context_archives::Column::ContentSha256.eq(&fingerprint))
                .one(&tx)
                .await?
            {
                tx.commit().await?;
                return Ok(row.id);
            }
            let row = context_archives::ActiveModel {
                session_id: Set(session_id),
                content_sha256: Set(Some(fingerprint)),
                ..Default::default()
            }
            .insert(&tx)
            .await?;
            for (index, payload) in messages.into_iter().enumerate() {
                context_archive_messages::ActiveModel {
                    archive_id: Set(row.id),
                    ordinal: Set(index as i64),
                    payload: Set(payload),
                }
                .insert(&tx)
                .await?;
            }
            tx.commit().await?;
            Ok(row.id)
        })
        .await
    }

    /// `message` selects an exact source message. Otherwise browse/search by
    /// ordinal; character paging keeps even a huge tool result bounded.
    #[allow(clippy::too_many_arguments)]
    pub async fn read_context_archive(
        &self,
        session_id: &str,
        archive_id: i64,
        message: Option<usize>,
        after_message: Option<usize>,
        query: Option<String>,
        offset: usize,
        length: usize,
    ) -> Result<Value> {
        ensure!(
            archive_id > 0 && length > 0 && length <= 16_000,
            "invalid history read range"
        );
        ensure!(offset < i64::MAX as usize, "history offset is too large");
        ensure!(
            message.is_none_or(|i| i <= i64::MAX as usize)
                && after_message.is_none_or(|i| i <= i64::MAX as usize),
            "history message index is too large"
        );
        ensure!(
            query
                .as_ref()
                .is_none_or(|q| !q.is_empty() && q.len() <= 512),
            "history query must contain 1–512 bytes"
        );

        let session = session_id.to_owned();

        self.read_access(move |conn| async move {
            let tx = conn.begin().await?;
            let exists = context_archives::Entity::find_by_id(archive_id)
                .filter(context_archives::Column::SessionId.eq(session))
                .one(&tx)
                .await?
                .is_some();
            ensure!(
                exists,
                "history archive not found in this session; call ReadHistory with \
                {{}} to discover available archive IDs instead of guessing"
            );
            // Slice in SQLite so a preview never materializes a whole large message.
            let payload: SimpleExpr = Expr::col(context_archive_messages::Column::Payload).into();
            let function = |name: &str, args: Vec<SimpleExpr>| -> SimpleExpr {
                Func::cust(Alias::new(name))
                    .args(args)
                    .into()
            };
            let mut select = context_archive_messages::Entity::find()
                .filter(context_archive_messages::Column::ArchiveId.eq(archive_id))
                .filter(
                    context_archive_messages::Column::Ordinal
                        .gt(after_message.map_or(-1, |i| i as i64)),
                );
            if let Some(message) = message {
                select =
                    select.filter(context_archive_messages::Column::Ordinal.eq(message as i64));
            }
            let (start, hit) = if let Some(query) = query.as_ref() {
                let position = function("instr", vec![payload.clone(), Expr::value(query.clone())]);
                select = select.filter(Expr::expr(position.clone()).gt(0));
                (
                    function(
                        "max",
                        vec![Expr::value(1), Expr::expr(position.clone()).sub(120)],
                    ),
                    Expr::expr(position).sub(1),
                )
            } else {
                (Expr::value(offset as i64 + 1), Expr::value(0))
            };
            let rows: Vec<(i64, i64, String, i64)> = select
                .select_only()
                .column(context_archive_messages::Column::Ordinal)
                .column_as(function("length", vec![payload.clone()]), "total")
                .column_as(
                    function("substr", vec![payload, start, Expr::value(length as i64)]),
                    "text",
                )
                .column_as(hit, "hit")
                .order_by_asc(context_archive_messages::Column::Ordinal)
                .limit(9)
                .into_tuple()
                .all(&tx)
                .await?;
            let has_more = rows.len() > 8;
            let mut items = Vec::new();
            for (index, total, text, hit) in rows.into_iter().take(8) {
                let total = total as usize;
                let start = if query.is_some() {
                    hit.saturating_sub(120).max(0) as usize
                } else {
                    offset
                };
                let next = start + text.chars().count();
                items.push(json!({
                    "message": index,
                    "serialized_message": text,
                    "offset": start,
                    "total_characters": total,
                    "next_offset": (next < total).then_some(next)
                }));
            }

            let next_message = if has_more {
                items
                    .last()
                    .map(|i| i["message"].clone())
            } else {
                None
            };

            Ok(json!({
                "archive_id": archive_id,
                "source_kind": "untrusted_session_history",
                "items": items,
                "next_after_message": next_message
            }))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn archive_listing_is_bounded_newest_first_and_session_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("orbit.db"))
            .await
            .unwrap();
        let owner = store
            .create(dir.path().display().to_string(), "model".into())
            .await
            .unwrap();
        let foreign = store
            .create(dir.path().display().to_string(), "model".into())
            .await
            .unwrap();

        let empty = store
            .list_context_archives(owner.id(), None)
            .await
            .unwrap();
        assert_eq!(empty["archives"], json!([]));
        assert!(
            empty["message"]
                .as_str()
                .unwrap()
                .contains("No pre-compaction archives")
        );

        let mut ids = Vec::new();
        for index in 0..ARCHIVE_LIST_LIMIT + 3 {
            ids.push(
                store
                    .archive_context(
                        owner.id(),
                        &[
                            ChatMessage::user(format!("private source body {index}")),
                            ChatMessage::assistant("answer"),
                        ],
                    )
                    .await
                    .unwrap(),
            );
        }

        let foreign_id = store
            .archive_context(foreign.id(), &[ChatMessage::user("foreign body")])
            .await
            .unwrap();
        let first = store
            .list_context_archives(owner.id(), None)
            .await
            .unwrap();
        assert_eq!(
            first["archives"]
                .as_array()
                .unwrap()
                .len(),
            ARCHIVE_LIST_LIMIT
        );
        assert_eq!(first["archives"][0]["archive_id"], *ids.last().unwrap());
        assert_eq!(first["archives"][0]["message_count"], 2);
        assert!(
            first["archives"][0]["created_at"]
                .as_i64()
                .is_some()
        );
        assert!(
            !first
                .to_string()
                .contains("private source body")
        );

        let second = store
            .list_context_archives(owner.id(), first["next_before_archive"].as_i64())
            .await
            .unwrap();
        assert_eq!(
            second["archives"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert!(second["next_before_archive"].is_null());
        let listed: Vec<_> = [first, second]
            .into_iter()
            .flat_map(|page| {
                page["archives"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|archive| archive["archive_id"].as_i64().unwrap())
                    .collect::<Vec<_>>()
            })
            .collect();

        ids.reverse();
        assert_eq!(listed, ids);
        assert!(!listed.contains(&foreign_id));
        assert_eq!(
            store
                .list_context_archives(foreign.id(), None)
                .await
                .unwrap()["archives"][0]["archive_id"],
            foreign_id
        );
        assert!(
            store
                .list_context_archives(owner.id(), Some(0))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn archive_is_durable_paged_and_session_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orbit.db");
        let store = Store::open(&path).await.unwrap();
        let owner = store
            .create(dir.path().display().to_string(), "model".into())
            .await
            .unwrap();
        let foreign = store
            .create(dir.path().display().to_string(), "model".into())
            .await
            .unwrap();
        let history = vec![
            ChatMessage::user("保留 API"),
            ChatMessage::tool(
                "call",
                format!("{}错误证据{}", "甲".repeat(4_000), "乙".repeat(4_000)),
            ),
        ];
        let archive = store
            .archive_context(owner.id(), &history)
            .await
            .unwrap();

        assert_eq!(
            store
                .archive_context(owner.id(), &history)
                .await
                .unwrap(),
            archive,
            "repeated compaction of one source must preserve archive references"
        );

        let reopened = Store::open(path).await.unwrap();

        assert_eq!(
            reopened
                .load_context_archive(owner.id(), archive)
                .await
                .unwrap(),
            history
        );
        assert!(
            reopened
                .load_context_archive(foreign.id(), archive)
                .await
                .is_err(),
            "an archive ID must not expose another session's history"
        );
        assert!(
            reopened
                .read_context_archive(foreign.id(), archive, None, None, None, 0, 100)
                .await
                .is_err()
        );

        let hit = reopened
            .read_context_archive(
                owner.id(),
                archive,
                None,
                None,
                Some("错误证据".into()),
                0,
                300,
            )
            .await
            .unwrap();

        assert!(
            hit["items"][0]["serialized_message"]
                .as_str()
                .unwrap()
                .contains("错误证据")
        );

        let mut recovered = String::new();
        let mut offset = 0;

        loop {
            let page = reopened
                .read_context_archive(owner.id(), archive, Some(1), None, None, offset, 300)
                .await
                .unwrap();
            let item = &page["items"][0];
            recovered.push_str(
                item["serialized_message"]
                    .as_str()
                    .unwrap(),
            );

            match item["next_offset"].as_u64() {
                Some(next) => offset = next as usize,
                None => break,
            }
        }

        assert_eq!(
            serde_json::from_str::<ChatMessage>(&recovered).unwrap(),
            history[1]
        );
    }
}
