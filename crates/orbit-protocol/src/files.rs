//! Structured workspace browsing and explicit user edits. Reads continue to use ReadFile.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    List {
        path: String,
        #[serde(default)]
        offset: usize,
        #[serde(default)]
        include_hidden: bool,
    },
    /// The original text is a compare-and-save checkpoint, never a Markdown rendering.
    Save {
        path: String,
        original: String,
        content: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub kind: EntryKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Snapshot {
    Directory {
        path: String,
        entries: Vec<Entry>,
        next_offset: Option<usize>,
        scan_truncated: bool,
        readonly: bool,
    },
    Saved {
        path: String,
        content: String,
    },
}
