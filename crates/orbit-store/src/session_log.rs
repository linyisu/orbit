//! Append-only conversation messages, separate from the mutable model projection.
use anyhow::{Result, ensure};
use orbit_model::types::ChatMessage;
use sha2::{Digest, Sha256};

use crate::store::Store;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalMessage {
    pub id: i64,
    pub run_id: i64,
    pub message: ChatMessage,
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
            (1..=1000).contains(&limit),
            "message page must contain 1–1000 entries"
        );
        let session_id = session_id.to_owned();
        self.access(move |conn| {
            conn.query_row("SELECT id FROM sessions WHERE id=?1", [&session_id], |_| {
                Ok(())
            })?;
            let legacy_gap: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM session_log_gaps WHERE session_id=?1)",
                [&session_id],
                |row| row.get(0),
            )?;
            let mut stmt = conn.prepare(
                "SELECT id,run_id,payload,sha256 FROM session_messages
                 WHERE session_id=?1 AND id>?2 ORDER BY id LIMIT ?3",
            )?;
            let rows = stmt.query_map(
                rusqlite::params![session_id, after_id.unwrap_or(0), limit as i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )?;
            let mut messages = Vec::new();
            for row in rows {
                let (id, run_id, payload, expected) = row?;
                let actual = Sha256::digest(payload.as_bytes())
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                ensure!(
                    actual == expected,
                    "canonical message {id} failed integrity check"
                );
                messages.push(CanonicalMessage {
                    id,
                    run_id,
                    message: serde_json::from_str(&payload)?,
                });
            }
            Ok((legacy_gap, messages))
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_agent::{
        agent::{Agent, AgentConfig},
        checkpoint::ExecutionCheckpoint,
        model::MockModel,
        tool::{ClosureTool, ToolRegistry},
    };
    use orbit_model::types::{ChatCompletion, ChatRequest, ToolCall};
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn canonical_messages_survive_projection_replacement_and_resume() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("orbit.db");
        let store = Store::open(&path).await.unwrap();
        let mut writer = store
            .create(
                directory.path().display().to_string(),
                "model".into(),
                false,
            )
            .await
            .unwrap();
        let id = writer.id().to_owned();
        writer.begin("hello".into(), "model".into()).await.unwrap();
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
                    in_flight: None,
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
                    in_flight: None,
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
        let (legacy_gap, messages) = reopened.canonical_messages(&id, None, 10).await.unwrap();
        assert!(!legacy_gap);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].message, user);
        assert_eq!(messages[1].message, reply);
        assert_eq!(
            reopened.load(&id).await.unwrap().history,
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
            .create(
                directory.path().display().to_string(),
                "model".into(),
                false,
            )
            .await
            .unwrap();
        let id = writer.id().to_owned();
        writer.begin("hello".into(), "model".into()).await.unwrap();
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

        let (legacy_gap, messages) = store.canonical_messages(&id, None, 10).await.unwrap();
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
        assert!(store.canonical_messages("missing", None, 10).await.is_err());
    }

    #[tokio::test]
    async fn agent_log_preserves_tool_call_and_result_pair() {
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(directory.path().join("orbit.db"))
            .await
            .unwrap();
        let mut writer = store
            .create(
                directory.path().display().to_string(),
                "model".into(),
                false,
            )
            .await
            .unwrap();
        let id = writer.id().to_owned();
        writer.begin("check".into(), "model".into()).await.unwrap();
        let call = ToolCall::function("call-1", "echo", serde_json::json!({"value":"ok"}));
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
            serde_json::json!({"type":"object"}),
            |arguments, _| async move {
                Ok(arguments["value"].as_str().unwrap_or_default().to_owned())
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
        let (_, messages) = store.canonical_messages(&id, None, 10).await.unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(
            messages[1].message,
            ChatMessage::assistant_tool_calls(vec![call])
        );
        assert_eq!(messages[2].message, ChatMessage::tool("call-1", "ok"));
        assert_eq!(messages[3].message, ChatMessage::assistant("done"));
    }
}
