//! Structured terminal views of directory listings and Git evidence.
use super::*;

const ADDED: Color = Color::Rgb(139, 195, 143);
const REMOVED: Color = Color::Rgb(218, 139, 139);

// Format each logical line before transcript layout so wrapped continuations
// retain the tool gutter. Never run tool data through Markdown.
struct View {
    lines: Vec<Line<'static>>,
    expanded: bool,
    width: u16,
    hidden: bool,
}
impl View {
    fn new(expanded: bool, width: u16) -> Self {
        Self {
            lines: vec![],
            expanded,
            width,
            hidden: false,
        }
    }
    fn push(&mut self, text: &str, color: Color) {
        let available = self.width.saturating_sub(4);
        let text = safe_output(text);
        self.hidden |= text.width() > usize::from(available);
        let parts = if self.expanded && available >= 2 {
            super::super::transcript::wrap_line(&Line::from(text), available).0
        } else {
            vec![Line::from(clipped(&text, usize::from(available)))]
        };
        for line in parts {
            self.lines.push(Line::from(vec![
                Span::raw(" ".repeat(usize::from(self.width).min(4))),
                Span::styled(line.to_string(), Style::default().fg(color)),
            ]));
        }
    }
    fn note(&mut self, text: &str) {
        self.push(text, SECONDARY);
    }
    fn omitted(&mut self, count: usize, unit: &str) {
        if count > 0 {
            self.hidden = true;
            self.note(&format!("另有 {count} {unit}"));
        }
    }
    fn paging(&mut self, value: &Value) {
        if value["has_more"] == true {
            self.note(&match value["next_offset"].as_u64() {
                Some(offset) => format!("尚有后续结果，需再次查询  offset={offset}"),
                None => "尚有后续结果，需再次查询".into(),
            });
        }
        if value["scan_truncated"] == true {
            self.note("已达到扫描上限，可缩小目录范围继续查询");
        }
        if value["output_truncated"] == true {
            self.note("工具输出已截断，可缩小查询范围");
        }
    }
    fn finish(mut self) -> Vec<Line<'static>> {
        if self.expanded || self.hidden {
            self.lines.push(toggle_line(self.expanded, self.width));
        }
        self.lines
    }
}

pub(super) fn files(row: &ToolRow, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let value: Value = serde_json::from_str(&row.output).ok()?;
    let entries = value["entries"].as_array()?;
    let arguments: Value = serde_json::from_str(&row.arguments).unwrap_or_default();
    let base = arguments["path"]
        .as_str()
        .unwrap_or(".")
        .trim_end_matches('/');
    let base = base.strip_prefix("./").unwrap_or(base);
    let prefix = format!("{base}/");
    let directories = entries.iter().filter(|e| e["kind"] == "directory").count();
    let mut view = View::new(expanded, width);
    if entries.is_empty() {
        view.note("当前查询未返回条目");
    } else {
        view.note(&format!(
            "本页 {} 个文件  {directories} 个目录",
            entries.len() - directories
        ));
    }
    let limit = if expanded { entries.len() } else { 6 };
    for entry in entries.iter().take(limit) {
        let path = entry["path"].as_str()?;
        // Keep every nested component; only remove the queried root already
        // visible in the tool header. Never reduce distinct paths to basenames.
        let path = path.strip_prefix(&prefix).unwrap_or(path);
        if entry["kind"] == "directory" {
            view.push(&format!("{path}/"), ACCENT);
        } else {
            view.push(path, Color::Reset);
        }
    }
    view.omitted(entries.len().saturating_sub(limit), "个条目");
    if let Some(skipped) = value["skipped_entries"].as_u64().filter(|n| *n > 0) {
        view.note(&format!("跳过 {skipped} 个不可展示条目"));
    }
    view.paging(&value);
    Some(view.finish())
}

pub(super) fn summary(arguments: &str) -> String {
    let value: Value = serde_json::from_str(arguments).unwrap_or_default();
    let label = match value["view"].as_str().unwrap_or("status") {
        "history" => "提交历史",
        "diff" if value["staged"] == true => "暂存区差异",
        "diff" => "工作区差异",
        _ => "工作区状态",
    };
    match value["path"].as_str() {
        Some(path) => format!(
            "{label}  {}",
            super::super::render::compact_preview(path, 100)
        ),
        None => label.into(),
    }
}

pub(super) fn git(row: &ToolRow, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let value: Value = serde_json::from_str(&row.output).ok()?;
    let mut view = View::new(expanded, width);
    if let Some(commits) = value["commits"].as_array() {
        view.note(&format!("本页 {} 条提交", commits.len()));
        let limit = if expanded { commits.len() } else { 4 };
        for commit in commits.iter().take(limit) {
            let id = commit["id"].as_str()?;
            let short = commit["short_id"].as_str()?;
            if !matches!(id.len(), 40 | 64)
                || !id.bytes().all(|b| b.is_ascii_hexdigit())
                || short.len() < 4
                || !id.starts_with(short)
            {
                view.push("历史记录字段异常，请重新查询提交历史", REMOVED);
                if expanded {
                    for line in serde_json::to_string_pretty(commit).ok()?.lines() {
                        view.note(line);
                    }
                }
                view.hidden = true;
                continue;
            }
            let subject = commit["subject"].as_str()?;
            view.push(
                &format!(
                    "{short}  {}",
                    if subject.is_empty() {
                        "无提交标题"
                    } else {
                        subject
                    }
                ),
                Color::Reset,
            );
            if expanded {
                view.note(&format!(
                    "  {}  {}",
                    commit["author"].as_str()?,
                    commit["authored_at"].as_str()?
                ));
                view.note(&format!("  {id}"));
                view.lines.push(Line::default());
            }
        }
        // Author, time and full ID are details available on expansion.
        view.hidden |= !commits.is_empty();
        view.omitted(commits.len().saturating_sub(limit), "条提交");
    } else if let Some(entries) = value["entries"].as_array() {
        let branch = &value["branch"];
        let head = branch["head"].as_str().unwrap_or("未知分支");
        view.push(head, ACCENT);
        if let Some(upstream) = branch["upstream"].as_str() {
            view.note(&format!(
                "跟踪 {upstream}  领先 {}  落后 {}",
                branch["ahead"], branch["behind"]
            ));
        }
        if expanded && let Some(id) = branch["oid"].as_str() {
            view.note(&format!("HEAD  {id}"));
        }
        view.note(if entries.is_empty() {
            "当前查询无变更"
        } else {
            "索引 / 工作区  路径"
        });
        let limit = if expanded { entries.len() } else { 6 };
        for entry in entries.iter().take(limit) {
            let (status, color) = match entry["kind"].as_str()? {
                "untracked" => ("??", ADDED),
                "ignored" => ("!!", SECONDARY),
                "unmerged" => (entry["xy"].as_str().unwrap_or("UU"), REMOVED),
                _ => (entry["xy"].as_str().unwrap_or(".."), ACCENT),
            };
            let path = entry["path"].as_str()?;
            let path = entry["original_path"]
                .as_str()
                .map(|old| format!("{old} → {path}"))
                .unwrap_or_else(|| path.into());
            view.push(&format!("{status:<2}  {path}"), color);
        }
        view.hidden |= branch["oid"].is_string();
        view.omitted(entries.len().saturating_sub(limit), "个变更");
    } else if let Some(lines) = value["lines"].as_array() {
        view.note(&format!(
            "{}  本页 {} 行",
            if value["staged"] == true {
                "暂存区差异"
            } else {
                "工作区差异"
            },
            lines.len()
        ));
        if lines.is_empty() {
            view.note("当前查询无差异");
        }
        let limit = if expanded { lines.len() } else { 8 };
        for line in lines.iter().take(limit) {
            let text = line.as_str()?;
            let color = if text.starts_with("+++")
                || text.starts_with("---")
                || text.starts_with("diff ")
            {
                ACCENT
            } else if text.starts_with('+') {
                ADDED
            } else if text.starts_with('-') {
                REMOVED
            } else if text.starts_with("@@") {
                Color::Rgb(139, 185, 195)
            } else {
                Color::Reset
            };
            view.push(text, color);
        }
        view.omitted(lines.len().saturating_sub(limit), "行差异");
    } else {
        return fallback(row, expanded, width);
    }
    view.paging(&value);
    Some(view.finish())
}

pub(super) fn fallback(row: &ToolRow, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let value: Value = serde_json::from_str(&row.output).ok()?;
    // Preserve existing command, write receipt and text-content renderers.
    if !value.is_object() && !value.is_array()
        || value.get("stdout").is_some()
        || value.get("written_bytes").is_some()
        || value["content"].is_string()
    {
        return None;
    }
    let pretty = serde_json::to_string_pretty(&value).ok()?;
    let lines: Vec<_> = pretty.lines().collect();
    let limit = if expanded { lines.len() } else { 6 };
    let mut view = View::new(expanded, width);
    for line in lines.iter().take(limit) {
        view.push(line, Color::Reset);
    }
    view.omitted(lines.len().saturating_sub(limit), "行");
    Some(view.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, value: Value) -> ToolRow {
        ToolRow {
            id: "test".into(),
            name: name.into(),
            arguments: "{}".into(),
            status: ToolStatus::Finished,
            output: value.to_string(),
        }
    }
    fn text(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }
    #[test]
    fn directory_preview_preserves_nested_paths_and_separates_paging_from_expansion() {
        let mut row = tool(
            "list_files",
            json!({"entries":[
            {"kind":"directory","path":"src/中文"},
            {"kind":"file","path":"src/a/mod.rs"},
            {"kind":"file","path":"src/b/mod.rs"},
            {"kind":"file","path":"src/c.rs"},
            {"kind":"file","path":"src/d.rs"},
            {"kind":"file","path":"src/e.rs"},
            {"kind":"file","path":"src/f.rs"}], "has_more":true,"next_offset":7}),
        );
        row.arguments = json!({"path":"src"}).to_string();
        let original = row.output.clone();
        let preview = text(&files(&row, false, 80).unwrap());
        assert!(preview.contains("本页 6 个文件  1 个目录"));
        assert!(preview.contains("中文/"));
        assert!(preview.contains("a/mod.rs") && preview.contains("b/mod.rs"));
        assert!(!preview.contains("src/"));
        assert!(preview.contains("另有 1 个条目") && preview.contains("[展开]"));
        let expanded = text(&files(&row, true, 80).unwrap());
        assert!(expanded.contains("f.rs") && expanded.contains("[收起]"));
        assert!(expanded.contains("需再次查询  offset=7"));
        assert_eq!(row.output, original);
    }
    #[test]
    fn git_views_show_evidence_instead_of_json_and_do_not_invent_repairs() {
        let history = tool(
            "git_inspect",
            json!({"commits":[{
            "id":"a".repeat(40),"short_id":"aaaaaaa", "author":"作者", "authored_at":"2026-09-19T10:00:00+08:00", "subject":"修复列表"}]}),
        );
        let preview = text(&git(&history, false, 90).unwrap());
        assert!(preview.contains("aaaaaaa  修复列表") && preview.contains("[展开]"));
        assert!(!preview.contains("author"));
        let expanded = text(&git(&history, true, 90).unwrap());
        assert!(expanded.contains("作者  2026-09-19") && expanded.contains(&"a".repeat(40)));
        let malformed = tool(
            "git_inspect",
            json!({"commits":[{
            "id":"", "short_id":"author", "author":"2026-09-18", "authored_at":"bad", "subject":"shifted"}]}),
        );
        assert!(text(&git(&malformed, false, 90).unwrap()).contains("历史记录字段异常"));
        assert!(text(&git(&malformed, true, 90).unwrap()).contains("shifted"));
        let status = tool(
            "git_inspect",
            json!({"branch":{"head":"dev","oid":"abc","upstream":"origin/dev","ahead":1,"behind":0},
            "entries":[{"kind":"renamed_or_copied","xy":"R.","original_path":"旧.rs","path":"新.rs"},
                {"kind":"untracked","path":"new.rs"}]}),
        );
        let status = text(&git(&status, false, 90).unwrap());
        assert!(status.contains("R.  旧.rs → 新.rs") && status.contains("??  new.rs"));
        let diff = tool(
            "git_inspect",
            json!({"staged":true,"lines":["--- a/file","+++ b/file","@@ -1 +1 @@","-old","+new"]}),
        );
        let lines = git(&diff, true, 80).unwrap();
        assert!(
            lines.iter().any(|l| l.to_string().ends_with("+new")
                && l.spans.last().unwrap().style.fg == Some(ADDED))
        );
        assert!(lines.iter().any(|l| l.to_string().ends_with("-old")
            && l.spans.last().unwrap().style.fg == Some(REMOVED)));
    }
    #[test]
    fn structured_results_fit_narrow_screens_escape_controls_and_keep_model_payload() {
        let long = format!("中文👩‍💻e\u{301}{}\u{1b}[31m", "很长的路径/".repeat(15));
        let rows = [
            tool(
                "list_files",
                json!({"entries":[{"path":long,"kind":"file"}]}),
            ),
            tool("git_inspect", json!({"lines":[long],"staged":false})),
            tool("extension_tool", json!({"nested":{"list":[long,"second"]}})),
        ];
        for row in rows {
            let original = row.output.clone();
            for width in [0, 1, 4, 5, 6, 12, 30, 80] {
                for expanded in [false, true] {
                    let lines = super::super::lines(&row, expanded, width).unwrap();
                    assert!(
                        lines.iter().all(|l| l.width() <= usize::from(width)),
                        "{} {width}",
                        row.name
                    );
                    assert!(!text(&lines).contains('\u{1b}'));
                }
            }
            assert_eq!(row.output, original);
        }
    }
    #[test]
    fn expansion_wraps_with_a_stable_gutter_and_preserves_text() {
        let original = "中文目录/很长的文件名称.rs".repeat(8);
        let mut view = View::new(true, 30);
        view.push(&original, Color::Reset);
        let lines = view.finish();
        let joined = lines[..lines.len() - 1]
            .iter()
            .map(|l| l.to_string().strip_prefix("    ").unwrap().to_owned())
            .collect::<String>();
        assert_eq!(joined, original);
    }
}
