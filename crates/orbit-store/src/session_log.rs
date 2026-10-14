//! Append-only conversation messages, separate from the mutable model projection.
use crate::entities::*;
use anyhow::{Result, ensure};
use orbit_model::types::ChatMessage;
#[cfg(test)]
use sea_orm::sea_query::Expr;
use sea_orm::*;
use sha2::{Digest, Sha256};

use crate::store::Store;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalMessage {
    pub id: i64,
    pub run_id: i64,
    pub message: ChatMessage,
}

pub(crate) fn decode(
    id: i64,
    run_id: i64,
    payload: String,
    expected: String,
) -> Result<CanonicalMessage> {
    let actual = Sha256::digest(payload.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    ensure!(
        actual == expected,
        "canonical message {id} failed integrity check"
    );
    Ok(CanonicalMessage {
        id,
        run_id,
        message: serde_json::from_str(&payload)?,
    })
}

impl Store {
    /// Return committed messages in stable order. A legacy gap means the
    /// pre-migration conversation cannot be asserted to be complete.
    pub async fn canonical_messages(
        &self,
        session_id: &str,
        after_id: Option<i64>,
        limit: usize,
    ) -> Result<(bool, Vec<CanonicalMessage>)> {
        ensure!(
            (1..=1_000).contains(&limit),
            "message page must contain 1–1000 entries"
        );

        let session_id = session_id.to_owned();

        self.read_access(move |conn| async move {
            let tx = conn.begin().await?;
            ensure!(
                sessions::Entity::find_by_id(&session_id)
                    .count(&tx)
                    .await?
                    > 0,
                "session not found"
            );
            let gap = session_log_gaps::Entity::find_by_id(&session_id)
                .one(&tx)
                .await?
                .is_some();
            let rows = session_messages::Entity::find()
                .filter(session_messages::Column::SessionId.eq(&session_id))
                .filter(session_messages::Column::Id.gt(after_id.unwrap_or(0)))
                .order_by_asc(session_messages::Column::Id)
                .limit(limit as u64)
                .all(&tx)
                .await?;
            let messages = rows
                .into_iter()
                .map(|row| decode(row.id, row.run_id, row.payload, row.sha256))
                .collect::<Result<Vec<_>>>()?;
            tx.commit().await?;
            Ok((gap, messages))
        })
        .await
    }

    /// Read the last committed message of one run without traversing its tool log.
    pub async fn canonical_tail(
        &self,
        session_id: &str,
        run_id: Option<i64>,
    ) -> Result<(bool, Option<CanonicalMessage>)> {
        let session_id = session_id.to_owned();
        self.read_access(move |conn| async move {
            let tx = conn.begin().await?;
            ensure!(
                sessions::Entity::find_by_id(&session_id)
                    .count(&tx)
                    .await?
                    > 0,
                "session not found"
            );
            let gap = session_log_gaps::Entity::find_by_id(&session_id)
                .one(&tx)
                .await?
                .is_some();
            let run = match run_id {
                Some(id) => Some(id),
                None => runs::Entity::find()
                    .filter(runs::Column::SessionId.eq(&session_id))
                    .order_by_desc(runs::Column::Id)
                    .one(&tx)
                    .await?
                    .map(|row| row.id),
            };
            let row = match run {
                Some(id) => {
                    session_messages::Entity::find()
                        .filter(session_messages::Column::SessionId.eq(&session_id))
                        .filter(session_messages::Column::RunId.eq(id))
                        .order_by_desc(session_messages::Column::Ordinal)
                        .one(&tx)
                        .await?
                }
                None => None,
            };
            let message = row
                .map(|row| decode(row.id, row.run_id, row.payload, row.sha256))
                .transpose()?;
            tx.commit().await?;
            Ok((gap, message))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use orbit_agent::{
        agent::{Agent, AgentConfig},
        checkpoint::ExecutionCheckpoint,
        model::MockModel,
        tool::{ClosureTool, ToolRegistry},
    };
    use orbit_model::types::{ChatCompletion, ChatRequest, ToolCall};
    use tokio_util::sync::CancellationToken;

    use super::*;

    #[tokio::test]
    async fn replay_images_are_bound_to_their_run_after_projection_compaction() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("orbit.db"))
            .await
            .unwrap();
        let mut writer = store
            .create(directory.path().display().to_string(), "model".into())
            .await
            .unwrap();
        let id = writer.id().to_owned();
        for asset in ["first.png", "second.png"] {
            // Identical prompts and upload names must retain distinct assets.
            writer
                .begin(String::new(), "model".into())
                .await
                .unwrap();
            let message = ChatMessage::user_with_attachments(
                "",
                vec![orbit_model::types::Attachment::Image {
                    lease: None,
                    name: "clipboard.png".into(),
                    mime_type: "image/png".into(),
                    path: directory.path().join(asset),
                }],
            );
            let journal = writer.execution_journal().unwrap();
            journal.record_message(message.clone());
            journal
                .save(
                    ExecutionCheckpoint {
                        history: vec![message],
                        turns: 0,
                        tool_calls: 0,
                        in_flight: Vec::new(),
                        attachments: vec![],
                    },
                    None,
                )
                .await
                .unwrap();
            writer
                .finish(
                    "completed",
                    vec![],
                    Some(vec![ChatMessage::user("compacted")]),
                )
                .await
                .unwrap();
        }
        let snapshot = store.load(&id).await.unwrap();
        assert_eq!(snapshot.runs.len(), 2);
        for (run, asset) in snapshot
            .runs
            .iter()
            .zip(["first.png", "second.png"])
        {
            assert_eq!(
                run.images,
                vec![orbit_protocol::MessageImage {
                    name: "clipboard.png".into(),
                    path: directory
                        .path()
                        .join(asset)
                        .to_string_lossy()
                        .into_owned(),
                }]
            );
        }
    }

    #[tokio::test]
    async fn canonical_messages_survive_projection_replacement_and_resume() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("orbit.db");
        let store = Store::open(&path).await.unwrap();
        let mut writer = store
            .create(directory.path().display().to_string(), "model".into())
            .await
            .unwrap();
        let id = writer.id().to_owned();
        writer
            .begin("hello".into(), "model".into())
            .await
            .unwrap();
        let journal = writer.execution_journal().unwrap();
        let user = ChatMessage::user("hello");
        let reply = ChatMessage::assistant("the original detailed answer");
        journal.record_message(user.clone());
        journal
            .save(
                ExecutionCheckpoint {
                    history: vec![user.clone()],
                    turns: 0,
                    tool_calls: 0,
                    in_flight: Vec::new(),
                    attachments: vec![],
                },
                None,
            )
            .await
            .unwrap();
        journal.record_message(reply.clone());
        journal
            .save(
                ExecutionCheckpoint {
                    history: vec![user.clone(), reply.clone()],
                    turns: 1,
                    tool_calls: 0,
                    in_flight: Vec::new(),
                    attachments: vec![],
                },
                None,
            )
            .await
            .unwrap();
        writer
            .finish(
                "completed",
                vec![],
                Some(vec![ChatMessage::user("compacted task state")]),
            )
            .await
            .unwrap();

        let reopened = Store::open(path).await.unwrap();
        let (legacy_gap, messages) = reopened
            .canonical_messages(&id, None, 10)
            .await
            .unwrap();

        assert!(!legacy_gap);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].message, user);
        assert_eq!(messages[1].message, reply);
        let (gap, tail) = reopened
            .canonical_tail(&id, None)
            .await
            .unwrap();
        assert!(!gap);
        assert_eq!(tail.unwrap().message, reply);
        assert_eq!(
            reopened
                .load(&id)
                .await
                .unwrap()
                .history,
            vec![ChatMessage::user("compacted task state")]
        );
    }

    #[tokio::test]
    async fn agent_persists_each_new_message_to_the_canonical_log() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("orbit.db"))
            .await
            .unwrap();
        let mut writer = store
            .create(directory.path().display().to_string(), "model".into())
            .await
            .unwrap();
        let id = writer.id().to_owned();
        writer
            .begin("hello".into(), "model".into())
            .await
            .unwrap();
        let model = MockModel::new(vec![ChatCompletion {
            id: None,
            model: None,
            message: ChatMessage::assistant("world"),
            finish_reason: Some("stop".into()),
            usage: None,
        }]);
        let agent = Agent::new(model, ToolRegistry::new(), AgentConfig::default())
            .with_execution_journal(writer.execution_journal().unwrap());
        let result = agent
            .run(
                ChatRequest::new(vec![ChatMessage::user("hello")]),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        writer
            .finish("completed", vec![], Some(result.history))
            .await
            .unwrap();

        let (legacy_gap, messages) = store
            .canonical_messages(&id, None, 10)
            .await
            .unwrap();

        assert!(!legacy_gap);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].message, ChatMessage::user("hello"));
        assert_eq!(messages[1].message, ChatMessage::assistant("world"));
    }

    #[tokio::test]
    async fn nonexistent_session_is_not_reported_as_an_empty_log() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("orbit.db"))
            .await
            .unwrap();

        assert!(
            store
                .canonical_messages("missing", None, 10)
                .await
                .is_err()
        );
        assert!(
            store
                .canonical_tail("missing", None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn tail_is_scoped_to_the_run_and_checks_integrity() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("orbit.db"))
            .await
            .unwrap();
        let mut writer = store
            .create(directory.path().display().to_string(), "orbit-model".into())
            .await
            .unwrap();
        let id = writer.id().to_owned();
        writer
            .begin("Orbit 第一轮".into(), "orbit-model".into())
            .await
            .unwrap();
        let first = writer.active_run_id().unwrap();
        let journal = writer.execution_journal().unwrap();
        journal.record_message(ChatMessage::assistant("Orbit 第一轮结果"));
        journal.flush().await.unwrap();
        writer
            .finish("completed", vec![], Some(vec![]))
            .await
            .unwrap();
        writer
            .begin("Orbit 第二轮".into(), "orbit-model".into())
            .await
            .unwrap();
        assert!(
            store
                .canonical_tail(&id, None)
                .await
                .unwrap()
                .1
                .is_none()
        );
        assert_eq!(
            store
                .canonical_tail(&id, Some(first))
                .await
                .unwrap()
                .1
                .unwrap()
                .message,
            ChatMessage::assistant("Orbit 第一轮结果")
        );
        store
            .write_access(move |conn| async move {
                session_messages::Entity::update_many()
                    .col_expr(session_messages::Column::Payload, Expr::value("{}"))
                    .filter(session_messages::Column::RunId.eq(first))
                    .exec(&conn)
                    .await?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            store
                .canonical_tail(&id, Some(first))
                .await
                .unwrap_err()
                .to_string()
                .contains("integrity check")
        );
    }

    #[tokio::test]
    async fn agent_log_preserves_tool_call_and_result_pair() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("orbit.db"))
            .await
            .unwrap();
        let mut writer = store
            .create(directory.path().display().to_string(), "model".into())
            .await
            .unwrap();
        let id = writer.id().to_owned();
        writer
            .begin("check".into(), "model".into())
            .await
            .unwrap();
        let call = ToolCall::function(
            "call-1",
            "echo",
            serde_json::json!({
                "value": "ok"
            }),
        );
        let model = MockModel::new(vec![
            ChatCompletion {
                id: None,
                model: None,
                message: ChatMessage::assistant_tool_calls(vec![call.clone()]),
                finish_reason: Some("tool_calls".into()),
                usage: None,
            },
            ChatCompletion {
                id: None,
                model: None,
                message: ChatMessage::assistant("done"),
                finish_reason: Some("stop".into()),
                usage: None,
            },
        ]);
        let mut tools = ToolRegistry::new();
        tools.register(ClosureTool::new(
            "echo",
            "Echo a value",
            serde_json::json!({
                "type": "object"
            }),
            |arguments, _| async move {
                Ok(arguments["value"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned())
            },
        ));
        let agent = Agent::new(model, tools, AgentConfig::default())
            .with_execution_journal(writer.execution_journal().unwrap());
        let result = agent
            .run(
                ChatRequest::new(vec![ChatMessage::user("check")]),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        writer
            .finish("completed", vec![], Some(result.history))
            .await
            .unwrap();
        let (_, messages) = store
            .canonical_messages(&id, None, 10)
            .await
            .unwrap();

        assert_eq!(messages.len(), 4);
        assert_eq!(
            messages[1].message,
            ChatMessage::assistant_tool_calls(vec![call])
        );
        assert_eq!(messages[2].message, ChatMessage::tool("call-1", "ok"));
        assert_eq!(messages[3].message, ChatMessage::assistant("done"));
    }
}
