use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskSnapshot {
    pub tasks: Vec<ProjectTask>,
    /// The task changed by this request; list requests have no changed task.
    pub changed: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectTask {
    pub id: i64,
    pub project_id: String,
    pub parent_id: Option<i64>,
    pub title: String,
    pub details: String,
    pub status: TaskStatus,
    pub priority: i64,
    #[serde(default)]
    pub owner_session_id: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<i64>,
    /// Dependencies that are currently not completed.
    #[serde(default)]
    pub blocked_by: Vec<i64>,
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

/// A concise, session-scoped fact deliberately saved for the current
/// conversation. This is separate from conversation compression: memories
/// are never an implicit replacement for historical messages.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMemory {
    pub id: i64,
    pub session_id: String,
    pub category: MemoryCategory,
    pub title: String,
    pub content: String,
    pub active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Enabled project package with a pinned revision; files stay in private storage.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectPlugin {
    pub project_id: String,
    pub marketplace_source: String,
    pub plugin_name: String,
    pub package_source: String,
    pub version: Option<String>,
    #[serde(default)]
    pub marketplace_revision: Option<String>,
    pub enabled_at: i64,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum TaskAction {
    List {
        include_completed: bool,
    },
    Add {
        title: String,
    },
    Create {
        title: String,
        details: String,
        priority: i64,
        parent_id: Option<i64>,
        depends_on: Vec<i64>,
    },
    Update {
        id: i64,
        title: Option<String>,
        details: Option<String>,
        status: Option<TaskStatus>,
        priority: Option<i64>,
        /// None leaves dependencies unchanged; Some([]) clears them.
        depends_on: Option<Vec<i64>>,
    },
    SetStatus {
        id: i64,
        status: TaskStatus,
    },
    Release {
        id: i64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MemoryAction {
    List {
        include_inactive: bool,
    },
    Add {
        category: MemoryCategory,
        content: String,
    },
    Search {
        query: String,
    },
    Forget {
        id: i64,
    },
}
