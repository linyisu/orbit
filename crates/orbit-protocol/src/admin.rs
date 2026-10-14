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

/// Maximum Unicode scalar count for an explicit session title.
pub const MAX_SESSION_TITLE_CHARS: usize = 120;

pub fn valid_title(title: &str) -> bool {
    !title.trim().is_empty() && title.chars().count() <= MAX_SESSION_TITLE_CHARS
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_title_limit_counts_characters_and_rejects_blank_input() {
        assert!(valid_title(&"界".repeat(MAX_SESSION_TITLE_CHARS)));
        assert!(!valid_title(&"a".repeat(MAX_SESSION_TITLE_CHARS + 1)));
        assert!(!valid_title(" \n\t"));
    }
}
