//! Shared, read-only detail pages for native and terminal tool inspectors.
use super::{ToolRun, ToolStatus};
use serde_json::Value;

#[cfg(test)]
mod streams_tests;

/// An explicit navigation target reported by a completed tool, never an executable action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Image(String),
    Agent(String),
    Question,
}

impl Target {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Image(_) => "查看图片",
            Self::Agent(_) => "查看子任务",
            Self::Question => "查看问答",
        }
    }
}

/// Navigation uses confirmed output, so unexecuted arguments cannot create a destination.
pub fn target(tool: &ToolRun) -> Option<Target> {
    let output: Value = serde_json::from_str(tool.output.as_deref()?).ok()?;
    match tool.name.as_str() {
        "read_image" if tool.status == ToolStatus::Complete => output["path"]
            .as_str()
            .filter(|path| !path.is_empty())
            .map(|path| Target::Image(path.to_owned())),
        "spawn_agent" | "send_agent_message" => {
            let id = output["agent_id"].as_str()?;
            (id.len() == 32
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit()))
            .then(|| Target::Agent(id.to_owned()))
        }
        "ask_user" if tool.status == ToolStatus::Complete => Some(Target::Question),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
    Command,
    Result,
    Arguments,
    Raw,
    Changes,
}

impl Stream {
    pub fn label(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Command => "命令",
            Self::Result => "结果",
            Self::Arguments => "参数",
            Self::Raw => "原始结果",
            Self::Changes => "变更",
        }
    }
}

/// Available pages are derived once for both frontends, including dynamic tools.
pub fn streams(tool: &ToolRun) -> &'static [Stream] {
    if !tool.changes.is_empty() {
        &[
            Stream::Changes,
            Stream::Result,
            Stream::Arguments,
            Stream::Raw,
        ]
    } else if tool.name == "run_command" {
        &[
            Stream::Stdout,
            Stream::Stderr,
            Stream::Command,
            Stream::Result,
            Stream::Arguments,
            Stream::Raw,
        ]
    } else {
        &[Stream::Result, Stream::Arguments, Stream::Raw]
    }
}

pub fn default_stream(tool: &ToolRun) -> Stream {
    if !tool.changes.is_empty() {
        return Stream::Changes;
    }
    if tool.name == "run_command" && tool.is_foreground_command() {
        if tool
            .command_result()
            .is_some_and(|result| result.stdout.is_empty() && !result.stderr.is_empty())
        {
            Stream::Stderr
        } else if tool.status != ToolStatus::Failed || tool.command_result().is_some() {
            Stream::Stdout
        } else {
            Stream::Raw
        }
    } else {
        Stream::Result
    }
}

pub fn content(tool: &ToolRun, stream: Stream) -> String {
    if stream == Stream::Changes {
        return tool
            .changes
            .iter()
            .map(|change| {
                similar::TextDiff::from_lines(
                    change
                        .before
                        .as_deref()
                        .unwrap_or_default(),
                    &change.after,
                )
                .unified_diff()
                .header(&change.path, &change.path)
                .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    if stream == Stream::Command {
        return serde_json::from_str::<serde_json::Value>(&tool.arguments)
            .ok()
            .and_then(|args| {
                args["command"]
                    .as_str()
                    .map(str::to_owned)
            })
            .unwrap_or_default();
    }
    if stream == Stream::Arguments {
        return pretty(&tool.arguments);
    }
    if let Some(result) = tool.command_result() {
        match stream {
            Stream::Stdout => return result.stdout,
            Stream::Stderr => return result.stderr,
            _ => {}
        }
    }
    if stream == Stream::Stdout {
        return tool
            .stdout_preview
            .clone()
            .unwrap_or_default();
    }
    if stream == Stream::Stderr {
        return String::new();
    }
    let Some(output) = &tool.output else {
        return String::new();
    };
    if stream != Stream::Raw
        && tool.name == "read_command_output"
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(output)
        && let Some(content) = value["content"].as_str()
    {
        return content.into();
    }
    if stream == Stream::Raw {
        pretty(output)
    } else {
        structured(&tool.name, output)
    }
}

fn pretty(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| text.into())
}

pub fn request_directory(tool: &ToolRun, workspace: &str) -> String {
    let args = serde_json::from_str::<serde_json::Value>(&tool.arguments).ok();
    args.as_ref()
        .and_then(|args| args["cwd"].as_str())
        .map_or_else(
            || workspace.into(),
            |cwd| {
                if cwd.starts_with('/') || cwd.as_bytes().get(1) == Some(&b':') {
                    cwd.to_owned()
                } else if cwd == "." {
                    workspace.to_owned()
                } else {
                    format!("{}/{cwd}", workspace.trim_end_matches('/'))
                }
            },
        )
}

pub fn status(tool: &ToolRun) -> String {
    if let Some(result) = tool.command_result() {
        let label = if result.cancelled {
            "已取消".into()
        } else if result.timed_out {
            "已超时".into()
        } else {
            result
                .exit_code
                .map_or_else(|| "进程已结束".into(), |code| format!("退出码 {code}"))
        };
        return format!("{label} · {} ms", result.elapsed.as_millis());
    }
    if tool.name == "run_command" && tool.status == ToolStatus::Background && tool.run_finished {
        return "结果未确认".into();
    }
    tool.elapsed_ms.map_or_else(
        || tool.status.label().into(),
        |elapsed| format!("{} · {elapsed} ms", tool.status.label()),
    )
}

pub fn note(tool: &ToolRun, stream: Stream) -> String {
    if stream == Stream::Changes {
        return "工具已提交的文件变更；此页只展示执行收据。".into();
    }
    if stream == Stream::Command {
        return "完整请求命令，可选择和复制。".into();
    }
    if stream == Stream::Arguments {
        return "工具调用的完整参数，可选择和复制。".into();
    }
    if stream == Stream::Raw {
        return "工具返回的原始数据，可选择和复制。".into();
    }
    if tool.name == "read_command_output"
        && let Some(output) = &tool.output
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(output)
    {
        return format!(
            "{} 单页快照 · 字节 {}–{} / 已保留 {}{}",
            value["stream"]
                .as_str()
                .unwrap_or("输出"),
            value["offset"],
            value["next_offset"],
            value["total_retained_bytes"],
            if value["truncated"] == true {
                " · 仍有后页"
            } else {
                ""
            },
        );
    }
    if tool.name != "run_command" {
        if tool.output.is_none() && tool.status == ToolStatus::Running {
            return "工具调用正在进行，尚未收到结果。".into();
        }
        return "已接收的工具结果，可选择和复制。".into();
    }
    if let Some(result) = tool.command_result() {
        let mut note = format!(
            "工具返回的有界结果 · stdout {} 字节 · stderr {} 字节",
            result.stdout_total_bytes, result.stderr_total_bytes,
        );
        if result.output_truncated {
            note.push_str(" · 仅显示每路最多 32KiB 的尾部");
        }
        if result.retained_output_truncated {
            note.push_str(" · 原始日志保留不完整");
        }
        if let Some(error) = result.retained_output_error {
            note.push_str(&format!(" · {error}"));
        }
        return note;
    }
    if tool.status == ToolStatus::Background || !tool.is_foreground_command() {
        if tool.run_finished {
            return "当前任务已结束，未收到后台命令的完成结果；此处保留启动回执。".into();
        }
        return "后台启动回执；等待完成事件，此页不提供后台实时日志。".into();
    }
    if tool.status != ToolStatus::Running {
        return "已接收的结果；未识别为命令结果结构，可在“结果”中查看原文。".into();
    }
    if stream == Stream::Stderr {
        return "stderr 将在命令结束后显示工具返回的结果。".into();
    }
    if tool.live {
        "实时 stdout 尾部 · 最多 16KiB · 由服务端提供".into()
    } else {
        "等待 stdout 预览；审批状态和停止任务操作位于主界面。".into()
    }
}

/// Projects known result contracts; unknown tools retain a generic structured view.
fn structured(name: &str, output: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(output) else {
        return output.to_owned();
    };
    match name {
        "read_file" | "read_document" | "read_command_output" | "web_fetch" => {
            if let Some(content) = value["content"].as_str() {
                let mut text = String::new();
                for key in ["path", "url", "title"] {
                    if let Some(label) = value[key].as_str() {
                        text.push_str(label);
                        text.push('\n');
                    }
                }
                text.push('\n');
                text.push_str(content);
                append_pagination(&value, &mut text);
                return text;
            }
        }
        "search_text" => {
            if let Some(matches) = value["matches"].as_array() {
                let mut text = String::new();
                for found in matches {
                    text.push_str(&format!(
                        "{}:{}:{}\n{}\n\n",
                        found["path"]
                            .as_str()
                            .unwrap_or_default(),
                        found["line"],
                        found["column"],
                        found["text"]
                            .as_str()
                            .unwrap_or_default(),
                    ));
                }
                if matches.is_empty() {
                    text.push_str("没有匹配项");
                }
                append_pagination(&value, &mut text);
                return text;
            }
        }
        "list_files" => {
            if let Some(entries) = value["entries"].as_array() {
                let mut text = entries
                    .iter()
                    .map(|entry| {
                        format!(
                            "{}  {}",
                            entry["kind"]
                                .as_str()
                                .unwrap_or_default(),
                            entry["path"]
                                .as_str()
                                .unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                append_pagination(&value, &mut text);
                return text;
            }
        }
        _ => {}
    }
    let mut text = String::new();
    describe(&value, 0, &mut text);
    text.trim_end().to_owned()
}

fn append_pagination(value: &Value, text: &mut String) {
    if value["truncated"] == true || value["has_more"] == true {
        text.push_str("\n\n仅显示本次返回的内容，仍有后续内容。");
    }
    if value["scan_truncated"] == true {
        text.push_str("\n扫描达到上限，结果可能不完整。");
    }
}

/// Expands text blocks without JSON escapes while keeping arbitrary structured fields visible.
fn describe(value: &Value, depth: usize, text: &mut String) {
    const MAX_DETAIL_DEPTH: usize = 24;
    if depth >= MAX_DETAIL_DEPTH {
        text.push_str("…（深层结构请查看原始结果）\n");
        return;
    }
    match value {
        Value::String(value) => {
            text.push_str(value);
            text.push('\n');
        }
        Value::Object(fields) => {
            for (key, value) in fields {
                text.push_str(&"  ".repeat(depth));
                text.push_str(key);
                text.push_str(": ");
                if matches!(key.as_str(), "data" | "blob")
                    && (fields.contains_key("mimeType")
                        || fields.contains_key("mime_type")
                        || fields
                            .get("type")
                            .and_then(Value::as_str)
                            .is_some_and(|kind| matches!(kind, "image" | "audio")))
                {
                    text.push_str("[二进制附件，原始结果中保留完整数据]\n");
                    continue;
                }
                if value.is_object() || value.is_array() {
                    text.push('\n');
                }
                describe(value, depth + 1, text);
            }
        }
        Value::Array(values) => {
            for value in values {
                describe(value, depth, text);
                text.push('\n');
            }
        }
        _ => {
            text.push_str(&value.to_string());
            text.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::model::ToolCall;
    use serde_json::json;

    #[test]
    fn diff_uses_confirmed_changes_instead_of_requested_text() {
        let call = ToolCall::function("change-1", "edit_file", json!({"path": "generated.rs"}));
        let mut tool = ToolRun::new(&call);
        tool.changes
            .push(super::super::FileChange {
                path: "generated.rs".into(),
                before: Some("old\n".into()),
                after: "new\n".into(),
            });
        assert_eq!(default_stream(&tool), Stream::Changes);
        let diff = content(&tool, Stream::Changes);
        assert!(diff.contains("-old\n+new\n"));
        assert_eq!(streams(&tool)[0], Stream::Changes);
    }

    #[test]
    fn generic_mcp_blocks_keep_text_readable_and_binary_payload_in_raw_page() {
        let call = ToolCall::function("dynamic-1", "mcp_generated_inspect", json!({}));
        let mut tool = ToolRun::new(&call);
        tool.output = Some(
            json!({"content": [
                {"type": "text", "text": "first line\nsecond line"},
                {"type": "image", "mimeType": "image/png", "data": "generated-payload"}
            ]})
            .to_string(),
        );
        let text = content(&tool, Stream::Result);
        assert!(text.contains("first line\nsecond line"));
        assert!(!text.contains("generated-payload"));
        assert!(content(&tool, Stream::Raw).contains("generated-payload"));
    }

    #[test]
    fn default_page_always_exists_for_running_and_finished_tools() {
        for name in ["run_command", "read_file", "mcp_generated_inspect"] {
            for background in [false, true] {
                for status in [
                    ToolStatus::Running,
                    ToolStatus::Background,
                    ToolStatus::Complete,
                    ToolStatus::Failed,
                ] {
                    let mut tool = ToolRun::new(&ToolCall::function(
                        "fixture",
                        name,
                        json!({"background": background}),
                    ));
                    tool.status = status;
                    assert!(streams(&tool).contains(&default_stream(&tool)));
                }
            }
        }
    }

    #[test]
    fn navigation_requires_confirmed_tool_output_and_valid_session_ids() {
        let mut tool = ToolRun::new(&ToolCall::function(
            "generated",
            "read_image",
            json!({"path": "sample.png"}),
        ));
        assert!(target(&tool).is_none());
        tool.set_output(
            ToolStatus::Complete,
            json!({"path": "sample.png"}).to_string(),
        );
        assert_eq!(target(&tool), Some(Target::Image("sample.png".into())));
        tool.name = "spawn_agent".into();
        tool.output = Some(json!({"agent_id": "../unrelated"}).to_string());
        assert!(target(&tool).is_none());
        tool.output = Some(json!({"agent_id": "a".repeat(32)}).to_string());
        assert_eq!(target(&tool), Some(Target::Agent("a".repeat(32))));
    }
}
