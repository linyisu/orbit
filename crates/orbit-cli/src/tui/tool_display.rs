//! Presentation-only views. Never rewrite tool results used by the model or store.
use std::{collections::BTreeMap, ops::Range};

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use regex::{Regex, RegexBuilder};
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{
    app::{ToolRow, ToolStatus},
    render::safe_output,
};

const ACCENT: Color = Color::Rgb(218, 139, 109);
const SECONDARY: Color = Color::Rgb(160, 160, 160);
// Keep adjacent file previews aligned even when their lengths differ.
const MIN_LINE_NUMBER_WIDTH: usize = 4;

pub(super) fn toggle_line(expanded: bool, width: u16) -> Line<'static> {
    Line::from(vec![
        Span::raw(" ".repeat(usize::from(width).min(4))),
        Span::styled(
            clipped(
                if expanded { "[收起]" } else { "[展开]" },
                usize::from(width).saturating_sub(4),
            ),
            Style::default().fg(ACCENT),
        ),
    ])
}

pub(super) fn search_summary(arguments: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(arguments) else {
        return String::new();
    };
    let query = super::render::compact_preview(value["query"].as_str().unwrap_or_default(), 100);
    let path = super::render::compact_preview(value["path"].as_str().unwrap_or("."), 100);
    format!("{} {path}", Value::String(query))
}

pub(super) fn memory_summary(arguments: &str) -> String {
    serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|value| {
            value["title"]
                .as_str()
                .map(|title| super::render::compact_preview(title, 100))
        })
        .unwrap_or_default()
}

// Use the search tool's literal/regex and Unicode case-folding semantics.
fn matcher(value: &Value) -> Option<Regex> {
    let query = value["query"].as_str()?;
    if query.is_empty() {
        return None;
    }
    let pattern = if value["regex"].as_bool() == Some(true) {
        query.to_owned()
    } else {
        regex::escape(query)
    };
    RegexBuilder::new(&pattern)
        .case_insensitive(value["case_sensitive"].as_bool() != Some(true))
        .build()
        .ok()
}

// Compute matches before escaping terminal controls. Map their offsets to the
// displayed text so tabs, clipping and wrapped regex matches keep their styles.
fn marked_source(text: &str, matcher: Option<&Regex>) -> (String, Vec<Range<usize>>) {
    let Some(matcher) = matcher else {
        return (safe_output(text), vec![]);
    };
    let mut rendered = String::new();
    let mut ranges = Vec::new();
    let mut offset = 0;
    for hit in matcher.find_iter(text).filter(|hit| !hit.is_empty()) {
        rendered.push_str(&safe_output(&text[offset..hit.start()]));
        let start = rendered.len();
        rendered.push_str(&safe_output(hit.as_str()));
        ranges.push(start..rendered.len());
        offset = hit.end();
    }
    rendered.push_str(&safe_output(&text[offset..]));
    (rendered, ranges)
}

fn source_spans(text: &str, marks: &[Range<usize>], range: Range<usize>) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut offset = range.start;
    for mark in marks {
        let start = mark.start.max(range.start);
        let end = mark.end.min(range.end);
        if start >= end {
            continue;
        }
        if offset < start {
            spans.push(Span::raw(text[offset..start].to_owned()));
        }
        spans.push(Span::styled(
            text[start..end].to_owned(),
            super::markdown::mark_style(Style::default()),
        ));
        offset = end;
    }
    if offset < range.end {
        spans.push(Span::raw(text[offset..range.end].to_owned()));
    }
    spans
}

pub(super) fn lines(row: &ToolRow, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    if row.status != ToolStatus::Finished {
        return None;
    }
    match row.name.as_str() {
        "project_overview" => overview(
            &serde_json::from_str::<Value>(&row.output).ok()?,
            expanded,
            width,
        ),
        "search_text" => search(
            &serde_json::from_str::<Value>(&row.output).ok()?,
            expanded,
            width,
        ),
        "read_file" => read(
            &serde_json::from_str::<Value>(&row.output).ok()?,
            expanded,
            width,
        ),
        "memory_save" => memory(
            &serde_json::from_str::<Value>(&row.output).ok()?,
            expanded,
            width,
        ),
        _ => None,
    }
}

fn overview(value: &Value, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let inventory = value["inventory"].as_object()?;
    let languages = inventory["languages"]
        .as_object()?
        .iter()
        .map(|(name, count)| format!("{name} {count}"))
        .collect::<Vec<_>>()
        .join(" / ");
    let mut text = vec![format!("{} 个文件  {languages}", inventory["files"])];
    let manifests = value["manifests"].as_array()?;
    for manifest in manifests.iter().take(if expanded { usize::MAX } else { 3 }) {
        text.push(format!(
            "{}{}",
            manifest["path"].as_str().unwrap_or_default(),
            manifest["package"]
                .as_str()
                .map(|name| format!("  {name}"))
                .unwrap_or_default()
        ));
        if expanded {
            for (key, label) in [
                ("declared_scripts", "声明的脚本"),
                ("verification_candidates", "待验证入口"),
                ("dependencies", "依赖"),
                ("workspace_members", "工作区成员"),
                ("workspaces", "工作区配置"),
                ("package_manager", "包管理器"),
                ("build_system", "构建后端"),
                ("configured_tools", "工具配置"),
            ] {
                if let Some(detail) = manifest.get(key).filter(|v| !v.is_null()) {
                    text.push(format!("  {label}：{detail}"));
                }
            }
            if manifest["parsed"] == false {
                text.push("  未解析构建配置，请读取原文件确认。".into());
            }
            if manifest["truncated"] == true {
                text.push("  部分配置已省略，请读取原文件。".into());
            }
        }
    }
    if expanded {
        for directory in value["inventory"]["directories"].as_array()? {
            text.push(format!(
                "目录  {}  {} 个文件",
                directory["path"].as_str().unwrap_or_default(),
                directory["files"]
            ));
        }
        for (key, label) in [("test_files", "测试"), ("instruction_files", "项目规则")] {
            for file in value[key].as_array()? {
                text.push(format!("{label}  {}", file.as_str().unwrap_or_default()));
            }
        }
        for symbol in value["symbols"].as_array()? {
            text.push(format!(
                "符号候选  {}:{}  {}",
                symbol["path"].as_str().unwrap_or_default(),
                symbol["line"],
                symbol["name"].as_str().unwrap_or_default()
            ));
        }
    }
    if value["truncated"].as_object()?.values().any(|v| v == true) {
        text.push("部分内容未纳入概览，可缩小目录范围继续查看。".into());
    }
    let mut out = Vec::new();
    for line in text {
        if expanded && width > 4 {
            let (wrapped, _) =
                super::transcript::wrap_line(&Line::from(safe_output(&line)), width - 4);
            out.extend(
                wrapped
                    .into_iter()
                    .map(|line| Line::from(format!("    {line}"))),
            );
        } else {
            note(&mut out, &line, width);
        }
    }
    out.push(toggle_line(expanded, width));
    Some(out)
}

fn memory(value: &Value, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let memory = &value["memory"];
    let category = match memory["category"].as_str()? {
        "fact" => "事实",
        "decision" => "决策",
        "constraint" => "约束",
        "preference" => "偏好",
        category => category,
    };
    let content = memory["content"].as_str()?;
    let mut out = Vec::new();
    let id = memory["id"]
        .as_u64()
        .map(|id| format!(" · #{id}"))
        .unwrap_or_default();
    note(
        &mut out,
        &format!("已保存 · {category} · 当前会话{id}"),
        width,
    );
    let mut omitted = false;
    for (index, line) in content.lines().enumerate() {
        if !expanded && index >= 3 {
            omitted = true;
            break;
        }
        let text = safe_output(line);
        let available = usize::from(width).saturating_sub(4);
        if expanded && available > 0 {
            let mut chunk = String::new();
            let mut used = 0;
            for glyph in text.graphemes(true) {
                if used + glyph.width() > available && !chunk.is_empty() {
                    out.push(Line::from(format!("    {}", std::mem::take(&mut chunk))));
                    used = 0;
                }
                if glyph.width() > available {
                    chunk.push('…');
                    used += 1;
                } else {
                    chunk.push_str(glyph);
                    used += glyph.width();
                }
            }
            out.push(Line::from(format!("    {chunk}")));
        } else {
            omitted |= text.width() > available;
            out.push(Line::from(clipped(
                &format!("    {text}"),
                usize::from(width),
            )));
        }
    }
    if expanded || omitted {
        out.push(toggle_line(expanded, width));
    }
    Some(out)
}

fn clipped(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for glyph in text.graphemes(true) {
        if used + glyph.width() > width - 1 {
            break;
        }
        out.push_str(glyph);
        used += glyph.width();
    }
    out.push('…');
    out
}

fn note(out: &mut Vec<Line<'static>>, text: &str, width: u16) {
    out.push(Line::from(Span::styled(
        clipped(&format!("    {}", safe_output(text)), usize::from(width)),
        Style::default().fg(SECONDARY),
    )));
}

fn source(
    out: &mut Vec<Line<'static>>,
    number: u64,
    digits: usize,
    text: &str,
    matcher: Option<&Regex>,
    expanded: bool,
    width: u16,
) {
    let prefix = format!("    {number:>digits$} │ ");
    let available = usize::from(width).saturating_sub(prefix.width());
    let (text, marks) = marked_source(text, matcher);
    if available == 0 {
        note(out, &format!("{number}: {text}"), width);
        return;
    }
    let style = Style::default().fg(if matcher.is_some() { ACCENT } else { SECONDARY });
    let mut push = |range: Range<usize>, continuation: bool, leading: bool, trailing: bool| {
        let mut spans = vec![Span::styled(
            if continuation {
                format!("    {} │ ", " ".repeat(digits))
            } else {
                prefix.clone()
            },
            style,
        )];
        if leading {
            spans.push(Span::raw("…"));
        }
        spans.extend(source_spans(&text, &marks, range));
        if trailing {
            spans.push(Span::raw("…"));
        }
        out.push(Line::from(spans));
    };
    if !expanded {
        // A search preview should show the hit, even deep inside a long line.
        let mut start = 0;
        if let Some(first) = marks.first()
            && text[..first.end].width() >= available
        {
            let before: Vec<_> = text
                .grapheme_indices(true)
                .take_while(|(offset, _)| *offset <= first.start)
                .collect();
            let mut context_width = 0;
            for (offset, glyph) in before.into_iter().rev() {
                if offset < first.start {
                    context_width += glyph.width();
                }
                if context_width > available / 4 {
                    break;
                }
                start = offset;
            }
        }
        let budget = available.saturating_sub(usize::from(start > 0));
        let truncated = text[start..].width() > budget;
        let preview = clipped(&text[start..], budget);
        let end = start + preview.len()
            - if truncated && budget > 0 {
                '…'.len_utf8()
            } else {
                0
            };
        push(start..end, false, start > 0, truncated && budget > 0);
        return;
    }
    // Wrap by display cells, preserving indentation and keeping wide/combining
    // graphemes intact. Continuations have a blank line-number gutter.
    let mut start = 0;
    let mut used = 0;
    let mut continuation = false;
    for (offset, glyph) in text.grapheme_indices(true) {
        let size = glyph.width();
        if used + size > available && offset > start {
            push(start..offset, continuation, false, false);
            start = offset;
            continuation = true;
            used = 0;
        }
        if size > available {
            push(offset..offset, continuation, false, true);
            start = offset + glyph.len();
            continuation = true;
            used = 0;
        } else {
            used += size;
        }
    }
    if start < text.len() || !continuation {
        push(start..text.len(), continuation, false, false);
    }
}

fn search(value: &Value, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let matches = value["matches"].as_array()?;
    let matcher = matcher(value);
    // Merge overlapping context and multiple matches on the same source line.
    let mut files: BTreeMap<&str, BTreeMap<u64, (&str, bool)>> = BTreeMap::new();
    for hit in matches {
        let path = hit["path"].as_str()?;
        let line = hit["line"].as_u64()?;
        let text = hit["text"].as_str()?;
        let rows = files.entry(path).or_default();
        rows.insert(line, (text, true));
        if expanded && let Some(context) = hit["context"].as_array() {
            for item in context {
                rows.entry(item["line"].as_u64()?)
                    .or_insert((item["text"].as_str()?, false));
            }
        }
    }
    let total_rows: usize = files.values().map(BTreeMap::len).sum();
    // Share one gutter across all files in the result, including large lines.
    let digits = files
        .values()
        .filter_map(|rows| rows.last_key_value())
        .map(|(line, _)| line.to_string().len())
        .max()
        .unwrap_or_default()
        .max(MIN_LINE_NUMBER_WIDTH);
    let clipped_source = files
        .values()
        .flat_map(|rows| rows.values())
        .any(|(text, _)| safe_output(text).width() > usize::from(width).saturating_sub(digits + 7));
    let has_context = matches.iter().any(|hit| {
        hit["context"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|row| row["line"] != hit["line"]))
    });
    let mut out = Vec::new();
    note(
        &mut out,
        &format!("{} 处匹配 · {} 个文件", matches.len(), files.len()),
        width,
    );
    let mut shown = 0;
    for (path, rows) in files {
        if !expanded && shown >= 3 {
            break;
        }
        if shown > 0 {
            out.push(Line::default());
        }
        note(&mut out, path, width);
        let mut previous = None;
        for (line, (text, matched)) in rows {
            if !expanded && shown >= 3 {
                break;
            }
            if previous.is_some_and(|last| line > last + 1) {
                out.push(Line::default());
            }
            source(
                &mut out,
                line,
                digits,
                text,
                if matched { matcher.as_ref() } else { None },
                expanded,
                width,
            );
            previous = Some(line);
            shown += 1;
        }
    }
    if shown < total_rows {
        note(
            &mut out,
            &format!("另有 {} 行匹配", total_rows - shown),
            width,
        );
    }
    if value["has_more"].as_bool() == Some(true) {
        note(&mut out, "还有更多匹配，当前结果未全部返回", width);
    }
    if value["scan_truncated"].as_bool() == Some(true) {
        note(&mut out, "搜索达到扫描上限，部分文件尚未搜索", width);
    }
    if expanded || shown < total_rows || clipped_source || has_context {
        out.push(toggle_line(expanded, width));
    }
    Some(out)
}

fn read(value: &Value, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let content = value["content"].as_str()?;
    let start = value["start_line"].as_u64()?;
    let end = value["end_line"].as_u64()?;
    let rows: Vec<_> = content
        .lines()
        .map(|line| {
            let (number, text) = line.split_once(": ")?;
            Some((number.parse::<u64>().ok()?, text))
        })
        .collect::<Option<_>>()?;
    let mut out = Vec::new();
    if rows.is_empty() {
        note(&mut out, "文件为空", width);
        return Some(out);
    }
    note(&mut out, &format!("第 {start}–{end} 行"), width);
    let digits = end.to_string().len().max(MIN_LINE_NUMBER_WIDTH);
    for (number, text) in rows.iter().take(if expanded { usize::MAX } else { 4 }) {
        source(&mut out, *number, digits, text, None, expanded, width);
    }
    if !expanded && rows.len() > 4 {
        note(&mut out, &format!("… 另有 {} 行", rows.len() - 4), width);
    }
    if expanded
        || rows.len() > 4
        || rows.iter().any(|(_, text)| {
            safe_output(text).width() > usize::from(width).saturating_sub(digits + 7)
        })
    {
        out.push(toggle_line(expanded, width));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn overview_preview_has_readable_evidence_and_expand_controls() {
        let value = json!({"inventory":{"files":3,"languages":{"Rust":2},"directories":[{"path":"src","files":2}]},
            "manifests":[{"path":"Cargo.toml","package":"example","parsed":true,
                "verification_candidates":[{"argv":["cargo","test"]}]}],
            "test_files":["tests/check.rs"],"instruction_files":[],
            "symbols":[{"path":"src/lib.rs","line":3,"name":"parse"}],
            "truncated":{"scan":false,"content":false,"output":false}});
        let tool = row("project_overview", value);
        for width in [1, 20, 80] {
            for expanded in [false, true] {
                assert!(
                    lines(&tool, expanded, width)
                        .unwrap()
                        .iter()
                        .all(|line| line.width() <= usize::from(width))
                );
            }
        }
        let expanded = lines(&tool, true, 80)
            .unwrap()
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(expanded.contains("待验证入口"));
        assert!(expanded.contains("src/lib.rs:3  parse"));
        assert!(expanded.contains("[收起]"));
        assert!(
            lines(&tool, false, 80)
                .unwrap()
                .last()
                .unwrap()
                .to_string()
                .contains("[展开]")
        );
    }

    fn row(name: &str, output: Value) -> ToolRow {
        ToolRow {
            id: "display-test".into(),
            name: name.into(),
            arguments: "{}".into(),
            status: ToolStatus::Finished,
            output: output.to_string(),
        }
    }

    #[test]
    fn memory_save_shows_title_and_scoped_preview_without_changing_the_result() {
        let content = format!(
            "审查结果：{}\n第二行\n第三行\n第四行",
            "中文内容".repeat(50)
        );
        let mut tool = row(
            "memory_save",
            json!({"memory":{
            "id":12,"active":true,"category":"fact","title":"审查记录","content":content
        },"source_kind":"session_memory"}),
        );
        tool.arguments =
            json!({"title":"审查记录","category":"fact","content":content}).to_string();
        let original = tool.output.clone();
        assert_eq!(memory_summary(&tool.arguments), "审查记录");
        let preview = lines(&tool, false, 60).unwrap();
        assert!(
            preview[0]
                .to_string()
                .contains("已保存 · 事实 · 当前会话 · #12")
        );
        assert!(preview.last().unwrap().to_string().contains("[展开]"));
        for width in [0, 1, 5, 30, 80] {
            for expanded in [false, true] {
                assert!(
                    lines(&tool, expanded, width)
                        .unwrap()
                        .iter()
                        .all(|line| line.width() <= width as usize)
                );
            }
        }
        let expanded = lines(&tool, true, 60).unwrap();
        let actual: String = expanded[1..expanded.len() - 1]
            .iter()
            .map(|line| line.to_string().strip_prefix("    ").unwrap().to_owned())
            .collect();
        assert_eq!(actual, content.lines().collect::<String>());
        assert_eq!(tool.output, original);
    }

    fn highlighted(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|line| &line.spans)
            .filter(|span| span.style == super::super::markdown::mark_style(Style::default()))
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn search_title_includes_query_and_scope_even_before_results_arrive() {
        let mut tool = row("search_text", json!({"matches":[]}));
        tool.arguments =
            json!({"query":"workspace snapshot", "path":"crates/orbit-tools/src"}).to_string();
        tool.status = ToolStatus::Running;
        let view = super::super::render::transcript_lines_at_width(
            &[super::super::app::TranscriptItem::Tool(tool)],
            false,
            120,
        );
        assert!(
            view[0]
                .to_string()
                .contains("search_text \"workspace snapshot\" crates/orbit-tools/src")
        );
        assert_eq!(search_summary(r#"{"query":"foo"}"#), "\"foo\" .");
    }

    #[test]
    fn highlighting_follows_literal_regex_case_and_unicode_search_rules() {
        for (config, text, expected) in [
            (json!({"query":"a.b"}), "a.b A.B axb", "a.bA.B"),
            (
                json!({"query":"a.b", "case_sensitive":true}),
                "a.b A.B axb",
                "a.b",
            ),
            (
                json!({"query":"a.b", "regex":true}),
                "a.b A.B axb",
                "a.bA.Baxb",
            ),
            (
                json!({"query":r"中文\s+value", "regex":true}),
                "前缀：中文\tVALUE 后缀",
                "中文    VALUE",
            ),
        ] {
            let pattern = matcher(&config).unwrap();
            let mut view = vec![];
            source(&mut view, 1, 4, text, Some(&pattern), true, 80);
            assert_eq!(highlighted(&view), expected);
        }
        assert!(matcher(&json!({"query":"[", "regex":true})).is_none());
    }

    #[test]
    fn highlight_survives_wrapping_and_focuses_long_line_previews() {
        let pattern = matcher(&json!({"query":"^ABCDEFGH$", "regex":true})).unwrap();
        let mut view = vec![];
        source(&mut view, 123, 4, "ABCDEFGH", Some(&pattern), true, 15);
        assert_eq!(view.len(), 2);
        assert_eq!(highlighted(&view), "ABCDEFGH");
        assert!(view.iter().all(|line| line.width() <= 15));

        let pattern = matcher(&json!({"query":"TARGET"})).unwrap();
        let text = format!("{} TARGET {}", "中文".repeat(50), "suffix".repeat(10));
        for width in [11, 12, 32, 80] {
            let mut view = vec![];
            source(&mut view, 1, 4, &text, Some(&pattern), false, width);
            assert!(view.iter().all(|line| line.width() <= usize::from(width)));
            if width >= 32 {
                assert_eq!(highlighted(&view), "TARGET");
            }
        }
    }

    #[test]
    fn search_groups_files_and_merges_overlapping_context_without_mutating_results() {
        let row = row(
            "search_text",
            json!({"matches": [
            {"path":"src/main.rs", "line":12, "column":1, "text":"let first = 1;",
             "context":[{"line":11,"text":"fn main() {"}, {"line":13,"text":"let second = 2;"}]},
            {"path":"src/main.rs", "line":13, "column":1, "text":"let second = 2;",
             "context":[{"line":12,"text":"let first = 1;"}]},
            {"path":"src/main.rs", "line":13, "column":5, "text":"let second = 2;", "context":[]}
        ], "has_more":true, "scan_truncated":true}),
        );
        let original = row.output.clone();
        let view = lines(&row, true, 80).unwrap();
        let text = view
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("3 处匹配 · 1 个文件"));
        assert_eq!(text.matches("src/main.rs").count(), 1);
        assert_eq!(text.matches("let first = 1;").count(), 1);
        assert_eq!(text.matches("let second = 2;").count(), 1);
        assert!(text.find("11 │").unwrap() < text.find("12 │").unwrap());
        assert!(text.contains("当前结果未全部返回"));
        assert!(text.contains("部分文件尚未搜索"));
        assert!(!text.contains("\"matches\""));
        assert_eq!(row.output, original);
    }

    #[test]
    fn file_preview_clips_by_cells_and_expansion_keeps_source_and_gutter() {
        let source = "    中文👩‍💻e\u{301}\tlet value = 123456789;".repeat(3);
        let row = row(
            "read_file",
            json!({"content":format!("744: {source}\n745: end\n"),
            "start_line":744,"end_line":745,"total_lines":745,"truncated":false}),
        );
        for width in [0, 1, 9, 12, 32, 80] {
            for expanded in [false, true] {
                let view = lines(&row, expanded, width).unwrap();
                assert!(view.iter().all(|line| line.width() <= usize::from(width)));
            }
        }
        let collapsed = lines(&row, false, 32).unwrap();
        assert_eq!(collapsed.len(), 4);
        assert!(collapsed[1].to_string().ends_with('…'));
        let expanded = lines(&row, true, 32).unwrap();
        let reconstructed = expanded[1..expanded.len() - 2]
            .iter()
            .map(|line| {
                let text = line.to_string();
                assert!(text.starts_with("     744 │ ") || text.starts_with("         │ "));
                text.split_once("│ ").unwrap().1.to_owned()
            })
            .collect::<String>();
        assert_eq!(reconstructed, safe_output(&source));
        // File length must not shift the rail between adjacent tool previews.
        for end in [4, 92, 168, 1024] {
            let file = self::row(
                "read_file",
                json!({
                    "content":"1: first\n2: second\n", "start_line":1,
                    "end_line":end, "total_lines":end, "truncated":false
                }),
            );
            let view = lines(&file, false, 80).unwrap();
            assert_eq!(view[1].to_string(), "       1 │ first");
            assert_eq!(view[2].to_string(), "       2 │ second");
        }
    }

    #[test]
    fn empty_search_and_invalid_or_failed_results_stay_distinct() {
        let empty = row("search_text", json!({"matches":[]}));
        assert!(
            lines(&empty, false, 80).unwrap()[0]
                .to_string()
                .contains("0 处匹配")
        );
        let mut invalid = row("read_file", json!({"error":"cannot read"}));
        assert!(lines(&invalid, false, 80).is_none());
        invalid.name = "search_text".into();
        invalid.output = "tool execution failed: denied".into();
        invalid.status = ToolStatus::Failed;
        assert!(lines(&invalid, false, 80).is_none());
    }
}
