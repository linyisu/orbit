use orbit_agent::events::AgentEvent;
use orbit_agent::tool::FileChange;
use orbit_model::types::ChatMessage;
use serde::{Deserialize, Serialize};

pub use orbit_protocol::Session;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    #[serde(default)]
    pub images: Vec<orbit_protocol::MessageImage>,
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
    #[serde(default)]
    pub recovery: Option<RecoveryInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryInfo {
    pub turns: usize,
    pub tool_calls: usize,
    pub unresolved_tools: usize,
}

/// A successfully committed workspace edit reconstructed from the durable
/// event stream. The source text is held in Orbit's private central database,
/// never in a project-side checkpoint file.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceChange {
    pub run_id: i64,
    pub run_status: String,
    pub call_id: String,
    pub tool: String,
    pub change: FileChange,
    /// A local `/undo` restored this exact checkpoint. The original audit
    /// receipt remains visible, but it is no longer eligible for another undo.
    pub undone: bool,
}

/// An actual completed sandbox command reconstructed from the event ledger.
/// It is evidence of execution, not a claim that the model made in prose.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationRecord {
    pub run_id: i64,
    pub run_status: String,
    pub command: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub cancelled: bool,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub evidence: Option<orbit_runtime::verification::VerificationEvidence>,
}

/// Project-scoped work that survives directory moves and conversation changes.
pub use orbit_protocol::project::{
    MemoryCategory, ProjectPlugin, ProjectTask, SessionMemory, TaskStatus,
};
