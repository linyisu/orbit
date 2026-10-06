//! Rendering of the private write-receipt ledger. These diffs compare the
//! exact text captured around a committed Orbit write; they do not claim that
//! the current file still has that content.

use std::time::Duration;

use orbit_store::types::WorkspaceChange;
use similar::TextDiff;

const MAX_DIFF_CHARS: usize = 48 * 1024;

pub fn list(changes: &[WorkspaceChange]) -> String {
    if changes.is_empty() {
        return "当前会话还没有 Orbit 成功写入的文件。".into();
    }

    let mut output = String::from("Orbit 变更\n");

    for (index, record) in changes.iter().enumerate() {
        use std::fmt::Write;

        let kind = if record.undone {
            "已回退"
        } else if record.change.before.is_some() {
            "修改"
        } else {
            "新建"
        };

        let _ = writeln!(
            output,
            "{}. {} · {} · {} · run {} {}",
            index + 1,
            record.change.path,
            kind,
            record.tool,
            record.run_id,
            record.run_status
        );
    }

    output.push_str(
        "\n/diff <路径> 可查看最近一次该路径写入的检查点差异；/undo [路径] 可在确认后回退。\n",
    );

    output
}

pub fn diff(changes: &[WorkspaceChange], path: &str) -> String {
    let Some(record) = changes
        .iter()
        .rev()
        .find(|record| record.change.path == path)
    else {
        return format!("当前会话没有 {path} 的 Orbit 写入检查点。先运行 /changes 查看路径。");
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
    let (text, truncated) = truncate_chars(text, MAX_DIFF_CHARS);
    let status = if record.undone { " · 已回退" } else { "" };

    let mut output = format!(
        "检查点：{} · {} · run {} {}{}\n{}",
        record.change.path, record.tool, record.run_id, record.run_status, status, text
    );

    if truncated {
        output.push_str("\n… 差异显示已截断；检查点全文仍在本地会话数据库中。\n");
    }

    output.push_str("\n这是该次 Orbit 写入前后的快照；当前文件可能已被后续操作修改。\n");

    output
}

fn truncate_chars(value: String, limit: usize) -> (String, bool) {
    if value.chars().count() <= limit {
        return (value, false);
    }

    (value.chars().take(limit).collect(), true)
}

#[cfg(test)]
mod tests {
    use orbit_agent::tool::FileChange;

    use super::*;

    fn record(path: &str, before: Option<&str>, after: &str) -> WorkspaceChange {
        WorkspaceChange {
            run_id: 7,
            run_status: "completed".into(),
            call_id: "call-1".into(),
            tool: "edit_file".into(),
            change: FileChange {
                path: path.into(),
                before: before.map(str::to_owned),
                after: after.into(),
            },
            undone: false,
        }
    }

    #[test]
    fn lists_and_renders_the_latest_path_checkpoint() {
        let changes = vec![
            record("src/a.rs", Some("old\n"), "mid\n"),
            record("src/a.rs", Some("mid\n"), "new\n"),
        ];

        assert!(list(&changes).contains("src/a.rs"));

        let rendered = diff(&changes, "src/a.rs");

        assert!(rendered.contains("-mid"));
        assert!(rendered.contains("+new"));
        assert!(!rendered.contains("old"));
    }
}
