use crate::store::Store;
use anyhow::ensure;
use orbit_agent::{
    checkpoint::{CheckpointFuture, ExecutionCheckpoint, ExecutionJournal},
    error::AgentError,
    events::AgentEvent,
};
use orbit_model::types::ChatMessage;
use rusqlite::params;
use sha2::{Digest, Sha256};
use std::sync::{Mutex, MutexGuard, PoisonError};

fn journal_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) struct Journal {
    pub store: Store,
    pub run: i64,
    pub session: String,
    pub commit_lock: tokio::sync::Mutex<()>,
    pub pending: std::sync::Mutex<Vec<AgentEvent>>,
    pub pending_messages: std::sync::Mutex<Vec<ChatMessage>>,
}

impl Journal {
    async fn commit(
        &self,
        checkpoint: Option<ExecutionCheckpoint>,
        event: Option<AgentEvent>,
    ) -> Result<(), AgentError> {
        let _guard = self.commit_lock.lock().await;
        let run = self.run;
        let session = self.session.clone();
        // Keep queued facts until the SQLite transaction commits. A failed
        // checkpoint must not silently discard the only copy of new messages.
        let events = journal_lock(&self.pending).clone();
        let messages = journal_lock(&self.pending_messages).clone();
        let event_count = events.len();
        let message_count = messages.len();
        if checkpoint.is_none() && event.is_none() && event_count == 0 && message_count == 0 {
            return Ok(());
        }
        let mut events = events;
        events.extend(event);
        self.store.access(move |conn| {
            let transaction=conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let active:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND session_id=?2 AND status='running')",params![run,session],|r|r.get(0))?;
            ensure!(active,"checkpoint writer no longer owns an active run");

            if let Some(checkpoint)=checkpoint {
                transaction.execute("INSERT INTO execution_checkpoints(run_id,payload) VALUES(?1,?2) ON CONFLICT(run_id) DO UPDATE SET payload=excluded.payload",params![run,serde_json::to_string(&checkpoint)?])?;
            }

            if !messages.is_empty() {
                let first_ordinal: i64 = transaction.query_row(
                    "SELECT COALESCE(MAX(ordinal)+1,0) FROM session_messages WHERE run_id=?1",
                    [run],
                    |row| row.get(0),
                )?;

                for (ordinal, message) in (first_ordinal..).zip(messages) {
                    let payload = serde_json::to_string(&message)?;
                    let sha256 = Sha256::digest(payload.as_bytes())
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>();
                    transaction.execute(
                        "INSERT INTO session_messages(session_id,run_id,ordinal,payload,sha256) VALUES(?1,?2,?3,?4,?5)",
                        params![session,run,ordinal,payload,sha256],
                    )?;
                }
            }

            for event in events {
                match &event {
                    AgentEvent::InputReceived { message_id, .. } => {
                        transaction.execute("UPDATE agent_messages SET received=1 WHERE id=?1 AND session_id=?2",params![message_id,session])?;
                    }
                    AgentEvent::ToolFinished { call, content, .. } if matches!(call.function.name.as_str(), "spawn_agent" | "send_agent_message") => {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(content)
                            && let Some(id) = value.get("invocation_id").and_then(|id|id.as_i64()) {
                            transaction.execute("UPDATE agent_invocations SET delivered=1 WHERE id=?1 AND parent_session_id=?2 AND result IS NOT NULL",params![id,session])?;
                        }
                    }
                    _ => {}
                }

                transaction.execute("INSERT INTO events(run_id,payload,created_at_ms) VALUES(?1,?2,CAST(unixepoch('subsec') * 1000 AS INTEGER))",params![run,serde_json::to_string(&event)?])?;
            }

            transaction.commit()?;

            Ok(())
        }).await.map_err(|e|AgentError::Checkpoint(e.to_string()))?;
        journal_lock(&self.pending).drain(..event_count);
        journal_lock(&self.pending_messages).drain(..message_count);

        Ok(())
    }
}

impl ExecutionJournal for Journal {
    fn record(&self, event: AgentEvent) {
        // These receipts were already committed together with their context
        // before emission. All other events retain their original stream order.
        if !matches!(
            event,
            AgentEvent::ToolStarted { .. }
                | AgentEvent::ToolBackgrounded { .. }
                | AgentEvent::ToolFinished { .. }
                | AgentEvent::ToolFailed { .. }
                | AgentEvent::InputReceived { .. }
        ) {
            journal_lock(&self.pending).push(event);
        }
    }

    fn record_message(&self, message: ChatMessage) {
        journal_lock(&self.pending_messages).push(message);
    }

    fn flush(&self) -> CheckpointFuture<'_> {
        Box::pin(self.commit(None, None))
    }

    fn save<'a>(
        &'a self,
        checkpoint: ExecutionCheckpoint,
        event: Option<AgentEvent>,
    ) -> CheckpointFuture<'a> {
        Box::pin(self.commit(Some(checkpoint), event))
    }
}
