use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Projects,
    LinkProject { id: String },
    Sessions,
    SessionStorage,
    RenameSession { id: String, title: String },
    DeleteSession { id: String },
    InspectSession { id: String, section: SessionSection },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSection {
    History,
    Compactions,
    Diagnostics,
}
