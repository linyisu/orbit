use orbit_agent::events::AgentEvent;
use orbit_agent::tool::FileChange;
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

/// A durable, user-visible unit of project work. Tasks live on the stable
/// project identity rather than a single conversation so a resumed or new
/// session can continue the same plan after a directory rename.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectTask {
    pub id: i64,
    pub project_id: String,
    pub parent_id: Option<i64>,
    pub title: String,
    pub details: String,
    pub status: TaskStatus,
    pub priority: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Blocked,
    Completed,
    Cancelled,
}

impl TaskStatus {
    pub const ALL: [Self; 5] = [
        Self::Pending,
        Self::InProgress,
        Self::Blocked,
        Self::Completed,
        Self::Cancelled,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|status| status.as_str() == value)
    }
}

/// A concise, project-scoped fact deliberately saved for future sessions.
/// This is separate from conversation compression: memories are never an
/// implicit replacement for historical messages.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectMemory {
    pub id: i64,
    pub project_id: String,
    pub category: MemoryCategory,
    pub title: String,
    pub content: String,
    pub source_session_id: Option<String>,
    pub active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    Constraint,
    Decision,
    Fact,
    Preference,
}

impl MemoryCategory {
    pub const ALL: [Self; 4] = [
        Self::Constraint,
        Self::Decision,
        Self::Fact,
        Self::Preference,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Constraint => "constraint",
            Self::Decision => "decision",
            Self::Fact => "fact",
            Self::Preference => "preference",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|category| category.as_str() == value)
    }
}
