//! Presentation-only views. Never rewrite tool results used by the model or store.
use std::collections::BTreeMap;

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
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

pub(super) fn lines(row: &ToolRow, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    if row.status != ToolStatus::Finished {
        return None;
    }
    match row.name.as_str() {
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
        _ => None,
    }
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
    matched: bool,
    expanded: bool,
    width: u16,
) {
    let prefix = format!("    {number:>digits$} │ ");
    let available = usize::from(width).saturating_sub(prefix.width());
    let text = safe_output(text);
    if available == 0 {
        note(out, &format!("{number}: {text}"), width);
        return;
    }
    let style = Style::default().fg(if matched { ACCENT } else { SECONDARY });
    let mut push = |text: String, continuation: bool| {
        out.push(Line::from(vec![
            Span::styled(
                if continuation {
                    format!("    {} │ ", " ".repeat(digits))
                } else {
                    prefix.clone()
                },
                style,
            ),
            Span::raw(text),
        ]));
    };
    if !expanded {
        push(clipped(&text, available), false);
        return;
    }
    // Wrap by display cells, preserving indentation and keeping wide/combining
    // graphemes intact. Continuations have a blank line-number gutter.
    let mut chunk = String::new();
    let mut used = 0;
    let mut continuation = false;
    for glyph in text.graphemes(true) {
        let size = glyph.width();
        if used + size > available && !chunk.is_empty() {
            push(std::mem::take(&mut chunk), continuation);
            continuation = true;
            used = 0;
        }
        if size > available {
            chunk.push('…');
            used += 1;
        } else {
            chunk.push_str(glyph);
            used += size;
        }
    }
    push(chunk, continuation);
}

fn search(value: &Value, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let matches = value["matches"].as_array()?;
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
        note(&mut out, path, width);
        let mut previous = None;
        for (line, (text, matched)) in rows {
            if !expanded && shown >= 3 {
                break;
            }
            if previous.is_some_and(|last| line > last + 1) {
                note(&mut out, "…", width);
            }
            source(&mut out, line, digits, text, matched, expanded, width);
            previous = Some(line);
            shown += 1;
        }
    }
    if shown < total_rows {
        note(
            &mut out,
            &format!("… 另有 {} 行匹配", total_rows - shown),
            width,
        );
    }
    if value["has_more"].as_bool() == Some(true) {
        note(&mut out, "还有更多匹配，当前结果未全部返回", width);
    }
    if value["scan_truncated"].as_bool() == Some(true) {
        note(&mut out, "搜索达到扫描上限，部分文件尚未搜索", width);
    }
    Some(out)
}

fn read(value: &Value, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let content = value["content"].as_str()?;
    let start = value["start_line"].as_u64()?;
    let end = value["end_line"].as_u64()?;
    let total = value["total_lines"].as_u64()?;
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
    note(
        &mut out,
        &format!(
            "第 {start}–{end} 行 · 本次 {} 行 · 全文 {total} 行",
            rows.len()
        ),
        width,
    );
    let digits = end.to_string().len().max(MIN_LINE_NUMBER_WIDTH);
    for (number, text) in rows.iter().take(if expanded { usize::MAX } else { 4 }) {
        source(&mut out, *number, digits, text, false, expanded, width);
    }
    if !expanded && rows.len() > 4 {
        note(&mut out, &format!("… 另有 {} 行", rows.len() - 4), width);
    }
    if value["truncated"].as_bool() == Some(true) {
        note(&mut out, "本次读取未返回完整文件", width);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        assert_eq!(collapsed.len(), 3);
        assert!(collapsed[1].to_string().ends_with('…'));
        let expanded = lines(&row, true, 32).unwrap();
        let reconstructed = expanded[1..expanded.len() - 1]
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
