//! Durable inputs waiting for the session Agent's next input boundary.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    List {
        after: Option<i64>,
    },
    Edit {
        id: i64,
        /// Exact stored content sampled by the editor, never a trimmed preview.
        expected: String,
        content: String,
        /// The active run sampled by List; None means an idle session.
        run: Option<i64>,
    },
    Remove {
        id: i64,
        expected: String,
        run: Option<i64>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Queued,
    /// Removed from the live inbox; its checkpoint receipt may still be pending.
    Claimed,
    /// The Agent has closed its inbox and is finishing its run.
    Finishing,
    /// Durable input left for a later run after interruption/cancellation.
    Idle,
    /// A different service owns execution, or durable and live data differ.
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<crate::MessageImage>,
    pub id: i64,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<crate::MessageImage>,
    pub state: State,
}

impl Entry {
    pub fn editable(&self) -> bool {
        matches!(self.state, State::Queued | State::Idle)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Snapshot {
    List {
        entries: Vec<Entry>,
        next: Option<i64>,
        run: Option<i64>,
    },
    Edited {
        entry: Entry,
    },
    Removed {
        id: i64,
    },
}

/// Verify ascending IDs and the continuation cursor of an inbox page.
pub fn valid_page(after: Option<i64>, entries: &[Entry], next: Option<i64>) -> bool {
    let mut previous = after.unwrap_or(0);
    for entry in entries {
        if entry.id <= previous {
            return false;
        }
        previous = entry.id;
    }
    next.is_none() || (!entries.is_empty() && next == entries.last().map(|entry| entry.id))
}

/// Verify that an entry state agrees with the sampled active run.
pub fn valid_entry(entry: &Entry, run: Option<i64>) -> bool {
    entry.id > 0
        && run.is_none_or(|run| run > 0)
        && match entry.state {
            State::Queued => run.is_some(),
            State::Idle => run.is_none(),
            State::Claimed | State::Finishing | State::Unavailable => true,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claimed_finishing_and_foreign_execution_never_offer_mutation() {
        for state in [State::Claimed, State::Finishing, State::Unavailable] {
            assert!(
                !Entry {
                    files: Vec::new(),
                    images: vec![],
                    id: 1,
                    content: "retained".into(),
                    state
                }
                .editable()
            );
        }
        for state in [State::Queued, State::Idle] {
            assert!(
                Entry {
                    files: Vec::new(),
                    images: vec![],
                    id: 1,
                    content: "editable".into(),
                    state
                }
                .editable()
            );
        }
    }

    #[test]
    fn queue_request_preserves_exact_cas_content_and_sampled_run() {
        let request = crate::Request::InputQueue {
            session: "session".into(),
            expected_workspace: "/workspace".into(),
            action: Action::Edit {
                id: 42,
                expected: "  原文\n第二行  ".into(),
                content: "修改".into(),
                run: Some(9),
            },
        };
        let bytes = serde_json::to_vec(&request).unwrap();
        let crate::Request::InputQueue {
            expected_workspace,
            action: Action::Edit {
                id, expected, run, ..
            },
            ..
        } = serde_json::from_slice(&bytes).unwrap()
        else {
            panic!("structured queue request expected")
        };
        assert_eq!(expected_workspace, "/workspace");
        assert_eq!(id, 42);
        assert_eq!(expected, "  原文\n第二行  ");
        assert_eq!(run, Some(9));
    }

    #[test]
    fn pagination_rejects_nonadvancing_ids_and_cursor_mismatches() {
        let entry = |id| Entry {
            files: Vec::new(),
            images: vec![],
            id,
            content: "input".into(),
            state: State::Queued,
        };
        assert!(valid_page(None, &[entry(4), entry(10)], Some(10)));
        assert!(valid_page(Some(10), &[entry(12)], None));
        assert!(!valid_page(Some(10), &[entry(10)], Some(10)));
        assert!(!valid_page(None, &[entry(4), entry(4)], None));
        assert!(!valid_page(None, &[], Some(4)));
        assert!(!valid_page(None, &[entry(4)], Some(5)));
    }

    #[test]
    fn contradictory_mutable_state_and_run_cannot_enable_controls() {
        let entry = |state| Entry {
            files: Vec::new(),
            images: vec![],
            id: 9,
            content: "input".into(),
            state,
        };
        assert!(valid_entry(&entry(State::Queued), Some(4)));
        assert!(valid_entry(&entry(State::Idle), None));
        assert!(!valid_entry(&entry(State::Queued), None));
        assert!(!valid_entry(&entry(State::Idle), Some(4)));
        assert!(!valid_entry(&entry(State::Queued), Some(0)));
    }
}
