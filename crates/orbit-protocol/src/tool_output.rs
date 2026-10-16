//! Read-only paging of retained evidence, independent of model context budgets.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    Command { output_id: String, stream: Stream },
    Web { snapshot_id: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Stdout,
    Stderr,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub content: String,
    /// Opaque byte offset into the same retained source; absent at EOF.
    pub next_offset: Option<u64>,
}
