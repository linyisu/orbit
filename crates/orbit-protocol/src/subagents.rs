//! Native discovery and control of children already delegated by this parent.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    List {
        #[serde(default)]
        offset: usize,
    },
    Send {
        id: String,
        message: String,
    },
    Cancel {
        id: String,
        /// The run sampled by the row; never cancel a subsequent invocation.
        run: i64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub role: String,
    pub model: String,
    pub task: String,
    pub workspace: String,
    pub status: String,
    pub run: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Snapshot {
    List {
        agents: Vec<Agent>,
        next: Option<usize>,
    },
    Sent {
        id: String,
        status: String,
        message_id: Option<i64>,
    },
    /// Cancellation was accepted; readers must still observe the run's final
    /// stored status before presenting it as stopped.
    Stopping { id: String, run: i64 },
}
