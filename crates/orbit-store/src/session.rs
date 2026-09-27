use orbit_agent::tool::FileChange;
use orbit_model::types::ChatMessage;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub workspace: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub title: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub id: i64,
    pub status: String,
    pub started_at_ms: i64,
    pub finished_at_ms: Option<i64>,
}

/// A committed Orbit write, stored separately from the model-visible tool result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceChange {
    pub id: i64,
    pub run_id: i64,
    pub run_status: String,
    pub call_id: String,
    pub tool: String,
    pub change: FileChange,
    pub undone: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalMessage {
    pub id: i64,
    pub run_id: i64,
    pub message: ChatMessage,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub session: Session,
    pub runs: Vec<Run>,
    pub messages: Vec<CanonicalMessage>,
}

impl Snapshot {
    /// Reconstruct a provider-safe request without changing the canonical log.
    /// An interrupted tool batch receives an explicit unknown-outcome result.
    pub fn projected_history(&self) -> Vec<ChatMessage> {
        let mut history = Vec::with_capacity(self.messages.len());
        let mut pending = Vec::new();

        for record in &self.messages {
            match &record.message {
                ChatMessage::Assistant { tool_calls, .. } => {
                    close_pending(&mut history, &mut pending);
                    pending.extend(tool_calls.iter().map(|call| call.id.clone()));
                }
                ChatMessage::Tool { tool_call_id, .. } => {
                    pending.retain(|id| id != tool_call_id);
                }
                _ => close_pending(&mut history, &mut pending),
            }

            history.push(record.message.clone());
        }

        close_pending(&mut history, &mut pending);
        history
    }
}

fn close_pending(history: &mut Vec<ChatMessage>, pending: &mut Vec<String>) {
    for id in pending.drain(..) {
        history.push(ChatMessage::tool(
            id,
            "tool execution was interrupted; its outcome is unknown. Inspect the workspace before retrying any operation.",
        ));
    }
}
