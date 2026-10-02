//! Presentation-only views. Never rewrite tool results used by the model or store.
mod structured;

use std::{collections::BTreeMap, ops::Range};

use ratatui::{
    style::{Color, Modifier, Style},
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

const ACCENT: Color = crate::tui::theme::THEME.accent;
const SECONDARY: Color = crate::tui::theme::THEME.tool_detail;
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

use syntect::easy::HighlightLines;

pub(super) fn search_summary(arguments: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(arguments) else {
        return String::new();
    };

    let query = super::render::compact_preview(value["query"].as_str().unwrap_or_default(), 100);
    let path = super::render::compact_preview(value["path"].as_str().unwrap_or("."), 100);

    format!("\"{}\"({path})", query)
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

pub(super) fn git_summary(arguments: &str) -> String {
    structured::summary(arguments)
}

pub(super) fn command_output_summary(arguments: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(arguments) else {
        return String::new();
    };
    let output_id = value["output_id"].as_str().unwrap_or_default();
    if output_id.is_empty() {
        return String::new();
    }
    let stream = value["stream"].as_str().unwrap_or("stdout");
    let stream_suffix = if stream == "stderr" { " [stderr]" } else { "" };

    if let Some(seq_str) = output_id.rsplit('-').next()
        && let Ok(seq) = seq_str.parse::<u64>()
    {
        format!("终端 #{}{stream_suffix} ({output_id})", seq + 1)
    } else {
        format!("终端 {output_id}{stream_suffix}")
    }
}


pub(super) fn write_file_preview(arguments: &str, width: u16) -> Option<Vec<Line<'static>>> {
    let value: Value = serde_json::from_str(arguments).ok()?;
    let content = value["content"].as_str()?;
    let path = value["path"].as_str().unwrap_or_default();
    let lines: Vec<_> = content.lines().collect();
    let total_lines = lines.len();
    if total_lines == 0 {
        return Some(vec![Line::from(Span::styled(
            "(empty file)",
            Style::default().fg(SECONDARY),
        ))]);
    }
    let digits = total_lines.to_string().len().max(MIN_LINE_NUMBER_WIDTH);
    let syntax = super::languages::syntax_for_label(path);
    let mut out = Vec::new();
    for (index, line) in lines.into_iter().enumerate() {
        let num = (index + 1) as u64;
        source(&mut out, num, digits, line, None, syntax, true, width);
    }
    Some(out)
}

pub(super) fn edit_file_preview(arguments: &str, _width: u16) -> Option<Vec<Line<'static>>> {
    let value: Value = serde_json::from_str(arguments).ok()?;
    let old_text = value["old_text"].as_str()?;
    let new_text = value["new_text"].as_str()?;
    let mut out = Vec::new();
    for line in old_text.lines() {
        out.push(Line::from(Span::styled(
            format!("- {line}"),
            Style::default().fg(crate::tui::theme::THEME.diff_removed),
        )));
    }
    for line in new_text.lines() {
        out.push(Line::from(Span::styled(
            format!("+ {line}"),
            Style::default().fg(crate::tui::theme::THEME.diff_added),
        )));
    }
    Some(out)
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

fn push_unmarked_spans(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    range: Range<usize>,
    tokens: Option<&[(Range<usize>, syntect::highlighting::Style)]>,
) {
    if range.start >= range.end {
        return;
    }
    let Some(tokens) = tokens else {
        spans.push(Span::raw(text[range].to_owned()));
        return;
    };

    let mut cur = range.start;
    for (token_range, style) in tokens {
        if token_range.end <= cur || token_range.start >= range.end {
            continue;
        }
        let seg_start = cur.max(token_range.start);
        let seg_end = range.end.min(token_range.end);
        if seg_start < seg_end {
            let c = style.foreground;
            spans.push(Span::styled(
                text[seg_start..seg_end].to_owned(),
                Style::default().fg(Color::Rgb(c.r, c.g, c.b)),
            ));
            cur = seg_end;
        }
    }
    if cur < range.end {
        spans.push(Span::raw(text[cur..range.end].to_owned()));
    }
}

fn source_spans(
    text: &str,
    marks: &[Range<usize>],
    range: Range<usize>,
    tokens: Option<&[(Range<usize>, syntect::highlighting::Style)]>,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut offset = range.start;

    for mark in marks {
        let start = mark.start.max(range.start);
        let end = mark.end.min(range.end);

        if start >= end {
            continue;
        }

        if offset < start {
            push_unmarked_spans(&mut spans, text, offset..start, tokens);
        }

        spans.push(Span::styled(
            text[start..end].to_owned(),
            super::markdown::mark_style(Style::default()),
        ));
        offset = end;
    }

    if offset < range.end {
        push_unmarked_spans(&mut spans, text, offset..range.end, tokens);
    }

    spans
}

pub(super) fn web_fetch_preview(output: &str, _width: u16) -> Option<Vec<Line<'static>>> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return Some(vec![Line::from(Span::styled(
            "(empty response)",
            Style::default().fg(SECONDARY),
        ))]);
    }

    let mut out = Vec::new();
    let mut fence = None;
    for (index, line) in trimmed.lines().enumerate() {
        let mut rendered = super::markdown::line(line, index, &mut fence);
        if rendered.spans.first().is_some_and(|s| s.content == "  ") {
            rendered.spans.remove(0);
        }
        out.push(rendered);
    }
    Some(out)
}

pub(super) fn lines(row: &ToolRow, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    if row.status != ToolStatus::Finished {
        return None;
    }

    match row.name.as_str() {
        "write_file" => write_file_preview(&row.arguments, width),
        "edit_file" => edit_file_preview(&row.arguments, width),
        "list_files" => structured::files(row, expanded, width),
        "git_inspect" => structured::git(row, expanded, width),
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
        "web_fetch" => web_fetch_preview(&row.output, width),
        _ => structured::fallback(row, expanded, width),
    }
}

fn overview(value: &Value, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let inventory = value["inventory"].as_object()?;
    let files_count = inventory.get("files").and_then(|v| v.as_u64()).unwrap_or(0);
    let languages = inventory.get("languages").and_then(|v| v.as_object());

    // Build language summary string
    let lang_summary = if let Some(langs) = languages {
        let mut sorted: Vec<_> = langs.iter().collect();
        sorted.sort_by_key(|(_, c)| std::cmp::Reverse(c.as_u64().unwrap_or(0)));
        sorted
            .into_iter()
            .map(|(name, count)| format!("{name} {count}"))
            .collect::<Vec<_>>()
            .join(" / ")
    } else {
        String::new()
    };

    if !expanded {
        let mut out = Vec::new();
        note(&mut out, &format!("{files_count} 个文件  {lang_summary}"), width);
        if let Some(manifests) = value["manifests"].as_array() {
            for manifest in manifests.iter().take(3) {
                let path = manifest["path"].as_str().unwrap_or_default();
                let pkg = manifest["package"]
                    .as_str()
                    .map(|n| format!("  {n}"))
                    .unwrap_or_default();
                note(&mut out, &format!("{path}{pkg}"), width);
            }
        }
        out.push(toggle_line(expanded, width));
        return Some(out);
    }

    let theme = &crate::tui::theme::THEME;
    let accent_style = Style::default().fg(theme.accent);
    let strong_style = Style::default().fg(theme.strong).add_modifier(Modifier::BOLD);
    let detail_style = Style::default().fg(theme.detail);
    let muted_style = Style::default().fg(theme.output_muted);
    let success_style = Style::default().fg(theme.success);
    let warn_style = Style::default().fg(theme.tool_running);

    let mut raw_lines: Vec<Line<'static>> = Vec::new();

    let push_line = |lines: &mut Vec<Line<'static>>, indent: usize, mut spans: Vec<Span<'static>>| {
        let prefix = " ".repeat(4 + indent * 2);
        spans.insert(0, Span::raw(prefix));
        lines.push(Line::from(spans));
    };

    let push_blank = |lines: &mut Vec<Line<'static>>| {
        lines.push(Line::from(""));
    };

    // 1. Overview header
    let mut header_spans = vec![
        Span::styled(format!("{files_count} 个文件"), strong_style),
    ];
    if let Some(langs) = languages {
        let mut sorted: Vec<_> = langs.iter().collect();
        sorted.sort_by_key(|(_, c)| std::cmp::Reverse(c.as_u64().unwrap_or(0)));
        if !sorted.is_empty() {
            header_spans.push(Span::styled(" · ", muted_style));
            for (idx, (name, count)) in sorted.into_iter().enumerate() {
                if idx > 0 {
                    header_spans.push(Span::styled(" / ", muted_style));
                }
                header_spans.push(Span::styled(name.to_string(), accent_style));
                header_spans.push(Span::styled(format!(" {count}"), detail_style));
            }
        }
    }
    push_line(&mut raw_lines, 0, header_spans);

    // 2. Manifests
    if let Some(manifests) = value["manifests"].as_array().filter(|m| !m.is_empty()) {
        for manifest in manifests {
            push_blank(&mut raw_lines);
            let path = manifest["path"].as_str().unwrap_or_default();
            let mut manifest_spans = vec![
                Span::styled("📦 ", Style::default()),
                Span::styled(path.to_owned(), strong_style),
            ];
            if let Some(pkg) = manifest["package"].as_str().filter(|p| !p.is_empty()) {
                manifest_spans.push(Span::styled(format!("  {pkg}"), accent_style));
            }
            if let Some(system) = manifest["build_system"].as_str().or_else(|| manifest["package_manager"].as_str()) {
                manifest_spans.push(Span::styled(format!(" [{system}]"), detail_style));
            }
            push_line(&mut raw_lines, 0, manifest_spans);

            // Workspaces / Members
            if let Some(members) = manifest.get("workspace_members").or_else(|| manifest.get("workspaces")) {
                let member_strs = if let Some(arr) = members.as_array() {
                    arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", ")
                } else if let Some(s) = members.as_str() {
                    s.to_string()
                } else {
                    members.to_string()
                };
                if !member_strs.is_empty() {
                    push_line(&mut raw_lines, 1, vec![
                        Span::styled("工作区成员: ", detail_style),
                        Span::styled(member_strs, detail_style),
                    ]);
                }
            }

            // Declared scripts
            if let Some(scripts) = manifest.get("declared_scripts").and_then(|v| v.as_object())
                && !scripts.is_empty()
            {
                push_line(&mut raw_lines, 1, vec![
                    Span::styled("声明的脚本:", detail_style),
                ]);
                for (name, cmd) in scripts {
                    let cmd_str = cmd.as_str().map(|s| s.to_string()).unwrap_or_else(|| cmd.to_string());
                    push_line(&mut raw_lines, 2, vec![
                        Span::styled(format!("{name:<12}"), accent_style),
                        Span::styled("→ ", muted_style),
                        Span::styled(cmd_str, detail_style),
                    ]);
                }
            } else if let Some(scripts) = manifest.get("declared_scripts").filter(|v| !v.is_null()) {
                push_line(&mut raw_lines, 1, vec![
                    Span::styled(format!("声明的脚本：{scripts}"), detail_style),
                ]);
            }

            // Verification candidates
            if let Some(candidates) = manifest.get("verification_candidates").and_then(|v| v.as_array()) {
                let mut candidate_cmds = Vec::new();
                for cand in candidates {
                    if let Some(argv) = cand.get("argv").and_then(|v| v.as_array()) {
                        let cmd = argv.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>().join(" ");
                        if !cmd.is_empty() {
                            candidate_cmds.push(cmd);
                        }
                    } else if let Some(s) = cand.as_str() {
                        candidate_cmds.push(s.to_string());
                    }
                }
                if !candidate_cmds.is_empty() {
                    push_line(&mut raw_lines, 1, vec![
                        Span::styled("待验证入口: ", detail_style),
                        Span::styled(candidate_cmds.join(" · "), success_style),
                    ]);
                }
            } else if let Some(cand) = manifest.get("verification_candidates").filter(|v| !v.is_null()) {
                push_line(&mut raw_lines, 1, vec![
                    Span::styled(format!("待验证入口：{cand}"), detail_style),
                ]);
            }

            // Dependencies
            if let Some(deps) = manifest.get("dependencies").and_then(|v| v.as_object())
                && !deps.is_empty()
            {
                let total = deps.len();
                let dep_names: Vec<_> = deps.keys().take(12).map(|s| s.as_str()).collect();
                let extra = if total > 12 { format!(" (+{} 个)", total - 12) } else { String::new() };
                push_line(&mut raw_lines, 1, vec![
                    Span::styled(format!("核心依赖 ({total}): "), detail_style),
                    Span::styled(format!("{}{extra}", dep_names.join(", ")), detail_style),
                ]);
            }

            if manifest.get("parsed") == Some(&Value::Bool(false)) {
                push_line(&mut raw_lines, 1, vec![
                    Span::styled("未解析构建配置，请读取原文件确认", warn_style),
                ]);
            }

            if manifest.get("truncated") == Some(&Value::Bool(true)) {
                push_line(&mut raw_lines, 1, vec![
                    Span::styled("部分配置已省略，请读取原文件", muted_style),
                ]);
            }
        }
    }

    // 3. Directories
    if let Some(dirs) = inventory.get("directories").and_then(|v| v.as_array()).filter(|d| !d.is_empty()) {
        push_blank(&mut raw_lines);
        push_line(&mut raw_lines, 0, vec![
            Span::styled("📂 目录结构:", strong_style),
        ]);
        let limit = 8;
        for dir in dirs.iter().take(limit) {
            let path = dir["path"].as_str().unwrap_or_default();
            let count = dir["files"].as_u64().unwrap_or(0);
            push_line(&mut raw_lines, 1, vec![
                Span::styled(format!("{path:<28}"), accent_style),
                Span::styled(format!("{count} 个文件"), detail_style),
            ]);
        }
        if dirs.len() > limit {
            push_line(&mut raw_lines, 1, vec![
                Span::styled(format!("... 另有 {} 个目录", dirs.len() - limit), muted_style),
            ]);
        }
    }

    // 4. Tests
    if let Some(tests) = value.get("test_files").and_then(|v| v.as_array()).filter(|t| !t.is_empty()) {
        push_blank(&mut raw_lines);
        push_line(&mut raw_lines, 0, vec![
            Span::styled(format!("🧪 测试文件 ({}):", tests.len()), strong_style),
        ]);
        let limit = 8;
        for file in tests.iter().take(limit) {
            let path = file.as_str().unwrap_or_default();
            push_line(&mut raw_lines, 1, vec![
                Span::styled(path.to_owned(), detail_style),
            ]);
        }
        if tests.len() > limit {
            push_line(&mut raw_lines, 1, vec![
                Span::styled(format!("... 另有 {} 个测试文件", tests.len() - limit), muted_style),
            ]);
        }
    }

    // 5. Instruction files
    if let Some(instructions) = value.get("instruction_files").and_then(|v| v.as_array()).filter(|i| !i.is_empty()) {
        push_blank(&mut raw_lines);
        push_line(&mut raw_lines, 0, vec![
            Span::styled("📋 项目规范:", strong_style),
        ]);
        for file in instructions {
            let path = file.as_str().unwrap_or_default();
            push_line(&mut raw_lines, 1, vec![
                Span::styled(path.to_owned(), success_style),
            ]);
        }
    }

    // 6. Relevant files & references
    if let Some(relevant) = value.get("relevant_files").and_then(|v| v.as_array()).filter(|r| !r.is_empty()) {
        push_blank(&mut raw_lines);
        push_line(&mut raw_lines, 0, vec![
            Span::styled("🎯 任务相关:", strong_style),
        ]);
        for item in relevant.iter().take(6) {
            let path = item["path"].as_str().unwrap_or_default();
            let score = item.get("score").and_then(|s| s.as_u64());
            let mut spans = vec![Span::styled(path.to_owned(), accent_style)];
            if let Some(s) = score {
                spans.push(Span::styled(format!(" (匹配分 {s})"), detail_style));
            }
            push_line(&mut raw_lines, 1, spans);
        }
    }

    if let Some(refs) = value.get("references").and_then(|v| v.as_array()).filter(|r| !r.is_empty()) {
        push_blank(&mut raw_lines);
        push_line(&mut raw_lines, 0, vec![
            Span::styled("🔗 跨文件引用 (References):", strong_style),
        ]);
        for item in refs.iter().take(6) {
            let path = item["path"].as_str().unwrap_or_default();
            let line = item.get("line").and_then(|l| l.as_u64());
            let name = item["name"].as_str().unwrap_or_default();
            let loc = line.map(|l| format!(":{l}")).unwrap_or_default();
            push_line(&mut raw_lines, 1, vec![
                Span::styled(format!("{path}{loc}  "), detail_style),
                Span::styled(name.to_owned(), accent_style),
            ]);
        }
    }

    // 7. Symbols -> Code Outline (AST)
    if let Some(symbols) = value.get("symbols").and_then(|v| v.as_array()).filter(|s| !s.is_empty()) {
        push_blank(&mut raw_lines);
        push_line(&mut raw_lines, 0, vec![
            Span::styled("🔍 代码大纲 (AST):", strong_style),
        ]);
        let limit = 8;
        for symbol in symbols.iter().take(limit) {
            let path = symbol["path"].as_str().unwrap_or_default();
            let line = symbol.get("line").and_then(|l| l.as_u64()).unwrap_or(0);
            let name = symbol["name"].as_str().unwrap_or_default();
            let kind = symbol["kind"].as_str().unwrap_or_default();
            let signature = symbol["signature"].as_str().unwrap_or_default();

            let mut spans = vec![
                Span::styled(format!("{path}:{line}  "), detail_style),
                Span::styled(name.to_owned(), accent_style.add_modifier(Modifier::BOLD)),
            ];

            if !kind.is_empty() {
                spans.push(Span::styled(format!(" [{kind}]"), muted_style));
            }

            if !signature.is_empty() && signature != name {
                spans.push(Span::styled(format!("  {signature}"), detail_style));
            }

            push_line(&mut raw_lines, 1, spans);
        }
        if symbols.len() > limit {
            push_line(&mut raw_lines, 1, vec![
                Span::styled(format!("... 另有 {} 个定义", symbols.len() - limit), muted_style),
            ]);
        }
    }

    // 8. Truncation notice
    if value["truncated"].as_object().is_some_and(|obj| obj.values().any(|v| v == true)) {
        push_blank(&mut raw_lines);
        push_line(&mut raw_lines, 0, vec![
            Span::styled("⚠️ 部分内容未纳入概览，可指定具体子目录范围继续查看", warn_style),
        ]);
    }

    let mut out = Vec::new();
    if width <= 4 {
        for line in raw_lines {
            out.push(Line::from(clipped(&line.to_string(), usize::from(width))));
        }
    } else {
        for line in raw_lines {
            if line.spans.is_empty() || (line.spans.len() == 1 && line.spans[0].content.is_empty()) {
                out.push(Line::from(""));
            } else {
                let (wrapped, _) = super::transcript::wrap_line(&line, width);
                for sub in wrapped {
                    if sub.width() > usize::from(width) {
                        out.push(Line::from(clipped(&sub.to_string(), usize::from(width))));
                    } else {
                        out.push(sub);
                    }
                }
            }
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

pub(super) fn clipped(text: &str, width: usize) -> String {
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

#[allow(clippy::too_many_arguments)]
fn source(
    out: &mut Vec<Line<'static>>,
    number: u64,
    digits: usize,
    text: &str,
    matcher: Option<&Regex>,
    syntax: Option<&syntect::parsing::SyntaxReference>,
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

    let tokens: Option<Vec<(Range<usize>, syntect::highlighting::Style)>> = syntax.and_then(|syn| {
        let mut highlighter = HighlightLines::new(syn, &crate::tui::theme::CODE_THEME);
        let line_tokens = highlighter.highlight_line(&text, &super::languages::SYNTAXES).ok()?;
        let mut ranges = Vec::new();
        let mut offset = 0;
        for (style, slice) in line_tokens {
            let end = offset + slice.len();
            ranges.push((offset..end, style));
            offset = end;
        }
        Some(ranges)
    });

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

        spans.extend(source_spans(&text, &marks, range, tokens.as_deref()));

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
        let syntax = super::languages::syntax_for_label(path);
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
                syntax,
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
    let path = value["path"].as_str().unwrap_or_default();
    let end = value["end_line"].as_u64().unwrap_or(1);
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

    let digits = end.to_string().len().max(MIN_LINE_NUMBER_WIDTH);
    let syntax = super::languages::syntax_for_label(path);

    for (number, text) in rows.iter().take(if expanded { usize::MAX } else { 4 }) {
        source(&mut out, *number, digits, text, None, syntax, expanded, width);
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
        let value = json!({
            "inventory": {
                "files": 3,
                "languages": { "Rust": 2 },
                "directories": [{ "path": "src", "files": 2 }]
            },
            "manifests": [{ "path": "Cargo.toml", "package": "example", "parsed": true, "verification_candidates": [{ "argv": ["cargo", "test"] }] }],
            "test_files": ["tests/check.rs"],
            "instruction_files": [],
            "symbols": [{ "path": "src/lib.rs", "line": 3, "name": "parse" }],
            "truncated": { "scan": false, "content": false, "output": false }
        });
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
            json!({
                "memory": {
                    "id": 12,
                    "active": true,
                    "category": "fact",
                    "title": "审查记录",
                    "content": content
                },
                "source_kind": "session_memory"
            }),
        );
        tool.arguments =
            json!({ "title": "审查记录", "category": "fact", "content": content }).to_string();
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
        let mut tool = row("search_text", json!({ "matches": [] }));
        tool.arguments =
            json!({ "query": "workspace snapshot", "path": "crates/orbit-tools/src" }).to_string();
        tool.status = ToolStatus::Running;
        let view = super::super::render::transcript_lines_at_width(
            &[super::super::app::TranscriptItem::Tool(tool)],
            false,
            120,
        );

        assert!(
            view[0]
                .to_string()
                .contains("search_text \"workspace snapshot\"(crates/orbit-tools/src)")
        );
        assert_eq!(search_summary(r#"{"query":"foo"}"#), "\"foo\"(.)");
    }

    #[test]
    fn highlighting_follows_literal_regex_case_and_unicode_search_rules() {
        for (config, text, expected) in [
            (json!({ "query": "a.b" }), "a.b A.B axb", "a.bA.B"),
            (
                json!({ "query": "a.b", "case_sensitive": true }),
                "a.b A.B axb",
                "a.b",
            ),
            (
                json!({ "query": "a.b", "regex": true }),
                "a.b A.B axb",
                "a.bA.Baxb",
            ),
            (
                json!({ "query": r"中文\s+value", "regex": true }),
                "前缀：中文\tVALUE 后缀",
                "中文    VALUE",
            ),
        ] {
            let pattern = matcher(&config).unwrap();
            let mut view = vec![];
            source(&mut view, 1, 4, text, Some(&pattern), None, true, 80);

            assert_eq!(highlighted(&view), expected);
        }

        assert!(matcher(&json!({ "query": "[", "regex": true })).is_none());
    }

    #[test]
    fn highlight_survives_wrapping_and_focuses_long_line_previews() {
        let pattern = matcher(&json!({ "query": "^ABCDEFGH$", "regex": true })).unwrap();
        let mut view = vec![];
        source(&mut view, 123, 4, "ABCDEFGH", Some(&pattern), None, true, 15);

        assert_eq!(view.len(), 2);
        assert_eq!(highlighted(&view), "ABCDEFGH");
        assert!(view.iter().all(|line| line.width() <= 15));

        let pattern = matcher(&json!({ "query": "TARGET" })).unwrap();
        let text = format!("{} TARGET {}", "中文".repeat(50), "suffix".repeat(10));

        for width in [11, 12, 32, 80] {
            let mut view = vec![];
            source(&mut view, 1, 4, &text, Some(&pattern), None, false, width);

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
            json!({
                "matches": [{
                    "path": "src/main.rs",
                    "line": 12,
                    "column": 1,
                    "text": "let first = 1;",
                    "context": [{ "line": 11, "text": "fn main() {" }, { "line": 13, "text": "let second = 2;" }]
                }, {
                    "path": "src/main.rs",
                    "line": 13,
                    "column": 1,
                    "text": "let second = 2;",
                    "context": [{ "line": 12, "text": "let first = 1;" }]
                }, {
                    "path": "src/main.rs",
                    "line": 13,
                    "column": 5,
                    "text": "let second = 2;",
                    "context": []
                }],
                "has_more": true,
                "scan_truncated": true
            }),
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
            json!({
                "content": format!("744: {source}\n745: end\n"),
                "start_line": 744,
                "end_line": 745,
                "total_lines": 745,
                "truncated": false
            }),
        );

        for width in [0, 1, 9, 12, 32, 80] {
            for expanded in [false, true] {
                let view = lines(&row, expanded, width).unwrap();

                assert!(view.iter().all(|line| line.width() <= usize::from(width)));
            }
        }

        let collapsed = lines(&row, false, 32).unwrap();

        assert_eq!(collapsed.len(), 3);
        assert!(collapsed[0].to_string().ends_with('…'));

        let expanded = lines(&row, true, 32).unwrap();
        let reconstructed = expanded[..expanded.len() - 2]
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
                    "content": "1: first\n2: second\n",
                    "start_line": 1,
                    "end_line": end,
                    "total_lines": end,
                    "truncated": false
                }),
            );
            let view = lines(&file, false, 80).unwrap();

            assert_eq!(view[0].to_string(), "       1 │ first");
            assert_eq!(view[1].to_string(), "       2 │ second");
        }
    }

    #[test]
    fn empty_search_and_invalid_or_failed_results_stay_distinct() {
        let empty = row("search_text", json!({ "matches": [] }));

        assert!(
            lines(&empty, false, 80).unwrap()[0]
                .to_string()
                .contains("0 处匹配")
        );

        let mut invalid = row("read_file", json!({ "error": "cannot read" }));

        assert!(lines(&invalid, false, 80).is_none());

        invalid.name = "search_text".into();
        invalid.output = "tool execution failed: denied".into();
        invalid.status = ToolStatus::Failed;

        assert!(lines(&invalid, false, 80).is_none());
    }

    #[test]
    fn write_file_preview_shows_line_numbers() {
        let args = json!({
            "path": "hello.py",
            "content": "print(\"Hello, world!\")\nprint(42)"
        })
        .to_string();
        let preview = write_file_preview(&args, 80).expect("preview should succeed");
        assert_eq!(preview.len(), 2);
        assert_eq!(preview[0].to_string(), "       1 │ print(\"Hello, world!\")");
        assert_eq!(preview[1].to_string(), "       2 │ print(42)");
    }

    #[test]
    fn edit_file_preview_shows_diff_lines() {
        let args = json!({
            "path": "test.txt",
            "old_text": "old line",
            "new_text": "new line"
        })
        .to_string();
        let preview = edit_file_preview(&args, 80).expect("preview should succeed");
        assert_eq!(preview.len(), 2);
        assert_eq!(preview[0].to_string(), "- old line");
        assert_eq!(preview[1].to_string(), "+ new line");
    }

    #[test]
    fn command_output_summary_displays_terminal_number_and_id() {
        let args1 = json!({
            "output_id": "cmd-12345-67890-0",
            "stream": "stdout"
        })
        .to_string();
        assert_eq!(
            command_output_summary(&args1),
            "终端 #1 (cmd-12345-67890-0)"
        );

        let args2 = json!({
            "output_id": "cmd-12345-67890-1",
            "stream": "stderr"
        })
        .to_string();
        assert_eq!(
            command_output_summary(&args2),
            "终端 #2 [stderr] (cmd-12345-67890-1)"
        );

        let args3 = json!({
            "output_id": "cmd-custom"
        })
        .to_string();
        assert_eq!(
            command_output_summary(&args3),
            "终端 cmd-custom"
        );
    }
}
