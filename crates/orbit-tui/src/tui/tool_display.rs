//! Presentation-only views. Never rewrite tool results used by the model or store.
mod structured;

use std::collections::BTreeMap;

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
    code::Renderer,
    icons,
    markdown::LinkedLine,
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

pub(super) fn web_search_summary(arguments: &str, output: Option<&str>) -> String {
    let args: Value = serde_json::from_str(arguments).unwrap_or_default();
    let query = args["query"].as_str().unwrap_or_default();
    let query_preview = super::render::compact_preview(query, 60);

    if let Some(out_str) = output
        && let Ok(out_val) = serde_json::from_str::<Value>(out_str)
        && let Some(provider) = out_val["provider"].as_str()
        && let Some(count) = out_val["results_returned"].as_u64()
    {
        format!("\"{query_preview}\" ({provider} · {count} 结果)")
    } else {
        format!("\"{query_preview}\"")
    }
}

pub(super) fn document_summary(arguments: &str, output: Option<&str>) -> String {
    let args: Value = serde_json::from_str(arguments).unwrap_or_default();
    let path = args["path"].as_str().unwrap_or_default();
    let path_preview = super::render::compact_preview(path, 60);

    if let Some(out_str) = output
        && let Ok(out_val) = serde_json::from_str::<Value>(out_str)
        && let Some(fmt) = out_val["format"].as_str()
    {
        format!("{path_preview} [{}]", fmt.to_ascii_uppercase())
    } else {
        path_preview
    }
}

pub(super) fn capability_summary(arguments: &str) -> String {
    let args: Value = serde_json::from_str(arguments).unwrap_or_default();
    let path = args["path"].as_str().unwrap_or_default();
    let access = args["access"].as_str().unwrap_or("read");
    let access_label = if access == "read_write" || access == "write" {
        "读写"
    } else {
        "只读"
    };
    format!("{access_label} · {path}")
}

pub(super) fn export_artifact_summary(arguments: &str, output: Option<&str>) -> String {
    let args: Value = serde_json::from_str(arguments).unwrap_or_default();
    let path = args["path"].as_str().unwrap_or_default();
    if let Some(out_str) = output
        && let Ok(out_val) = serde_json::from_str::<Value>(out_str)
        && let Some(bytes) = out_val["bytes"].as_u64()
    {
        let name = out_val["name"].as_str().unwrap_or(path);
        format!("{name} ({})", structured::format_bytes(bytes))
    } else {
        path.to_owned()
    }
}

pub(super) fn git_export_patch_summary(output: Option<&str>) -> String {
    if let Some(out_str) = output
        && let Ok(out_val) = serde_json::from_str::<Value>(out_str)
        && let Some(bytes) = out_val["bytes"].as_u64()
    {
        format!("补丁 ({})", structured::format_bytes(bytes))
    } else {
        "导出补丁".to_owned()
    }
}

pub(super) fn read_image_summary(arguments: &str) -> String {
    let args: Value = serde_json::from_str(arguments).unwrap_or_default();
    let path = args["path"].as_str().unwrap_or_default();
    super::render::compact_preview(path, 60)
}

fn summary_line(text: &str, limit: usize) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default();
    super::render::compact_preview(
        &line.split_whitespace().collect::<Vec<_>>().join(" "),
        limit,
    )
}

fn agent_read_target(output: &Value) -> String {
    output["agent"]["task"]
        .as_str()
        .filter(|task| !task.trim().is_empty())
        .map(|task| summary_line(task, 36))
        .unwrap_or_else(|| "子代理".into())
}

fn agent_read_detail(args: &Value, out: &Value) -> String {
    if args["agent_id"].as_str().is_none() {
        return out["total"]
            .as_u64()
            .map(|total| format!("{total} 个子任务"))
            .unwrap_or_else(|| "子任务列表".into());
    }

    let view = out["view"].as_str().or(args["view"].as_str());
    if out["result_available"] == false {
        return match out["status"].as_str() {
            Some("failed" | "cancelled" | "interrupted") => "任务未完成，没有最终结果",
            _ => "结果尚未生成",
        }
        .into();
    }

    let content = out["content"].as_str().unwrap_or_default();
    if content.is_empty() {
        return if out.is_null() {
            "读取中"
        } else {
            "没有后续记录"
        }
        .into();
    }

    if view == Some("result") {
        return format!("最终结果 · {}", summary_line(content, 100));
    }
    if let Ok(record) = serde_json::from_str::<Value>(content) {
        if let Some(calls) = record["tool_calls"]
            .as_array()
            .filter(|calls| !calls.is_empty())
        {
            let tools = calls
                .iter()
                .take(2)
                .filter_map(|call| {
                    let function = &call["function"];
                    let name = function["name"].as_str()?;
                    let summary =
                        tool_summary(name, function["arguments"].as_str().unwrap_or("{}"), "");
                    Some(format!("{name} {summary}").trim_end().to_owned())
                })
                .collect::<Vec<_>>()
                .join("；");
            if !tools.is_empty() {
                return format!("工具调用 · {}", summary_line(&tools, 100));
            }
        }

        if let Some(text) = record["content"].as_str() {
            let label = match record["role"].as_str() {
                Some("user") => "任务说明",
                Some("tool") => "工具结果",
                _ => "回答",
            };
            let result: Value = serde_json::from_str(text).unwrap_or_default();
            let text = result["content"].as_str().unwrap_or(text);
            return format!("{label} · {}", summary_line(text, 100));
        }
    }

    if content.trim_start().starts_with('{') {
        format!("执行记录片段 · 读取了 {} 字", content.chars().count())
    } else {
        format!("回答片段 · {}", summary_line(content, 100))
    }
}

fn agent_read_summary(arguments: &str, output: &str) -> String {
    let args: Value = serde_json::from_str(arguments).unwrap_or_default();
    let out: Value = serde_json::from_str(output).unwrap_or_default();
    let detail = agent_read_detail(&args, &out);
    if args["agent_id"].as_str().is_some() {
        format!("{} · {detail}", agent_read_target(&out))
    } else {
        detail
    }
}

pub(super) fn group_target_summary(row: &ToolRow) -> String {
    let args: Value = serde_json::from_str(&row.arguments).unwrap_or_default();
    match row.name.as_str() {
        "read_agent" if args["agent_id"].as_str().is_some() => {
            agent_read_target(&serde_json::from_str(&row.output).unwrap_or_default())
        }
        "read_agent" => "子任务列表".into(),
        "read_file" | "write_file" | "edit_file" | "read_image" | "read_document"
        | "export_artifact" | "request_capability" => {
            safe_output(args["path"].as_str().unwrap_or_default())
        }

        "list_files" | "project_overview" => {
            safe_output(args["path"].as_str().unwrap_or("当前目录"))
        }
        _ => tool_summary(&row.name, &row.arguments, &row.output),
    }
}

pub(super) fn group_call_summary(row: &ToolRow) -> String {
    if row.name == "read_agent" {
        agent_read_detail(
            &serde_json::from_str(&row.arguments).unwrap_or_default(),
            &serde_json::from_str(&row.output).unwrap_or_default(),
        )
    } else {
        tool_summary(&row.name, &row.arguments, &row.output)
    }
}

pub(super) fn tool_summary(name: &str, arguments: &str, output: &str) -> String {
    match name {
        "enter_plan" | "update_plan" | "submit_plan" => {
            let record = serde_json::from_str::<orbit_protocol::plan::State>(output).ok();
            let status = record.as_ref().and_then(|s| s.plan.as_ref()).map_or(
                if name == "submit_plan" {
                    "待确认"
                } else {
                    "只读规划"
                },
                |p| match p.status {
                    orbit_protocol::plan::Status::Draft => "草稿",
                    orbit_protocol::plan::Status::Review => "待确认",
                    orbit_protocol::plan::Status::Approved => "已批准",
                },
            );
            format!("{} · 查看当前计划", status)
        }
        "ask_user" => {
            let form = serde_json::from_str::<orbit_protocol::user_input::Form>(arguments).ok();
            let record = serde_json::from_str::<orbit_protocol::user_input::Record>(output).ok();
            let status = record.as_ref().map_or("待回答", |r| match r.status {
                orbit_protocol::user_input::Status::Pending => "待回答",
                orbit_protocol::user_input::Status::Answered => "已回答",
                orbit_protocol::user_input::Status::Declined => "已取消问答",
                orbit_protocol::user_input::Status::Cancelled => "任务已取消",
                orbit_protocol::user_input::Status::Interrupted => "任务已中断",
            });
            format!(
                "{} 个问题 · {} · 查看",
                form.map_or(0, |f| f.questions.len()),
                status
            )
        }
        "search_text" => search_summary(arguments),
        "memory_save" => memory_summary(arguments),
        "git_inspect" => git_summary(arguments),
        "read_file" => super::render::read_file_summary(arguments, output),
        "read_command_output" => command_output_summary(arguments),
        "web_search" => web_search_summary(arguments, Some(output)),
        "read_document" => document_summary(arguments, Some(output)),
        "request_capability" => capability_summary(arguments),
        "export_artifact" => export_artifact_summary(arguments, Some(output)),
        "git_export_patch" => git_export_patch_summary(Some(output)),
        "read_image" => read_image_summary(arguments),
        "read_agent" => agent_read_summary(arguments, output),
        _ => super::render::default_tool_summary(arguments),
    }
}

pub(super) fn write_file_preview(arguments: &str, width: u16) -> Option<Vec<Line<'static>>> {
    write_file_linked(arguments, width).map(|lines| lines.into_iter().map(Line::from).collect())
}

fn write_file_linked(arguments: &str, width: u16) -> Option<Vec<LinkedLine>> {
    let value: Value = serde_json::from_str(arguments).ok()?;
    let content = value["content"].as_str()?;
    let path = value["path"].as_str().unwrap_or_default();
    let lines: Vec<_> = content.lines().collect();
    let total_lines = lines.len();
    if total_lines == 0 {
        return Some(vec![LinkedLine::plain(Line::from(Span::styled(
            "(empty file)",
            Style::default().fg(SECONDARY),
        )))]);
    }

    let digits = total_lines.to_string().len().max(MIN_LINE_NUMBER_WIDTH);
    let mut renderer = Renderer::for_path(path);
    let mut out = Vec::new();
    for (index, line) in lines.into_iter().enumerate() {
        let num = (index + 1) as u64;
        source(
            &mut out,
            num,
            digits,
            line,
            None,
            &mut renderer,
            true,
            width,
        );
    }

    Some(out)
}

pub(super) fn edit_file_preview(arguments: &str, width: u16) -> Option<Vec<Line<'static>>> {
    let value: Value = serde_json::from_str(arguments).ok()?;
    let old_text = value["old_text"].as_str()?;
    let new_text = value["new_text"].as_str()?;
    let path = value["path"].as_str().unwrap_or_default();
    let digits = old_text
        .lines()
        .count()
        .max(new_text.lines().count())
        .to_string()
        .len()
        .max(MIN_LINE_NUMBER_WIDTH);
    let mut out = Vec::new();
    for (content, change) in [
        (old_text, super::code::Change::Removed),
        (new_text, super::code::Change::Added),
    ] {
        let mut renderer = Renderer::for_path(path);
        for (index, source) in content.lines().enumerate() {
            out.extend(renderer.render(
                source,
                super::code::Options {
                    gutter: super::code::Gutter::Diff {
                        number: index as u64 + 1,
                        digits,
                        indent: 4,
                    },
                    width: Some(width),
                    change: Some(change),
                    ..Default::default()
                },
            ));
        }
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

fn markdown_preview(content: &str, width: u16) -> Vec<LinkedLine> {
    super::markdown::document(content, width.saturating_add(2))
        .into_iter()
        .map(|mut linked| {
            linked.line.spans.insert(0, Span::raw("  "));
            linked.shift_offsets(2);
            linked
        })
        .collect()
}

fn web_fetch_preview(output: &str, width: u16) -> Option<Vec<LinkedLine>> {
    let (text, header_spans) = if let Ok(val) = serde_json::from_str::<Value>(output) {
        let content = val["content"].as_str().unwrap_or(output).to_owned();
        let url = val["url"]
            .as_str()
            .or_else(|| val["requested_url"].as_str())
            .unwrap_or_default();
        let start = val["start_char"].as_u64().unwrap_or(0);
        let total = val["total_chars"].as_u64().unwrap_or(0);
        let theme = &crate::tui::theme::THEME;
        let mut spans = vec![
            Span::styled(
                format!("{} ", icons::WEB),
                Style::default().fg(theme.accent),
            ),
            Span::styled(url.to_owned(), Style::default().fg(theme.md_link)),
        ];

        if total > 0 {
            let end = start + content.chars().count() as u64;
            spans.push(Span::styled(
                format!(" · 字符 {start}..{end} (共 {total})"),
                Style::default().fg(theme.tool_detail),
            ));
        }

        (content, Some(spans))
    } else {
        (output.to_owned(), None)
    };

    let trimmed = text.trim();
    let mut out = Vec::new();
    if let Some(header) = header_spans {
        out.push(LinkedLine::plain(Line::from(header)));
    }
    if trimmed.is_empty() {
        out.push(LinkedLine::plain(Line::from(Span::styled(
            "(empty response)",
            Style::default().fg(SECONDARY),
        ))));
        return Some(out);
    }

    out.extend(markdown_preview(trimmed, width));
    Some(out)
}

pub(super) fn agent_session_id(output: &str) -> Option<String> {
    let value: Value = serde_json::from_str(output).ok()?;
    let id = value["agent_id"].as_str()?;
    (id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())).then(|| id.to_owned())
}

pub(super) fn linked_lines(row: &ToolRow, expanded: bool, width: u16) -> Option<Vec<LinkedLine>> {
    if row.status != ToolStatus::Finished {
        return None;
    }
    match row.name.as_str() {
        "read_image" => {
            let value: Value = serde_json::from_str(&row.output).ok()?;
            let path = value["path"].as_str()?;
            let label = super::render::safe_output(path);
            let mut line = LinkedLine::plain(Line::from(label.clone()));
            line.media.push(super::media::Target {
                range: 0..label.len(),
                source: super::media::Source::Image(path.into()),
            });
            return Some(vec![line]);
        }

        "web_fetch" => return web_fetch_preview(&row.output, width),
        "read_document" => return structured::document_with_links(row, expanded, width),
        "read_file" => {
            return read_linked(
                &serde_json::from_str::<Value>(&row.output).ok()?,
                expanded,
                width,
            );
        }

        "write_file" => return write_file_linked(&row.arguments, width),
        "search_text" => {
            return search_linked(
                &serde_json::from_str::<Value>(&row.output).ok()?,
                expanded,
                width,
            );
        }

        _ => {}
    }

    lines(row, expanded, width).map(|lines| lines.into_iter().map(LinkedLine::plain).collect())
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

        "web_fetch" => web_fetch_preview(&row.output, width)
            .map(|lines| lines.into_iter().map(|linked| linked.line).collect()),
        "web_search" => structured::web_search(row, expanded, width),
        "read_command_output" => structured::command_output(row, expanded, width),
        "read_document" => structured::document(row, expanded, width),
        "request_capability" => structured::capability(row, expanded, width),
        "export_artifact" => structured::export_artifact(row, expanded, width),
        "git_export_patch" => structured::git_export_patch(row, expanded, width),
        "read_image" => structured::read_image(row, expanded, width),
        _ => structured::fallback(row, expanded, width),
    }
}

fn overview(value: &Value, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    let inventory = value["inventory"].as_object()?;
    let files_count = inventory.get("files").and_then(|v| v.as_u64()).unwrap_or(0);
    let languages = inventory.get("languages").and_then(|v| v.as_object());

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
        note(
            &mut out,
            &format!("{files_count} 个文件  {lang_summary}"),
            width,
        );
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
    let strong_style = Style::default()
        .fg(theme.strong)
        .add_modifier(Modifier::BOLD);
    let detail_style = Style::default().fg(theme.detail);
    let muted_style = Style::default().fg(theme.output_muted);
    let success_style = Style::default().fg(theme.success);
    let warn_style = Style::default().fg(theme.tool_running);

    let mut raw_lines: Vec<Line<'static>> = Vec::new();

    let push_line =
        |lines: &mut Vec<Line<'static>>, indent: usize, mut spans: Vec<Span<'static>>| {
            let prefix = " ".repeat(4 + indent * 2);
            spans.insert(0, Span::raw(prefix));
            lines.push(Line::from(spans));
        };

    let push_blank = |lines: &mut Vec<Line<'static>>| {
        lines.push(Line::from(""));
    };

    let mut header_spans = vec![Span::styled(format!("{files_count} 个文件"), strong_style)];
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

    if let Some(manifests) = value["manifests"].as_array().filter(|m| !m.is_empty()) {
        for manifest in manifests {
            push_blank(&mut raw_lines);
            let path = manifest["path"].as_str().unwrap_or_default();
            let mut manifest_spans = vec![
                Span::styled(
                    format!("{} ", icons::PACKAGE_VARIANT_CLOSED),
                    Style::default(),
                ),
                Span::styled(path.to_owned(), strong_style),
            ];
            if let Some(pkg) = manifest["package"].as_str().filter(|p| !p.is_empty()) {
                manifest_spans.push(Span::styled(format!("  {pkg}"), accent_style));
            }
            if let Some(system) = manifest["build_system"]
                .as_str()
                .or_else(|| manifest["package_manager"].as_str())
            {
                manifest_spans.push(Span::styled(format!(" [{system}]"), detail_style));
            }

            push_line(&mut raw_lines, 0, manifest_spans);

            if let Some(members) = manifest
                .get("workspace_members")
                .or_else(|| manifest.get("workspaces"))
            {
                let member_strs = if let Some(arr) = members.as_array() {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                } else if let Some(s) = members.as_str() {
                    s.to_string()
                } else {
                    members.to_string()
                };

                if !member_strs.is_empty() {
                    push_line(
                        &mut raw_lines,
                        1,
                        vec![
                            Span::styled("工作区成员: ", detail_style),
                            Span::styled(member_strs, detail_style),
                        ],
                    );
                }
            }

            if let Some(scripts) = manifest.get("declared_scripts").and_then(|v| v.as_object())
                && !scripts.is_empty()
            {
                push_line(
                    &mut raw_lines,
                    1,
                    vec![Span::styled("声明的脚本:", detail_style)],
                );
                for (name, cmd) in scripts {
                    let cmd_str = cmd
                        .as_str()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| cmd.to_string());
                    push_line(
                        &mut raw_lines,
                        2,
                        vec![
                            Span::styled(format!("{name:<12}"), accent_style),
                            Span::styled("→ ", muted_style),
                            Span::styled(cmd_str, detail_style),
                        ],
                    );
                }
            } else if let Some(scripts) = manifest.get("declared_scripts").filter(|v| !v.is_null())
            {
                push_line(
                    &mut raw_lines,
                    1,
                    vec![Span::styled(format!("声明的脚本：{scripts}"), detail_style)],
                );
            }

            if let Some(candidates) = manifest
                .get("verification_candidates")
                .and_then(|v| v.as_array())
            {
                let mut candidate_cmds = Vec::new();
                for cand in candidates {
                    if let Some(argv) = cand.get("argv").and_then(|v| v.as_array()) {
                        let cmd = argv
                            .iter()
                            .filter_map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(" ");
                        if !cmd.is_empty() {
                            candidate_cmds.push(cmd);
                        }
                    } else if let Some(s) = cand.as_str() {
                        candidate_cmds.push(s.to_string());
                    }
                }

                if !candidate_cmds.is_empty() {
                    push_line(
                        &mut raw_lines,
                        1,
                        vec![
                            Span::styled("待验证入口: ", detail_style),
                            Span::styled(candidate_cmds.join(" · "), success_style),
                        ],
                    );
                }
            } else if let Some(cand) = manifest
                .get("verification_candidates")
                .filter(|v| !v.is_null())
            {
                push_line(
                    &mut raw_lines,
                    1,
                    vec![Span::styled(format!("待验证入口：{cand}"), detail_style)],
                );
            }

            if let Some(deps) = manifest.get("dependencies").and_then(|v| v.as_object())
                && !deps.is_empty()
            {
                let total = deps.len();
                let dep_names: Vec<_> = deps.keys().take(12).map(|s| s.as_str()).collect();
                let extra = if total > 12 {
                    format!(" (+{} 个)", total - 12)
                } else {
                    String::new()
                };

                push_line(
                    &mut raw_lines,
                    1,
                    vec![
                        Span::styled(format!("核心依赖 ({total}): "), detail_style),
                        Span::styled(format!("{}{extra}", dep_names.join(", ")), detail_style),
                    ],
                );
            }

            if manifest.get("parsed") == Some(&Value::Bool(false)) {
                push_line(
                    &mut raw_lines,
                    1,
                    vec![Span::styled("未解析构建配置，请读取原文件确认", warn_style)],
                );
            }

            if manifest.get("truncated") == Some(&Value::Bool(true)) {
                push_line(
                    &mut raw_lines,
                    1,
                    vec![Span::styled("部分配置已省略，请读取原文件", muted_style)],
                );
            }
        }
    }

    if let Some(dirs) = inventory
        .get("directories")
        .and_then(|v| v.as_array())
        .filter(|d| !d.is_empty())
    {
        push_blank(&mut raw_lines);
        push_line(
            &mut raw_lines,
            0,
            vec![Span::styled(
                format!("{} 目录结构:", icons::FOLDER_OPEN),
                strong_style,
            )],
        );
        let limit = 8;
        for dir in dirs.iter().take(limit) {
            let path = dir["path"].as_str().unwrap_or_default();
            let count = dir["files"].as_u64().unwrap_or(0);
            push_line(
                &mut raw_lines,
                1,
                vec![
                    Span::styled(format!("{path:<28}"), accent_style),
                    Span::styled(format!("{count} 个文件"), detail_style),
                ],
            );
        }

        if dirs.len() > limit {
            push_line(
                &mut raw_lines,
                1,
                vec![Span::styled(
                    format!("... 另有 {} 个目录", dirs.len() - limit),
                    muted_style,
                )],
            );
        }
    }

    if let Some(tests) = value
        .get("test_files")
        .and_then(|v| v.as_array())
        .filter(|t| !t.is_empty())
    {
        push_blank(&mut raw_lines);
        push_line(
            &mut raw_lines,
            0,
            vec![Span::styled(
                format!("{} 测试文件 ({}):", icons::TEST_TUBE, tests.len()),
                strong_style,
            )],
        );
        let limit = 8;
        for file in tests.iter().take(limit) {
            let path = file.as_str().unwrap_or_default();
            push_line(
                &mut raw_lines,
                1,
                vec![Span::styled(path.to_owned(), detail_style)],
            );
        }

        if tests.len() > limit {
            push_line(
                &mut raw_lines,
                1,
                vec![Span::styled(
                    format!("... 另有 {} 个测试文件", tests.len() - limit),
                    muted_style,
                )],
            );
        }
    }

    if let Some(instructions) = value
        .get("instruction_files")
        .and_then(|v| v.as_array())
        .filter(|i| !i.is_empty())
    {
        push_blank(&mut raw_lines);
        push_line(
            &mut raw_lines,
            0,
            vec![Span::styled(
                format!("{} 项目规范:", icons::CLIPBOARD_TEXT_OUTLINE),
                strong_style,
            )],
        );
        for file in instructions {
            let path = file.as_str().unwrap_or_default();
            push_line(
                &mut raw_lines,
                1,
                vec![Span::styled(path.to_owned(), success_style)],
            );
        }
    }

    if let Some(relevant) = value
        .get("relevant_files")
        .and_then(|v| v.as_array())
        .filter(|r| !r.is_empty())
    {
        push_blank(&mut raw_lines);
        push_line(
            &mut raw_lines,
            0,
            vec![Span::styled(
                format!("{} 任务相关:", icons::TARGET),
                strong_style,
            )],
        );
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

    if let Some(refs) = value
        .get("references")
        .and_then(|v| v.as_array())
        .filter(|r| !r.is_empty())
    {
        push_blank(&mut raw_lines);
        push_line(
            &mut raw_lines,
            0,
            vec![Span::styled(
                format!("{} 跨文件引用 (References):", icons::LINK_VARIANT),
                strong_style,
            )],
        );
        for item in refs.iter().take(6) {
            let path = item["path"].as_str().unwrap_or_default();
            let line = item.get("line").and_then(|l| l.as_u64());
            let name = item["name"].as_str().unwrap_or_default();
            let loc = line.map(|l| format!(":{l}")).unwrap_or_default();
            push_line(
                &mut raw_lines,
                1,
                vec![
                    Span::styled(format!("{path}{loc}  "), detail_style),
                    Span::styled(name.to_owned(), accent_style),
                ],
            );
        }
    }

    if let Some(symbols) = value
        .get("symbols")
        .and_then(|v| v.as_array())
        .filter(|s| !s.is_empty())
    {
        push_blank(&mut raw_lines);
        push_line(
            &mut raw_lines,
            0,
            vec![Span::styled(
                format!("{} 代码大纲 (AST):", icons::MAGNIFY),
                strong_style,
            )],
        );
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
            push_line(
                &mut raw_lines,
                1,
                vec![Span::styled(
                    format!("... 另有 {} 个定义", symbols.len() - limit),
                    muted_style,
                )],
            );
        }
    }

    if value["truncated"]
        .as_object()
        .is_some_and(|obj| obj.values().any(|v| v == true))
    {
        push_blank(&mut raw_lines);
        push_line(
            &mut raw_lines,
            0,
            vec![Span::styled(
                format!(
                    "{} 部分内容未纳入概览，可指定具体子目录范围继续查看",
                    icons::ALERT_OUTLINE
                ),
                warn_style,
            )],
        );
    }

    let mut out = Vec::new();
    if width <= 4 {
        for line in raw_lines {
            out.push(Line::from(clipped(&line.to_string(), usize::from(width))));
        }
    } else {
        for line in raw_lines {
            if line.spans.is_empty() || (line.spans.len() == 1 && line.spans[0].content.is_empty())
            {
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

fn note<T: From<LinkedLine>>(out: &mut Vec<T>, text: &str, width: u16) {
    out.push(T::from(LinkedLine::plain(Line::from(Span::styled(
        clipped(&format!("    {}", safe_output(text)), usize::from(width)),
        Style::default().fg(SECONDARY),
    )))));
}

#[allow(clippy::too_many_arguments)]
fn source<T: From<LinkedLine>>(
    out: &mut Vec<T>,
    number: u64,
    digits: usize,
    text: &str,
    matcher: Option<&Regex>,
    renderer: &mut Renderer,
    expanded: bool,
    width: u16,
) {
    out.extend(
        renderer
            .render_linked(
                text,
                super::code::Options {
                    gutter: super::code::Gutter::Number {
                        number,
                        digits,
                        indent: 4,
                    },
                    width: Some(width),
                    overflow: if expanded {
                        super::code::Overflow::Wrap
                    } else {
                        super::code::Overflow::Clip
                    },
                    highlights: matcher
                        .map(|matcher| {
                            matcher
                                .find_iter(text)
                                .filter(|hit| !hit.is_empty())
                                .map(|hit| hit.range())
                                .collect()
                        })
                        .unwrap_or_default(),
                    ..Default::default()
                },
            )
            .into_iter()
            .map(T::from),
    );
}

fn search(value: &Value, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    search_linked(value, expanded, width).map(|lines| lines.into_iter().map(Line::from).collect())
}

fn search_linked(value: &Value, expanded: bool, width: u16) -> Option<Vec<LinkedLine>> {
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
            out.push(LinkedLine::plain(Line::default()));
        }

        note(&mut out, path, width);
        let mut renderer = Renderer::for_path(path);
        let mut previous = None;

        for (line, (text, matched)) in rows {
            if !expanded && shown >= 3 {
                break;
            }

            if previous.is_some_and(|last| line > last + 1) {
                out.push(LinkedLine::plain(Line::default()));
                renderer = Renderer::for_path(path);
            }

            source(
                &mut out,
                line,
                digits,
                text,
                if matched { matcher.as_ref() } else { None },
                &mut renderer,
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
        out.push(LinkedLine::plain(toggle_line(expanded, width)));
    }

    Some(out)
}

fn read(value: &Value, expanded: bool, width: u16) -> Option<Vec<Line<'static>>> {
    read_linked(value, expanded, width).map(|lines| lines.into_iter().map(Line::from).collect())
}

fn read_linked(value: &Value, expanded: bool, width: u16) -> Option<Vec<LinkedLine>> {
    let content = value["content"].as_str()?;
    let path = value["path"].as_str().unwrap_or_default();
    let end = value["end_line"].as_u64().unwrap_or(1);
    let rows = read_rows(content)?;
    let mut out = Vec::new();

    if rows.is_empty() {
        note(&mut out, "文件为空", width);
        return Some(out);
    }

    let digits = end.to_string().len().max(MIN_LINE_NUMBER_WIDTH);
    let mut renderer = Renderer::for_path(path);

    for (number, text) in rows.iter().take(if expanded { usize::MAX } else { 4 }) {
        source(
            &mut out,
            *number,
            digits,
            text,
            None,
            &mut renderer,
            expanded,
            width,
        );
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
        out.push(LinkedLine::plain(toggle_line(expanded, width)));
    }

    Some(out)
}

fn read_rows(content: &str) -> Option<Vec<(u64, &str)>> {
    content
        .lines()
        .map(|line| {
            let (number, text) = line.split_once(": ")?;
            Some((number.parse::<u64>().ok()?, text))
        })
        .collect()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod subagent_tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn child_cards_expose_live_and_completed_session_links_at_narrow_widths() {
        let id = "00000000000000000000000000000001";
        for status in [ToolStatus::Running, ToolStatus::Finished] {
            let row=ToolRow{id:"orbit-child-call".into(),name:"spawn_agent".into(),arguments:json!({"task":"审查 Orbit 中文标题与很长的补充上下文"}).to_string(),output:json!({"agent_id":id,"role":"review","model":"orbit::model","status":if status==ToolStatus::Running {"running"} else {"completed"},"summary":"**检查完成**"}).to_string(),status};
            for width in [12, 30, 80] {
                let collapsed = super::super::render::tool_card_with_links(&row, false, width);
                assert_eq!(collapsed.len(), 1);
                assert!(collapsed[0].line.to_string().contains("查看"));
                assert!(collapsed[0].line.width() <= usize::from(width));
                let link = &collapsed[0].links[0];
                assert_eq!(link.url, format!("orbit://session/{id}"));
                assert_eq!(&collapsed[0].line.to_string()[link.start..link.end], "查看");
                let expanded = super::super::render::tool_card_with_links(&row, true, width);
                assert!(
                    expanded
                        .iter()
                        .flat_map(|l| &l.links)
                        .any(|l| l.url == format!("orbit://session/{id}"))
                );
            }
        }

        assert!(agent_session_id("{\"agent_id\":\"../orbit.db\"}").is_none());
    }
}
