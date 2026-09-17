use orbit_agent::events::AgentEvent;
use orbit_model::types::ChatMessage;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub workspace: String,
    pub project_id: String,
    pub model: String,
    /// The selected reasoning setting, stored independently from provider
    /// configuration so a resumed conversation keeps its own preference.
    pub reasoning_effort: Option<String>,
    pub demo: bool,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub prompt: String,
    pub status: String,
    /// Wall-clock duration measured by the writer, in milliseconds. Runs saved
    /// before this column existed retain `None` rather than a fabricated value.
    pub elapsed_ms: Option<u64>,
    pub events: Vec<AgentEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub session: Session,
    pub history: Vec<ChatMessage>,
    pub runs: Vec<Run>,
}
