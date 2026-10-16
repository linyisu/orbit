//! Presentation reads do not load the model projection or a whole conversation.
use anyhow::{Context, Result, ensure};
use orbit_agent::events::AgentEvent;
use orbit_protocol::RunPresentation;
type Update = orbit_protocol::Update<AgentEvent>;
use sea_orm::*;

use crate::{
    entities::{events, runs, session_messages},
    store::Store,
};

#[derive(Clone, Debug)]
pub struct RunIndex {
    pub id: i64,
    pub start: usize,
    pub events: usize,
    pub finished: bool,
}

impl RunIndex {
    pub fn end(&self) -> usize {
        self.start + 1 + self.events + usize::from(self.finished)
    }
}

impl Store {
    /// Freeze the shape of the existing transcript when a session opens. Only
    /// IDs and counts are retained; closed runs cannot change under its writer.
    pub async fn transcript_index(&self, session: &str) -> Result<Vec<RunIndex>> {
        let session = session.to_owned();
        self.read_access(move |conn| async move {
            let rows = runs::Entity::find()
                .select_only()
                .column(runs::Column::Id)
                .column(runs::Column::Status)
                .column_as(
                    sea_query::Expr::col((events::Entity, events::Column::Id)).count(),
                    "event_count",
                )
                .join_rev(JoinType::LeftJoin, events::Relation::Run.def())
                .filter(runs::Column::SessionId.eq(session))
                .group_by(runs::Column::Id)
                .order_by_asc(runs::Column::Id)
                .into_tuple::<(i64, String, i64)>()
                .all(&conn)
                .await?;
            let mut start = 0;
            Ok(rows
                .into_iter()
                .map(|(id, status, count)| {
                    let run = RunIndex {
                        id,
                        start,
                        events: count as usize,
                        finished: status != "running",
                    };
                    start = run.end();
                    run
                })
                .collect())
        })
        .await
    }

    /// Read at most one transport page from a run. Cursor positions include
    /// Started/Finished, so paging through empty and interrupted runs is exact.
    pub async fn transcript_run_page(
        &self,
        session: &str,
        index: &RunIndex,
        after: usize,
        limit: usize,
    ) -> Result<Vec<Update>> {
        ensure!(
            after >= index.start && after < index.end(),
            "invalid run cursor"
        );
        ensure!(
            (1..=orbit_protocol::PAGE_EVENTS).contains(&limit),
            "invalid transcript page size"
        );
        let index = index.clone();
        let session = session.to_owned();
        self.read_access(move |conn| async move {
            let tx = conn.begin().await?;
            let row = runs::Entity::find_by_id(index.id)
                .filter(runs::Column::SessionId.eq(&session))
                .one(&tx)
                .await?
                .context("transcript run no longer exists")?;
            let mut updates = Vec::new();
            let mut position = after - index.start;
            if position == 0 {
                let message = session_messages::Entity::find()
                    .filter(session_messages::Column::RunId.eq(index.id))
                    .filter(session_messages::Column::SessionId.eq(&session))
                    .filter(session_messages::Column::Ordinal.eq(0))
                    .one(&tx)
                    .await?
                    .map(|row| {
                        crate::session_log::decode(row.id, row.run_id, row.payload, row.sha256)
                    })
                    .transpose()?;
                let attachments = match message.map(|record| record.message) {
                    Some(orbit_model::types::ChatMessage::User {
                        content,
                        attachments,
                    }) if content == row.prompt => attachments,
                    _ => Vec::new(),
                };
                let metadata =
                    |attachment: &orbit_model::types::Attachment| orbit_protocol::MessageImage {
                        name: attachment.name().into(),
                        path: attachment
                            .path()
                            .to_string_lossy()
                            .into_owned(),
                    };
                let files = attachments
                    .iter()
                    .filter(|a| !a.is_image())
                    .map(metadata)
                    .collect();
                let image_assets = attachments
                    .iter()
                    .filter(|a| a.is_image())
                    .map(metadata)
                    .collect::<Vec<_>>();
                let first = |tag: &'static str| {
                    events::Entity::find()
                        .filter(events::Column::RunId.eq(index.id))
                        .filter(events::Column::Payload.starts_with(tag))
                        .order_by_asc(events::Column::Id)
                };
                let input = first("{\"InputReceived\":")
                    .one(&tx)
                    .await?;
                let turn = first("{\"TurnStarted\":")
                    .one(&tx)
                    .await?;
                let queued_input =
                    input.is_some_and(|input| turn.is_none_or(|turn| input.id < turn.id));
                updates.push(Update::Started {
                    presentation: RunPresentation {
                        file_assets: files,
                        replayed: true,
                        compact: row.prompt == "/compact",
                        queued_input,
                        images: image_assets
                            .iter()
                            .map(|image| image.name.clone())
                            .collect(),
                        image_assets,
                    },
                    prompt: row.prompt,
                });
                position += 1;
            }
            let offset = position - 1;
            let remaining = limit.saturating_sub(updates.len());
            let count = index
                .events
                .saturating_sub(offset)
                .min(remaining);
            if count > 0 {
                for event in events::Entity::find()
                    .filter(events::Column::RunId.eq(index.id))
                    .order_by_asc(events::Column::Id)
                    .limit(count as u64)
                    .offset(offset as u64)
                    .all(&tx)
                    .await?
                {
                    updates.push(Update::agent(serde_json::from_str::<AgentEvent>(
                        &event.payload,
                    )?));
                    position += 1;
                }
            }
            if position == index.events + 1 && index.finished && updates.len() < limit {
                updates.push(Update::Finished {
                    elapsed_ms: row
                        .elapsed_ms
                        .map(|ms| ms.max(0) as u64),
                    status: row.status,
                });
            }
            tx.commit().await?;
            Ok(updates)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_model::types::{ChatMessage, StreamEvent, ToolCall};

    #[tokio::test]
    async fn paged_transcript_matches_full_receipts_while_runtime_keeps_only_execution_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("orbit.db"))
            .await
            .unwrap();
        let mut writer = store
            .create(dir.path().display().to_string(), "test".into())
            .await
            .unwrap();
        let id = writer.id().to_owned();
        for run in 0..4 {
            writer
                .begin(format!("question {run}"), "test".into())
                .await
                .unwrap();
            let journal = writer.execution_journal().unwrap();
            for index in 0..140 {
                journal.record(AgentEvent::Model(StreamEvent::TextDelta(format!(
                    "{run}/{index} {}",
                    "body".repeat(200)
                ))));
                journal.record(AgentEvent::ReasoningFinished { elapsed_ms: index });
            }
            journal.record(AgentEvent::RequestMetrics {
                profile: "test".into(),
                estimated_input: 10,
                retries: None,
                usage: None,
                elapsed_ms: 1,
            });
            // File activity is recovered from successful receipts even when the
            // containing message has already left the model's context window.
            let receipt = AgentEvent::ToolFinished {
                outcome: None,
                files: None,
                call: ToolCall::function(
                    format!("read-{run}"),
                    "ReadFile",
                    serde_json::json!({
                        "path": "source.rs"
                    }),
                ),
                content: serde_json::json!({
                    "path": "source.rs",
                    "content": "source".repeat(20_000),
                    "total_lines": 200
                })
                .to_string(),
                changes: vec![],
            };
            journal
                .save(
                    orbit_agent::checkpoint::ExecutionCheckpoint {
                        history: vec![ChatMessage::user(format!("question {run}"))],
                        turns: 1,
                        tool_calls: 1,
                        in_flight: vec![],
                        attachments: vec![],
                    },
                    Some(receipt),
                )
                .await
                .unwrap();
            writer
                .finish(
                    &crate::types::RunCompletion::Completed {
                        history: vec![ChatMessage::user(format!("question {run}"))],
                    },
                    vec![AgentEvent::Completed {
                        turns: 1,
                        tool_calls: 1,
                    }],
                )
                .await
                .unwrap();
        }
        let full = store.load(&id).await.unwrap();
        let runtime = store.load_runtime(&id).await.unwrap();
        assert_eq!(runtime.runs.len(), full.runs.len());
        assert_eq!(
            serde_json::to_value(&runtime.history).unwrap(),
            serde_json::to_value(&full.history).unwrap()
        );
        assert_eq!(
            runtime.files,
            orbit_agent::memory::FileActivity::from_events(
                full.runs
                    .iter()
                    .flat_map(|run| &run.events)
            )
        );
        assert!(
            serde_json::to_vec(&*runtime)
                .unwrap()
                .len()
                < serde_json::to_vec(&full).unwrap().len() / 100
        );

        let index = store
            .transcript_index(&id)
            .await
            .unwrap();
        let mut expected = Vec::new();
        for run in full.runs {
            expected.push(Update::Started {
                prompt: run.prompt,
                presentation: RunPresentation {
                    replayed: true,
                    ..Default::default()
                },
            });
            expected.extend(
                run.events
                    .into_iter()
                    .map(Update::agent),
            );
            expected.push(Update::Finished {
                status: run.status,
                elapsed_ms: run.elapsed_ms,
            });
        }
        let mut actual = Vec::new();
        let mut after = 0;
        for run in &index {
            while after < run.end() {
                let page = store
                    .transcript_run_page(&id, run, after, 17)
                    .await
                    .unwrap();
                assert!(!page.is_empty() && page.len() <= 17);
                after += page.len();
                actual.extend(page);
            }
        }
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }
}
