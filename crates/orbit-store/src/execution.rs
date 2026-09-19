use crate::store::Store;
use anyhow::ensure;
use orbit_agent::{
    checkpoint::{CheckpointFuture, ExecutionCheckpoint, ExecutionJournal},
    error::AgentError,
    events::AgentEvent,
};
use rusqlite::params;

pub(crate) struct Journal {
    pub store: Store,
    pub run: i64,
    pub session: String,
    pub pending: std::sync::Mutex<Vec<AgentEvent>>,
}
impl Journal {
    async fn commit(
        &self,
        checkpoint: Option<ExecutionCheckpoint>,
        event: Option<AgentEvent>,
    ) -> Result<(), AgentError> {
        let run = self.run;
        let session = self.session.clone();
        let mut events = std::mem::take(&mut *self.pending.lock().expect("journal lock poisoned"));
        events.extend(event);
        self.store.access(move |conn| {
            let transaction=conn.transaction()?;
            let active:bool=transaction.query_row("SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1 AND session_id=?2 AND status='running')",params![run,session],|r|r.get(0))?;
            ensure!(active,"checkpoint writer no longer owns an active run");
            if let Some(checkpoint)=checkpoint {
                transaction.execute("INSERT INTO execution_checkpoints(run_id,payload) VALUES(?1,?2) ON CONFLICT(run_id) DO UPDATE SET payload=excluded.payload",params![run,serde_json::to_string(&checkpoint)?])?;
            }
            for event in events {
                transaction.execute("INSERT INTO events(run_id,payload,created_at_ms) VALUES(?1,?2,CAST(unixepoch('subsec') * 1000 AS INTEGER))",params![run,serde_json::to_string(&event)?])?;
            }
            transaction.commit()?;
            Ok(())
        }).await.map_err(|e|AgentError::Checkpoint(e.to_string()))
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
