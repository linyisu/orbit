use anyhow::{Context, Result, ensure};
use orbit_model::types::ToolCall;
use rusqlite::{OptionalExtension, params};
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

pub(crate) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS agent_sessions (
        session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
        parent_session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
        role TEXT NOT NULL CHECK(role IN ('explore','review')), task TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS agent_sessions_parent ON agent_sessions(parent_session_id);
    CREATE TABLE IF NOT EXISTS agent_invocations (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        parent_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        parent_run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
        child_session_id TEXT NOT NULL REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
        call TEXT NOT NULL, result TEXT, delivered INTEGER NOT NULL DEFAULT 0 CHECK(delivered IN (0,1))
    );
    CREATE INDEX IF NOT EXISTS agent_invocations_delivery ON agent_invocations(parent_session_id,delivered);
    CREATE TABLE IF NOT EXISTS agent_messages (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
        content TEXT NOT NULL, received INTEGER NOT NULL DEFAULT 0 CHECK(received IN (0,1))
    );
    PRAGMA user_version=21;")?;
    Ok(())
}

impl Store {
    pub async fn subagent_parent(&self, id: &str) -> Result<Option<String>> {
        let id = id.to_owned();
        self.read_access(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT parent_session_id FROM agent_sessions WHERE session_id=?1",
                    [id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten())
        })
        .await
    }

    pub async fn is_subagent(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.read_access(move |conn| {
            Ok(conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_sessions WHERE session_id=?1)",
                [id],
                |r| r.get::<_, bool>(0),
            )?)
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
        let id = self.access(move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            ensure!(!tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_sessions WHERE session_id=?1)", [&parent], |r| r.get::<_,bool>(0))?, "subagents cannot create nested agents");
            let (workspace, project, demo): (String,String,bool) = tx.query_row("SELECT workspace,project_id,demo FROM sessions WHERE id=?1", [&parent], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            let id: String = tx.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?;
            tx.execute("INSERT INTO sessions(id,workspace,project_id,model,demo) VALUES(?1,?2,?3,?4,?5)", params![id,workspace,project,model,demo])?;
            tx.execute("INSERT INTO agent_sessions(session_id,parent_session_id,role,task) VALUES(?1,?2,?3,?4)", params![id,parent,role,task])?;
            tx.commit()?;
            Ok(id)
        }).await?;
        self.acquire(&id).await
    }

    pub async fn subagents(&self, parent: &str) -> Result<Vec<SubagentSession>> {
        let parent = parent.to_owned();
        self.read_access(move |conn| {
            let mut query = conn.prepare("SELECT a.session_id,a.role,s.model,a.task FROM agent_sessions a JOIN sessions s ON s.id=a.session_id WHERE a.parent_session_id=?1 ORDER BY s.rowid")?;
            Ok(query.query_map([parent], |r| Ok(SubagentSession{id:r.get(0)?,role:r.get(1)?,model:r.get(2)?,task:r.get(3)?}))?.collect::<rusqlite::Result<_>>()?)
        }).await
    }

    pub async fn owned_subagent(&self, parent: &str, id: &str) -> Result<SubagentSession> {
        let parent = parent.to_owned();
        let id = id.to_owned();
        self.read_access(move |conn| {
            conn.query_row("SELECT a.session_id,a.role,s.model,a.task FROM agent_sessions a JOIN sessions s ON s.id=a.session_id WHERE a.parent_session_id=?1 AND a.session_id=?2",params![parent,id],|r|Ok(SubagentSession{id:r.get(0)?,role:r.get(1)?,model:r.get(2)?,task:r.get(3)?})).context("subagent does not belong to this conversation")
        }).await
    }

    pub async fn begin_agent_invocation(
        &self,
        parent: &str,
        id: &str,
        call: ToolCall,
    ) -> Result<i64> {
        self.owned_subagent(parent, id).await?;
        let parent = parent.to_owned();
        let id = id.to_owned();
        self.access(move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            // A direct user follow-up can resume a child after the parent has finished.
            // Retain its originating run without inventing a new parent execution.
            let run: i64 = tx.query_row("SELECT id FROM runs WHERE session_id=?1 ORDER BY id DESC LIMIT 1", [&parent], |r|r.get(0)).context("parent agent has no originating run")?;
            tx.execute("INSERT INTO agent_invocations(parent_session_id,parent_run_id,child_session_id,call) VALUES(?1,?2,?3,?4)",params![parent,run,id,serde_json::to_string(&call)?])?;
            let invocation = tx.last_insert_rowid();
            tx.commit()?;
            Ok(invocation)
        }).await
    }

    pub async fn finish_agent_invocation(
        &self,
        parent: &str,
        invocation: i64,
        result: Value,
    ) -> Result<()> {
        let parent = parent.to_owned();
        self.access(move |conn| {
            ensure!(conn.execute("UPDATE agent_invocations SET result=?3 WHERE id=?1 AND parent_session_id=?2 AND result IS NULL", params![invocation,parent,serde_json::to_string(&result)?])? == 1, "subagent invocation is unavailable or already finished");
            Ok(())
        }).await
    }

    pub async fn pending_agent_results(&self, parent: &str) -> Result<Vec<AgentInvocation>> {
        let parent = parent.to_owned();
        self.read_access(move |conn| {
            let mut query = conn.prepare("SELECT i.id,a.session_id,a.role,s.model,a.task,i.call,i.result FROM agent_invocations i JOIN agent_sessions a ON a.session_id=i.child_session_id JOIN sessions s ON s.id=a.session_id WHERE i.parent_session_id=?1 AND i.delivered=0 ORDER BY i.id")?;
            let records = query.query_map([parent], |r| Ok((r.get::<_,i64>(0)?,SubagentSession{id:r.get(1)?,role:r.get(2)?,model:r.get(3)?,task:r.get(4)?},r.get::<_,String>(5)?,r.get::<_,Option<String>>(6)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
            records.into_iter().map(|(id,agent,call,result)| Ok(AgentInvocation{id,agent,call:serde_json::from_str(&call)?,result:result.map(|r|serde_json::from_str(&r)).transpose()?})).collect()
        }).await
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
        self.queue_session_input(id, content).await
    }

    pub async fn queue_session_input(&self, id: &str, content: String) -> Result<i64> {
        ensure!(
            !content.trim().is_empty() && content.len() <= 32_768,
            "message must contain 1..32768 bytes"
        );
        let id = id.to_owned();
        self.access(move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let count: i64 = tx.query_row(
                "SELECT count(*) FROM agent_messages WHERE session_id=?1 AND received=0",
                [&id],
                |r| r.get(0),
            )?;

            ensure!(count < 32, "subagent message queue is full");
            tx.execute(
                "INSERT INTO agent_messages(session_id,content) VALUES(?1,?2)",
                params![id, content],
            )?;
            let message = tx.last_insert_rowid();
            tx.commit()?;
            Ok(message)
        })
        .await
    }

    pub async fn pending_agent_messages(
        &self,
        id: &str,
    ) -> Result<Vec<orbit_agent::inbox::InboxMessage>> {
        let id = id.to_owned();
        self.read_access(move |conn| {
            let mut query = conn.prepare("SELECT id,content FROM agent_messages WHERE session_id=?1 AND received=0 ORDER BY id")?;
            Ok(query.query_map([id], |r|Ok(orbit_agent::inbox::InboxMessage{id:r.get(0)?,content:r.get(1)?}))?.collect::<rusqlite::Result<_>>()?)
        }).await
    }
}

#[cfg(test)]
mod tests {
    use orbit_agent::{checkpoint::ExecutionCheckpoint, events::AgentEvent};
    use orbit_model::types::ChatMessage;

    use super::*;

    #[tokio::test]
    async fn children_are_scoped_hidden_and_deleted_with_closed_parent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("orbit.db")).await.unwrap();
        let parent = store
            .create(
                dir.path().display().to_string(),
                "orbit-model".into(),
                false,
            )
            .await
            .unwrap();
        let other = store
            .create(
                dir.path().display().to_string(),
                "orbit-model".into(),
                false,
            )
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
        assert_eq!(store.subagents(parent.id()).await.unwrap().len(), 1);
        assert!(store.owned_subagent(other.id(), child.id()).await.is_err());
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
        assert!(store.delete_session(&parent_id).await.is_err());
        assert!(store.session(&parent_id).await.is_ok());
        drop(child);
        assert!(store.delete_session(&parent_id).await.unwrap());
        assert!(store.session(&child_id).await.is_err());
        assert_eq!(store.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn completion_and_messages_are_acknowledged_only_with_owned_checkpoints() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("orbit.db")).await.unwrap();
        let mut parent = store
            .create(
                dir.path().display().to_string(),
                "orbit-model".into(),
                false,
            )
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
            "spawn_agent",
            serde_json::json!({"task":"inspect Orbit"}),
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
            in_flight: None,
            attachments: vec![],
        };
        let child_journal = child.execution_journal().unwrap();
        let event = AgentEvent::InputReceived {
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
        let result = serde_json::json!({"agent_id":child.id(),"invocation_id":invocation,"status":"completed","summary":"done"});
        child
            .finish_subagent(
                "completed",
                Some(vec![ChatMessage::assistant("done")]),
                parent.id().into(),
                invocation,
                result.clone(),
            )
            .await
            .unwrap();
        let pending = store.pending_agent_results(parent.id()).await.unwrap();
        assert_eq!(pending[0].result, Some(result.clone()));
        let finished = AgentEvent::ToolFinished {
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
