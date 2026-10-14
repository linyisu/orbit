//! Session planning is independent of approval preferences.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkMode {
    Plan,
    #[default]
    Build,
}

impl WorkMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Build => "build",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Draft,
    Review,
    Approved,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub workspace: String,
    #[serde(default)]
    pub files: std::collections::BTreeMap<String, Option<String>>,
    #[serde(default)]
    pub task_ids: Vec<i64>,
    pub revision: u64,
    pub title: String,
    pub markdown: String,
    pub steps: Vec<String>,
    pub status: Status,
    #[serde(default)]
    pub feedback: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub generation: u64,
    pub mode: WorkMode,
    pub plan: Option<Plan>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Enable,
    Enter,
    Leave,
    Approve { generation: u64 },
    Feedback { generation: u64, text: String },
}
