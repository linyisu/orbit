use std::ops::Range;

use ratatui::{
    style::Style,
    text::{Line, Span},
};
use serde_json::{Value, json};

use super::{
    app::{ToolRow, ToolStatus, TranscriptItem},
    markdown::LinkedLine,
    theme::THEME,
};

#[cfg(test)]
mod tests;

fn candidate(item: &TranscriptItem) -> Option<(&ToolRow, Value)> {
    match item {
        TranscriptItem::Tool(row)
            if row.status != ToolStatus::Failed && !super::subagents::is_task(&row.name) =>
        {
            target(row).map(|target| (row, target))
        }

        _ => None,
    }
}

// Pagination changes the slice being read, not the object the call operates on.
fn target(row: &ToolRow) -> Option<Value> {
    let args: Value = serde_json::from_str(&row.arguments).ok()?;
    args.as_object()?;
    let path = || {
        args["path"]
            .as_str()
            .filter(|path| !path.is_empty())
    };
    let ids = |key: &str| {
        let mut ids = args[key]
            .as_array()?
            .iter()
            .map(|id| id.as_str().filter(|id| !id.is_empty()))
            .collect::<Option<Vec<_>>>()?;
        if ids.is_empty() {
            return None;
        }
        ids.sort_unstable();
        ids.dedup();
        Some(json!(ids))
    };
    Some(match row.name.as_str() {
        "read_file" | "write_file" | "edit_file" | "read_image" | "read_document" => {
            json!(path()?)
        }
        "request_capability" => json!([path()?, args["access"].as_str()?]),
        "list_files" => json!([
            args["path"].as_str().unwrap_or("."),
            args["depth"].as_u64().unwrap_or(1),
            args["include_hidden"]
                .as_bool()
                .unwrap_or(false)
        ]),

        "project_overview" | "code_symbols" => json!([
            args["path"].as_str().unwrap_or("."),
            args["query"]
                .as_str()
                .unwrap_or_default()
        ]),

        "search_text" => json!([
            args["path"].as_str().unwrap_or("."),
            args["query"].as_str()?,
            args["glob"],
            args["regex"].as_bool().unwrap_or(false),
            args["case_sensitive"]
                .as_bool()
                .unwrap_or(false),
            args["include_hidden"]
                .as_bool()
                .unwrap_or(false)
        ]),

        "web_search" => {
            args["query"].as_str()?;
            let mut key = args;
            key.as_object_mut()?
                .remove("max_results");
            key
        }

        "web_fetch" => json!(args["url"].as_str()?),
        "read_agent" => match args.get("agent_id") {
            None | Some(Value::Null) => json!(["list"]),
            Some(Value::String(id)) if !id.is_empty() => json!(["agent", id]),
            _ => return None,
        },

        "wait_agents" | "cancel_agents" => {
            json!([ids("agent_ids")?, args["all"].as_bool().unwrap_or(false)])
        }
        "read_command_output" => json!([
            args["output_id"].as_str()?,
            args["stream"]
                .as_str()
                .unwrap_or("stdout")
        ]),

        "get_command_status" | "wait_commands" | "cancel_commands" => {
            json!([ids("job_ids")?, args["mode"].as_str().unwrap_or("any")])
        }
        "run_command" => json!([
            args["command"].as_str()?,
            args["cwd"].as_str().unwrap_or("."),
            args["workspace_access"]
                .as_str()
                .unwrap_or("read"),
            args["network"]
                .as_str()
                .unwrap_or("deny"),
            args["background"]
                .as_bool()
                .unwrap_or(false)
        ]),

        "git_inspect" => json!([
            args["view"]
                .as_str()
                .unwrap_or("status"),
            args["path"],
            args["staged"]
                .as_bool()
                .unwrap_or(false)
        ]),

        "git_push" => json!([
            args["remote_url"].as_str()?,
            args["source_branch"].as_str()?,
            args["target_branch"].as_str()?
        ]),

        "read_history" => json!([args["archive_id"].as_i64()?, args["query"]]),
        "memory_search" => json!(args["query"].as_str()?),
        "memory_list" | "task_list" => args,
        "task_update" if args["action"] == "update" => json!(args["id"].as_i64()?),
        _ => return None,
    })
}

pub(super) fn start(items: &[TranscriptItem], index: usize) -> usize {
    let Some((row, target)) = items.get(index).and_then(candidate) else {
        return index;
    };
    let mut start = index;
    while start > 0
        && candidate(&items[start - 1])
            .is_some_and(|(other, key)| other.name == row.name && key == target)
    {
        start -= 1;
    }

    start
}

pub(super) fn range(items: &[TranscriptItem], index: usize) -> Range<usize> {
    let Some((row, target)) = items.get(index).and_then(candidate) else {
        return index..index + 1;
    };
    let mut end = index + 1;
    while end < items.len()
        && candidate(&items[end])
            .is_some_and(|(other, key)| other.name == row.name && key == target)
    {
        end += 1;
    }

    index..end
}

pub(super) fn header(items: &[TranscriptItem], width: u16, framed: bool) -> LinkedLine {
    let TranscriptItem::Tool(first) = &items[0] else {
        unreachable!()
    };
    let running = items
        .iter()
        .filter(|item| {
            matches!(
                item,
                TranscriptItem::Tool(row) if row.status == ToolStatus::Running
            )
        })
        .count();
    let content_width = if framed {
        width.saturating_sub(4)
    } else {
        width
    };
    let mut spans = vec![
        Span::raw(if framed { "" } else { "  " }),
        Span::styled(
            super::render::safe_output(&first.name),
            Style::default().fg(if running == 0 {
                THEME.tool_finished
            } else {
                THEME.tool_running
            }),
        ),
    ];
    let mut suffix = Vec::new();
    if running > 0 {
        suffix.push(Span::styled(
            format!(" · {running} 进行中"),
            Style::default().fg(THEME.tool_running),
        ));
    }

    suffix.push(Span::styled(
        format!(" · {} 次调用", items.len()),
        Style::default().fg(THEME.tool_summary),
    ));
    let available = usize::from(content_width).saturating_sub(
        spans
            .iter()
            .chain(&suffix)
            .map(Span::width)
            .sum::<usize>()
            + 3,
    );
    let target = super::tool_display::group_target_summary(first, available);
    if !target.is_empty() && available > 0 {
        let tail = matches!(
            first.name.as_str(),
            "read_file"
                | "write_file"
                | "edit_file"
                | "read_image"
                | "read_document"
                | "request_capability"
                | "list_files"
                | "project_overview"
                | "code_symbols"
        );
        let target = super::render::fit_label(&target, available, tail);
        spans.push(Span::styled(
            format!(" · {target}"),
            Style::default().fg(THEME.tool_summary),
        ));
    }

    spans.extend(suffix);
    let header = LinkedLine::plain(Line::from(spans))
        .wrap(content_width)
        .into_iter()
        .next()
        .unwrap_or_else(|| LinkedLine::plain(Line::default()));

    if framed {
        super::render::tool_frame_line(header, width)
    } else {
        header
    }
}

pub(super) struct Section {
    pub lines: Vec<LinkedLine>,
    pub header: usize,
    pub body: Range<usize>,
}

pub(super) fn section(
    row: &ToolRow,
    expanded: bool,
    width: u16,
    first: bool,
    last: bool,
) -> Section {
    if width < 10 {
        return Section {
            lines: super::render::tool_card_with_links(row, expanded, width),
            header: 0,
            body: 0..0,
        };
    }

    let inner_width = width - 4;
    let mut lines = Vec::new();
    if first {
        lines.push(super::render::tool_frame_line(
            LinkedLine::chrome(Line::default()),
            width,
        ));
    }

    let header = lines.len();
    lines.extend(
        super::render::tool_group_title_with_links(row, inner_width)
            .into_iter()
            .map(|line| super::render::tool_frame_line(line, width)),
    );
    let body = if expanded {
        lines.push(super::render::tool_border_line(width, ('├', '┤')));
        let start = lines.len();
        lines.extend(
            super::render::tool_body_with_links(row, inner_width)
                .into_iter()
                .map(|line| super::render::tool_frame_line(line, width)),
        );
        let end = lines.len();
        if !last {
            lines.push(super::render::tool_border_line(width, ('├', '┤')));
        }
        start..end
    } else {
        0..0
    };
    if last {
        lines.push(super::render::tool_border_line(width, ('╰', '╯')));
    }
    Section {
        lines,
        header,
        body,
    }
}
