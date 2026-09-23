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

pub(crate) struct Journal {
    pub store: Store,
    pub run: i64,
    pub session: String,
    pub pending: std::sync::Mutex<Vec<AgentEvent>>,
    pub pending_messages: std::sync::Mutex<Vec<ChatMessage>>,
}
impl Journal {
    async fn commit(
        &self,
        checkpoint: Option<ExecutionCheckpoint>,
        event: Option<AgentEvent>,
    ) -> Result<(), AgentError> {
        let run = self.run;
        let session = self.session.clone();
        // Keep queued facts until the SQLite transaction commits. A failed
        // checkpoint must not silently discard the only copy of new messages.
        let events = self.pending.lock().expect("journal lock poisoned").clone();
        let messages = self
            .pending_messages
            .lock()
            .expect("message journal lock poisoned")
            .clone();
        let event_count = events.len();
        let message_count = messages.len();
        let mut events = events;
        events.extend(event);
        self.store.access(move |conn| {
            let transaction=conn.transaction()?;
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
                transaction.execute("INSERT INTO events(run_id,payload,created_at_ms) VALUES(?1,?2,CAST(unixepoch('subsec') * 1000 AS INTEGER))",params![run,serde_json::to_string(&event)?])?;
            }
            transaction.commit()?;
            Ok(())
        }).await.map_err(|e|AgentError::Checkpoint(e.to_string()))?;
        self.pending
            .lock()
            .expect("journal lock poisoned")
            .drain(..event_count);
        self.pending_messages
            .lock()
            .expect("message journal lock poisoned")
            .drain(..message_count);
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
                | AgentEvent::ToolFinished { .. }
                | AgentEvent::ToolFailed { .. }
        ) {
            self.pending
                .lock()
                .expect("journal lock poisoned")
                .push(event);
        }
    }
    fn record_message(&self, message: ChatMessage) {
        self.pending_messages
            .lock()
            .expect("message journal lock poisoned")
            .push(message);
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
