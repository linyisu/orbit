//! Host-acknowledged recovery checkpoints. A failed write stops execution before
//! the next side effect; an event channel alone is not a durability guarantee.
use crate::{error::AgentError, events::AgentEvent};
use orbit_model::types::{Attachment, ChatMessage};
use std::{future::Future, pin::Pin};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ExecutionCheckpoint {
    pub history: Vec<ChatMessage>,
    pub turns: usize,
    pub tool_calls: usize,
    pub in_flight: Option<String>,
    pub attachments: Vec<Attachment>,
}
impl ExecutionCheckpoint {
    /// Reconcile an interrupted batch without replaying any operation. An
    /// in-flight operation may have committed before its receipt was saved.
    pub fn recovered_history(&self) -> (Vec<ChatMessage>, usize) {
        let mut history = self.history.clone();
        let mut unresolved = 0;
        if let Some(index) = history
            .iter()
            .rposition(|m| matches!(m, ChatMessage::Assistant { .. }))
        {
            let ChatMessage::Assistant { tool_calls, .. } = &history[index] else {
                unreachable!()
            };
            let missing: Vec<_> = tool_calls
                .iter()
                .filter(|call| {
                    !history[index+1..].iter().any(|m| {
                matches!(m, ChatMessage::Tool { tool_call_id, .. } if tool_call_id == &call.id)
            })
                })
                .cloned()
                .collect();
            for call in missing {
                unresolved += 1;
                let message = if self.in_flight.as_deref() == Some(&call.id) {
                    "Execution interrupted while this tool was in flight. Its side effects are UNKNOWN. Do not automatically repeat it. Inspect files or external state and reconcile with the user's task before deciding any further action."
                } else {
                    "Execution interrupted before this tool started. It was NOT executed. Re-evaluate whether it is still needed; this message is a recovery status, not a tool result."
                };
                history.push(ChatMessage::tool(call.id, message));
            }
        }
        if !self.attachments.is_empty() {
            history.push(ChatMessage::user_with_attachments(
                "Recovered attachments returned by completed tools.",
                self.attachments.clone(),
            ));
        }
        (history, unresolved)
    }
}

pub type CheckpointFuture<'a> = Pin<Box<dyn Future<Output = Result<(), AgentError>> + Send + 'a>>;
pub trait ExecutionJournal: Send + Sync {
    fn record(&self, event: AgentEvent);
    fn flush(&self) -> CheckpointFuture<'_>;
    /// Atomically save the effective context and an optional tool audit event.
    /// The caller waits for success before starting another tool or model call.
    fn save<'a>(
        &'a self,
        checkpoint: ExecutionCheckpoint,
        event: Option<AgentEvent>,
    ) -> CheckpointFuture<'a>;
}
