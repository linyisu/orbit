//! Human-readable views of the session's committed file-write receipts.

use std::{fmt::Write, time::Duration};

use orbit_store::WorkspaceChange;
use similar::TextDiff;

const MAX_LIST_ENTRIES: usize = 100;
const MAX_DIFF_CHARS: usize = 48 * 1024;

pub fn list(changes: &[WorkspaceChange]) -> String {
    if changes.is_empty() {
        return "No file changes recorded in this session.".to_owned();
    }

    let mut output = format!("File changes · {} recorded\n", changes.len());
    for record in changes.iter().rev().take(MAX_LIST_ENTRIES) {
        let kind = if record.undone {
            "undone"
        } else if record.change.before.is_some() {
            "modified"
        } else {
            "created"
        };
        let _ = writeln!(
            output,
            "{}  {}  {}  ·  {}",
            record.id, record.change.path, kind, record.tool
        );
    }
    if changes.len() > MAX_LIST_ENTRIES {
        let _ = writeln!(
            output,
            "{} older changes are not shown here.",
            changes.len() - MAX_LIST_ENTRIES
        );
    }
    output.push_str("\nUse /diff <path> to inspect a receipt or /undo [path] to restore it.");
    output
}

pub fn diff(changes: &[WorkspaceChange], path: &str) -> String {
    let Some(record) = changes
        .iter()
        .rev()
        .find(|record| record.change.path == path)
    else {
        return format!(
            "No recorded write for {path} in this session. Use /changes to list paths."
        );
    };

    let before = record.change.before.as_deref().unwrap_or_default();
    let text = TextDiff::configure()
        .timeout(Duration::from_millis(250))
        .diff_lines(before, &record.change.after)
        .unified_diff()
        .header(
            &format!("a/{}", record.change.path),
            &format!("b/{}", record.change.path),
        )
        .to_string();
    let truncated = text.chars().count() > MAX_DIFF_CHARS;
    let mut output = format!(
        "Write #{} · {} · {}{}\n{}",
        record.id,
        record.tool,
        record.run_status,
        if record.undone { " · undone" } else { "" },
        text.chars().take(MAX_DIFF_CHARS).collect::<String>()
    );
    if truncated {
        output.push_str(
            "\nDiff display truncated; the full receipt remains in the session database.",
        );
    }
    output.push_str("\nThis is the recorded before/after state; the current file may differ.");
    output
}

#[cfg(test)]
mod tests {
    use orbit_agent::tool::FileChange;

    use super::*;

    fn record(path: &str, before: Option<&str>, after: &str) -> WorkspaceChange {
        WorkspaceChange {
            id: 5,
            run_id: 2,
            run_status: "completed".to_owned(),
            call_id: "call-1".to_owned(),
            tool: "edit_file".to_owned(),
            change: FileChange {
                path: path.to_owned(),
                before: before.map(str::to_owned),
                after: after.to_owned(),
            },
            undone: false,
        }
    }

    #[test]
    fn shows_the_latest_receipt_for_a_path() {
        let changes = vec![
            record("src/main.rs", Some("old\n"), "middle\n"),
            record("src/main.rs", Some("middle\n"), "new\n"),
        ];

        let listing = list(&changes);
        let rendered = diff(&changes, "src/main.rs");

        assert!(listing.contains("src/main.rs"));
        assert!(rendered.contains("-middle"));
        assert!(rendered.contains("+new"));
        assert!(!rendered.contains("-old"));
    }
}
