use orbit_agent::events::AgentEvent;
use orbit_model::types::ChatMessage;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub workspace: String,
    pub project_id: String,
    pub model: String,
    pub demo: bool,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub prompt: String,
    pub status: String,
    pub events: Vec<AgentEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub session: Session,
    pub history: Vec<ChatMessage>,
    pub runs: Vec<Run>,
}
