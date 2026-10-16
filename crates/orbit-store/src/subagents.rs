use crate::entities::*;
use anyhow::{Context, Result, ensure};
use orbit_model::types::ToolCall;
use sea_orm::sea_query::Expr;
use sea_orm::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::store::{SessionWriter, Store};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubagentSession {
    pub id: String,
    pub role: String,
    pub model: String,
    pub task: String,
}

#[derive(Clone, Debug)]
pub struct AgentInvocation {
    pub id: i64,
    pub agent: SubagentSession,
    pub call: ToolCall,
    pub result: Option<Value>,
}

#[derive(FromQueryResult)]
struct PendingInvocationRow {
    id: i64,
    session_id: String,
    role: String,
    model: String,
    task: String,
    call: String,
    result: Option<String>,
}

fn agent_query() -> Select<agent_sessions::Entity> {
    agent_sessions::Entity::find()
        .join(JoinType::InnerJoin, agent_sessions::Relation::Session.def())
}

fn agent_columns(query: Select<agent_sessions::Entity>) -> Select<agent_sessions::Entity> {
    query
        .select_only()
        .column(agent_sessions::Column::SessionId)
        .column(agent_sessions::Column::Role)
        .column(sessions::Column::Model)
        .column(agent_sessions::Column::Task)
}

fn agent_model((id, role, model, task): (String, String, String, String)) -> SubagentSession {
    SubagentSession {
        id,
        role,
        model,
        task,
    }
}

impl Store {
    pub async fn subagent_parent(&self, id: &str) -> Result<Option<String>> {
        let id = id.to_owned();
        self.read_access(move |conn| async move {
            Ok(agent_sessions::Entity::find_by_id(id)
                .one(&conn)
                .await?
                .and_then(|row| row.parent_session_id))
        })
        .await
    }

    pub async fn is_subagent(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.read_access(move |conn| async move {
            Ok(agent_sessions::Entity::find_by_id(id)
                .count(&conn)
                .await?
                > 0)
        })
        .await
    }

    pub async fn create_subagent(
        &self,
        parent: &str,
        model: String,
        role: String,
        task: String,
    ) -> Result<SessionWriter> {
        ensure!(
            matches!(role.as_str(), "explore" | "review" | "develop"),
            "invalid subagent role"
        );
        let parent = parent.to_owned();
        let id = self
            .write_access(move |conn| async move {
                let tx = conn.begin().await?;
                ensure!(
                    agent_sessions::Entity::find_by_id(&parent)
                        .one(&tx)
                        .await?
                        .is_none(),
                    "subagents cannot create nested agents"
                );
                let row = sessions::Entity::find_by_id(&parent)
                    .one(&tx)
                    .await?
                    .context("parent session not found")?;
                let id = crate::database::new_id()?;
                sessions::ActiveModel {
                    id: Set(id.clone()),
                    workspace: Set(row.workspace),
                    project_id: Set(row.project_id),
                    model: Set(model),
                    ..Default::default()
                }
                .insert(&tx)
                .await?;
                agent_sessions::ActiveModel {
                    session_id: Set(id.clone()),
                    parent_session_id: Set(Some(parent)),
                    role: Set(role),
                    task: Set(task),
                }
                .insert(&tx)
                .await?;
                tx.commit().await?;
                Ok(id)
            })
            .await?;
        self.acquire(&id).await
    }

    pub async fn subagents(&self, parent: &str) -> Result<Vec<SubagentSession>> {
        let parent = parent.to_owned();
        self.read_access(move |conn| async move {
            Ok(agent_columns(agent_query())
                .filter(agent_sessions::Column::ParentSessionId.eq(parent))
                .order_by_asc(Expr::col((
                    sessions::Entity,
                    sea_query::Alias::new("rowid"),
                )))
                .into_tuple()
                .all(&conn)
                .await?
                .into_iter()
                .map(agent_model)
                .collect())
        })
        .await
    }

    pub async fn owned_subagent(&self, parent: &str, id: &str) -> Result<SubagentSession> {
        let parent = parent.to_owned();
        let id = id.to_owned();
        self.read_access(move |conn| async move {
            Ok(agent_model(
                agent_columns(agent_query())
                    .filter(agent_sessions::Column::ParentSessionId.eq(parent))
                    .filter(agent_sessions::Column::SessionId.eq(id))
                    .into_tuple()
                    .one(&conn)
                    .await?
                    .context("subagent does not belong to this conversation")?,
            ))
        })
        .await
    }

    pub async fn begin_agent_invocation(
        &self,
        parent: &str,
        id: &str,
        call: ToolCall,
    ) -> Result<i64> {
        let parent = parent.to_owned();
        let id = id.to_owned();
        self.write_access(move |conn| async move {
            let tx = conn.begin().await?;
            ensure!(
                agent_sessions::Entity::find_by_id(&id)
                    .filter(agent_sessions::Column::ParentSessionId.eq(&parent))
                    .one(&tx)
                    .await?
                    .is_some(),
                "subagent does not belong to this conversation"
            );
            let run = runs::Entity::find()
                .filter(runs::Column::SessionId.eq(&parent))
                .order_by_desc(runs::Column::Id)
                .one(&tx)
                .await?
                .context("parent agent has no originating run")?;
            let row = agent_invocations::ActiveModel {
                parent_session_id: Set(parent),
                parent_run_id: Set(run.id),
                child_session_id: Set(id),
                call: Set(serde_json::to_string(&call)?),
                ..Default::default()
            }
            .insert(&tx)
            .await?;
            tx.commit().await?;
            Ok(row.id)
        })
        .await
    }

    pub async fn pending_agent_results(&self, parent: &str) -> Result<Vec<AgentInvocation>> {
        let parent = parent.to_owned();
        self.read_access(move |conn| async move {
            let rows = agent_invocations::Entity::find()
                .select_only()
                .column(agent_invocations::Column::Id)
                .column(agent_sessions::Column::SessionId)
                .column(agent_sessions::Column::Role)
                .column(sessions::Column::Model)
                .column(agent_sessions::Column::Task)
                .column(agent_invocations::Column::Call)
                .column(agent_invocations::Column::Result)
                .join(
                    JoinType::InnerJoin,
                    agent_invocations::Relation::ChildSession.def(),
                )
                .join(JoinType::InnerJoin, agent_sessions::Relation::Session.def())
                .filter(agent_invocations::Column::ParentSessionId.eq(parent))
                .filter(agent_invocations::Column::Delivered.eq(false))
                .order_by_asc(agent_invocations::Column::Id)
                .into_model::<PendingInvocationRow>()
                .all(&conn)
                .await?;
            rows.into_iter()
                .map(|row| {
                    Ok(AgentInvocation {
                        id: row.id,
                        agent: agent_model((row.session_id, row.role, row.model, row.task)),
                        call: serde_json::from_str(&row.call)?,
                        result: row
                            .result
                            .map(|r| serde_json::from_str(&r))
                            .transpose()?,
                    })
                })
                .collect()
        })
        .await
    }

    pub async fn queue_agent_message(
        &self,
        parent: &str,
        id: &str,
        content: String,
    ) -> Result<i64> {
        ensure!(
            !content.trim().is_empty() && content.len() <= 32_768,
            "message must contain 1..32768 bytes"
        );
        self.owned_subagent(parent, id).await?;
        self.queue_session_input(id, content)
            .await
    }

    pub async fn queue_session_input(&self, id: &str, content: String) -> Result<i64> {
        self.queue_session_input_with_attachments(id, content, vec![])
            .await
    }

    pub async fn queue_session_input_with_attachments(
        &self,
        id: &str,
        content: String,
        attachments: Vec<orbit_model::types::Attachment>,
    ) -> Result<i64> {
        ensure!(
            (!content.trim().is_empty() || !attachments.is_empty())
                && content.len() <= 1_024 * 1_024,
            "message must contain 1..1048576 bytes"
        );
        let id = id.to_owned();
        let database = self.path().to_owned();
        self.write_access(move |conn| async move {
            let tx = conn.begin().await?;
            crate::attachments::register(&tx, &database, &id, &attachments).await?;
            let count = agent_messages::Entity::find()
                .filter(agent_messages::Column::SessionId.eq(&id))
                .filter(agent_messages::Column::Received.eq(false))
                .count(&tx)
                .await?;
            ensure!(count < 32, "session message queue is full");
            let row = agent_messages::ActiveModel {
                session_id: Set(id),
                content: Set(content),
                attachments: Set(serde_json::to_string(&attachments)?),
                ..Default::default()
            }
            .insert(&tx)
            .await?;
            tx.commit().await?;
            Ok(row.id)
        })
        .await
    }

    pub async fn pending_agent_messages(
        &self,
        id: &str,
    ) -> Result<Vec<orbit_agent::inbox::InboxMessage>> {
        let id = id.to_owned();
        self.read_access(move |conn| async move {
            agent_messages::Entity::find()
                .filter(agent_messages::Column::SessionId.eq(id))
                .filter(agent_messages::Column::Received.eq(false))
                .order_by_asc(agent_messages::Column::Id)
                .all(&conn)
                .await?
                .into_iter()
                .map(|row| -> Result<orbit_agent::inbox::InboxMessage> {
                    Ok(orbit_agent::inbox::InboxMessage {
                        attachments: serde_json::from_str(&row.attachments)?,
                        id: row.id,
                        content: row.content,
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use orbit_agent::{checkpoint::ExecutionCheckpoint, events::AgentEvent};
    use orbit_model::types::ChatMessage;

    use super::*;

    #[tokio::test]
    async fn migration_preserves_child_inputs_and_allows_root_session_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orbit.db");
        use crate::tests::support::{self, insert};
        let connection = support::connect(&path).await;
        support::legacy_schema(&connection, 26).await;
        let parent_id = "a".repeat(32);
        let child_id = "b".repeat(32);
        insert(&connection, "projects", &[("id", "project".into())])
            .await
            .unwrap();
        for id in [&parent_id, &child_id] {
            insert(
                &connection,
                "sessions",
                &[
                    ("id", id.clone().into()),
                    ("project_id", "project".into()),
                    ("workspace", dir.path().to_str().unwrap().into()),
                    ("model", "model".into()),
                    ("demo", false.into()),
                ],
            )
            .await
            .unwrap();
        }
        insert(
            &connection,
            "agent_sessions",
            &[
                ("session_id", child_id.clone().into()),
                ("parent_session_id", parent_id.clone().into()),
                ("role", "review".into()),
                ("task", "review".into()),
            ],
        )
        .await
        .unwrap();
        let child_input = insert(
            &connection,
            "agent_messages",
            &[
                ("session_id", child_id.clone().into()),
                ("content", "pending child".into()),
            ],
        )
        .await
        .unwrap()
        .last_insert_id() as i64;
        connection.close().await.unwrap();
        let store = Store::open(&path).await.unwrap();
        let pending = store
            .pending_agent_messages(&child_id)
            .await
            .unwrap();
        assert_eq!(pending[0].id, child_input);
        assert_eq!(pending[0].content, "pending child");
        let input = store
            .queue_session_input(&parent_id, "steer root".into())
            .await
            .unwrap();
        assert!(input > child_input);
        assert_eq!(
            store
                .pending_agent_messages(&parent_id)
                .await
                .unwrap()[0]
                .content,
            "steer root"
        );
        assert!(
            store
                .queue_session_input("missing", "invalid".into())
                .await
                .is_err()
        );
        assert!(
            store
                .queue_agent_message(&child_id, &parent_id, "not an owned child".into())
                .await
                .is_err()
        );
        store
            .delete_session(&parent_id)
            .await
            .unwrap();
        assert!(
            store
                .pending_agent_messages(&parent_id)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn children_are_scoped_hidden_and_deleted_with_closed_parent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("orbit.db"))
            .await
            .unwrap();
        let parent = store
            .create(dir.path().display().to_string(), "orbit-model".into())
            .await
            .unwrap();
        let other = store
            .create(dir.path().display().to_string(), "orbit-model".into())
            .await
            .unwrap();
        let child = store
            .create_subagent(
                parent.id(),
                "orbit-child-model".into(),
                "review".into(),
                "inspect Orbit".into(),
            )
            .await
            .unwrap();
        let child_id = child.id().to_owned();
        let parent_id = parent.id().to_owned();
        assert_eq!(store.list().await.unwrap().len(), 2);
        assert_eq!(
            store
                .subagents(parent.id())
                .await
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .owned_subagent(other.id(), child.id())
                .await
                .is_err()
        );
        assert!(
            store
                .queue_agent_message(other.id(), child.id(), "foreign".into())
                .await
                .is_err()
        );
        assert!(
            store
                .create_subagent(
                    child.id(),
                    "orbit-model".into(),
                    "explore".into(),
                    "nested".into()
                )
                .await
                .is_err()
        );
        drop(parent);
        assert!(
            store
                .delete_session(&parent_id)
                .await
                .is_err()
        );
        assert!(store.session(&parent_id).await.is_ok());
        drop(child);
        assert!(
            store
                .delete_session(&parent_id)
                .await
                .unwrap()
        );
        assert!(store.session(&child_id).await.is_err());
        assert_eq!(store.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn completion_and_messages_are_acknowledged_only_with_owned_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("orbit.db"))
            .await
            .unwrap();
        let mut parent = store
            .create(dir.path().display().to_string(), "orbit-model".into())
            .await
            .unwrap();
        let mut child = store
            .create_subagent(
                parent.id(),
                "orbit-model".into(),
                "explore".into(),
                "inspect Orbit".into(),
            )
            .await
            .unwrap();

        parent
            .begin("root".into(), "orbit-model".into())
            .await
            .unwrap();
        child
            .begin("child".into(), "orbit-model".into())
            .await
            .unwrap();
        let call = ToolCall::function(
            "child",
            "SpawnAgent",
            serde_json::json!({
                "task": "inspect Orbit"
            }),
        );
        let invocation = store
            .begin_agent_invocation(parent.id(), child.id(), call.clone())
            .await
            .unwrap();
        let message = store
            .queue_agent_message(parent.id(), child.id(), "检查 UTF-8".into())
            .await
            .unwrap();
        let input = ChatMessage::user("检查 UTF-8");
        let checkpoint = ExecutionCheckpoint {
            history: vec![input.clone()],
            turns: 0,
            tool_calls: 0,
            in_flight: Vec::new(),
            attachments: vec![],
        };
        let child_journal = child.execution_journal().unwrap();
        let event = AgentEvent::InputReceived {
            attachments: vec![],
            message_id: message,
            content: "检查 UTF-8".into(),
        };
        parent
            .execution_journal()
            .unwrap()
            .save(checkpoint.clone(), Some(event.clone()))
            .await
            .unwrap();
        assert_eq!(
            store
                .pending_agent_messages(child.id())
                .await
                .unwrap()
                .len(),
            1
        );
        child_journal.record_message(input);
        child_journal
            .save(checkpoint.clone(), Some(event))
            .await
            .unwrap();
        assert!(
            store
                .pending_agent_messages(child.id())
                .await
                .unwrap()
                .is_empty()
        );
        let result = serde_json::json!({
            "agent_id": child.id(),
            "invocation_id": invocation,
            "status": "completed",
            "summary": "done"
        });
        child
            .finish_subagent(
                &crate::types::RunCompletion::Completed {
                    history: vec![ChatMessage::assistant("done")],
                },
                parent.id().into(),
                invocation,
                result.clone(),
            )
            .await
            .unwrap();
        let pending = store
            .pending_agent_results(parent.id())
            .await
            .unwrap();
        assert_eq!(pending[0].result, Some(result.clone()));
        let finished = AgentEvent::ToolFinished {
            outcome: None,
            files: None,
            call,
            content: result.to_string(),
            changes: vec![],
        };

        parent
            .execution_journal()
            .unwrap()
            .save(checkpoint, Some(finished))
            .await
            .unwrap();
        assert!(
            store
                .pending_agent_results(parent.id())
                .await
                .unwrap()
                .is_empty()
        );
    }
}
