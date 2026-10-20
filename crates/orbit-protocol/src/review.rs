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
    Status {
        offset: usize,
    },
    /// Browse a pinned commit ancestry without changing HEAD or the index.
    History {
        revision: String,
        offset: usize,
    },
    Commit {
        revision: String,
    },
    CommitFile {
        revision: String,
        path: String,
    },
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
    Status {
        entries: Vec<Entry>,
        next: Option<usize>,
        branch: Option<String>,
    },
    History {
        revision: String,
        commits: Vec<Commit>,
        branches: Vec<String>,
        next: Option<usize>,
    },
    Commit {
        commit: Commit,
        message: String,
        files: Vec<String>,
    },
    CommitFile {
        path: String,
        patch: String,
        content: Option<String>,
    },
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
pub struct Commit {
    pub id: String,
    pub short_id: String,
    pub authored_at: String,
    pub author: String,
    pub subject: String,
    #[serde(default)]
    pub publication: Publication,
}

/// Reachability from local remote-tracking refs; no network request is implied.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Publication {
    #[default]
    Unknown,
    Unpushed,
    Published,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub run_id: i64,
    pub call_id: String,
    pub undone: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_commit_records_do_not_invent_publication_status() {
        let commit: Commit = serde_json::from_value(serde_json::json!({
            "id": "generated",
            "short_id": "gen",
            "authored_at": "2020-01-01",
            "author": "Example",
            "subject": "Generated record"
        }))
        .unwrap();
        assert_eq!(commit.publication, Publication::Unknown);
    }
}
