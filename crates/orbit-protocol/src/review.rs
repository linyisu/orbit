use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Checkpoint,
    Staged,
    Unstaged,
    Untracked,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    List {
        kind: Kind,
        offset: usize,
    },
    Diff {
        kind: Kind,
        path: String,
    },
    /// Revert only the checkpoint the user reviewed, never a newer write.
    Undo {
        path: String,
        run_id: i64,
        call_id: String,
    },
    /// Update one index entry only if the file, index and HEAD still match the review.
    Stage {
        path: String,
        receipt: String,
    },
    Unstage {
        path: String,
        receipt: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub kind: Kind,
    pub status: String,
    pub undone: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "view", rename_all = "snake_case")]
pub enum Snapshot {
    List {
        entries: Vec<Entry>,
        next: Option<usize>,
        branch: Option<String>,
    },
    Diff {
        path: String,
        patch: String,
        note: String,
        checkpoint: Option<Checkpoint>,
        #[serde(default)]
        index_receipt: Option<String>,
    },
    Undone {
        path: String,
        detail: String,
    },
    IndexUpdated {
        path: String,
        staged: bool,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub run_id: i64,
    pub call_id: String,
    pub undone: bool,
}
