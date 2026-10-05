//! Immutable pre-compaction snapshots. Reads always require the session ID;
//! knowing an archive ID never grants access to another conversation.
use anyhow::{Result, ensure};
use orbit_model::types::ChatMessage;
use rusqlite::params;
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

        self.access(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT a.id,a.created_at,
                        (SELECT count(*) FROM context_archive_messages m WHERE m.archive_id=a.id)
                 FROM context_archives a
                 WHERE a.session_id=?1 AND (?2 IS NULL OR a.id<?2)
                 ORDER BY a.id DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(
                params![session_id, before_archive, (ARCHIVE_LIST_LIMIT + 1) as i64],
                |row| Ok(json!({
                    "archive_id": row.get::<_, i64>(0)?,
                    "created_at": row.get::<_, i64>(1)?,
                    "message_count": row.get::<_, i64>(2)?,
                })),
            )?;
            let mut archives = rows.collect::<Result<Vec<_>, _>>()?;
            let has_more = archives.len() > ARCHIVE_LIST_LIMIT;
            archives.truncate(ARCHIVE_LIST_LIMIT);
            let next = if has_more {
                archives.last().map(|archive| archive["archive_id"].clone())
            } else {
                None
            };
            let message = if archives.is_empty() {
                if before_archive.is_some() {
                    "No older archives in this conversation."
                } else {
                    "No pre-compaction archives exist in this conversation yet. Read_history only reads saved compaction snapshots; do not guess archive IDs."
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
        }).await
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

        self.access(move |conn| {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM context_archives WHERE id=?1 AND session_id=?2)",
                params![archive_id, session_id],
                |row| row.get(0),
            )?;

            ensure!(exists, "history archive not found in this session");

            let mut stmt = conn.prepare(
                "SELECT payload FROM context_archive_messages WHERE archive_id=?1 ORDER BY ordinal",
            )?;
            let rows = stmt.query_map(params![archive_id], |row| row.get::<_, String>(0))?;
            let mut messages = Vec::new();

            for row in rows {
                messages.push(serde_json::from_str(&row?)?);
            }

            Ok(messages)
        })
        .await
    }

    pub async fn archive_context(&self, session_id: &str, messages: &[ChatMessage]) -> Result<i64> {
        let session_id = session_id.to_owned();
        let messages = messages
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()?;
        let fingerprint = Sha256::digest(serde_json::to_vec(&messages)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();

        self.access(move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute(
                "INSERT OR IGNORE INTO context_archives(session_id,content_sha256) VALUES (?1,?2)",
                params![session_id, fingerprint],
            )?;
            let inserted = tx.changes() > 0;
            let id: i64 = tx.query_row(
                "SELECT id FROM context_archives WHERE session_id=?1 AND content_sha256=?2",
                params![session_id, fingerprint],
                |row| row.get(0),
            )?;

            if !inserted {
                tx.commit()?;
                return Ok(id);
            }

            {
                let mut insert = tx.prepare("INSERT INTO context_archive_messages(archive_id,ordinal,payload) VALUES (?1,?2,?3)")?;

                for (index, message) in messages.iter().enumerate() {
                    insert.execute(params![id, index as i64, message])?;
                }
            }
            tx.commit()?;

            Ok(id)
        }).await
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

        self.access(move |conn| {
            let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM context_archives WHERE id=?1 AND session_id=?2)", params![archive_id,session], |r| r.get(0))?;
            ensure!(exists, "history archive not found in this session; call read_history with {{}} to discover available archive IDs instead of guessing");

            // SQL performs slicing: do not load the entire snapshot to show a
            // few hits. Query previews begin near the first matching text.
            let mut stmt = conn.prepare("SELECT ordinal,length(payload),substr(payload,?5+1,?6),
                CASE WHEN ?4 IS NULL THEN 0 ELSE instr(payload,?4)-1 END,
                CASE WHEN ?4 IS NULL THEN '' ELSE substr(payload,max(1,instr(payload,?4)-120),?6) END
                FROM context_archive_messages
                WHERE archive_id=?1 AND (?2 IS NULL OR ordinal=?2) AND ordinal>?3
                  AND (?4 IS NULL OR instr(payload,?4)>0)
                ORDER BY ordinal LIMIT 9")?;
            let mut rows = stmt.query(params![archive_id, message.map(|i| i as i64), after_message.map(|i| i as i64).unwrap_or(-1), query, offset as i64, length as i64])?;
            let mut items = Vec::new();
            let mut has_more = false;

            while let Some(row) = rows.next()? {
                if items.len() == 8 { has_more = true; break; }

                let index: i64 = row.get(0)?;
                let total = row.get::<_, i64>(1)? as usize;
                let hit: i64 = row.get(3)?;
                let start = if query.is_some() { hit.saturating_sub(120).max(0) as usize } else { offset };

                let text: String = row.get(if query.is_some() {4} else {2})?;
                let next = start + text.chars().count();
                items.push(json!({
                    "message": index,
                    "serialized_message": text,
                    "offset": start,
                    "total_characters": total,
                    "next_offset": (next < total).then_some(next)
                }));
            }

            let next_message = if has_more { items.last().map(|i| i["message"].clone()) } else { None };

            Ok(json!({ "archive_id": archive_id, "source_kind": "untrusted_session_history", "items": items, "next_after_message": next_message }))
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn archive_listing_is_bounded_newest_first_and_session_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("orbit.db")).await.unwrap();
        let owner = store
            .create(dir.path().display().to_string(), "model".into(), false)
            .await
            .unwrap();
        let foreign = store
            .create(dir.path().display().to_string(), "model".into(), false)
            .await
            .unwrap();

        let empty = store.list_context_archives(owner.id(), None).await.unwrap();
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
        let first = store.list_context_archives(owner.id(), None).await.unwrap();
        assert_eq!(
            first["archives"].as_array().unwrap().len(),
            ARCHIVE_LIST_LIMIT
        );
        assert_eq!(first["archives"][0]["archive_id"], *ids.last().unwrap());
        assert_eq!(first["archives"][0]["message_count"], 2);
        assert!(first["archives"][0]["created_at"].as_i64().is_some());
        assert!(!first.to_string().contains("private source body"));

        let second = store
            .list_context_archives(owner.id(), first["next_before_archive"].as_i64())
            .await
            .unwrap();
        assert_eq!(second["archives"].as_array().unwrap().len(), 3);
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
            .create(dir.path().display().to_string(), "model".into(), false)
            .await
            .unwrap();
        let foreign = store
            .create(dir.path().display().to_string(), "model".into(), false)
            .await
            .unwrap();
        let history = vec![
            ChatMessage::user("保留 API"),
            ChatMessage::tool(
                "call",
                format!("{}错误证据{}", "甲".repeat(4000), "乙".repeat(4000)),
            ),
        ];
        let archive = store.archive_context(owner.id(), &history).await.unwrap();

        assert_eq!(
            store.archive_context(owner.id(), &history).await.unwrap(),
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
            recovered.push_str(item["serialized_message"].as_str().unwrap());

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
