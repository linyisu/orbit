//! Shared, read-only detail pages for native and terminal tool inspectors.
use super::{ToolRun, ToolStatus};
use serde_json::Value;

mod request;
pub mod retained;
pub use request::Request;

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
        "ReadImage" if tool.status == ToolStatus::Complete => output["path"]
            .as_str()
            .filter(|path| !path.is_empty())
            .map(|path| Target::Image(path.to_owned())),
        "SpawnAgent" | "SendAgentMessage" => {
            let id = output["agent_id"].as_str()?;
            (id.len() == 32
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit()))
            .then(|| Target::Agent(id.to_owned()))
        }
        "AskUser" if tool.status == ToolStatus::Complete => Some(Target::Question),
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
        &[Stream::Changes, Stream::Raw]
    } else if tool.command_result().is_some()
        || (tool.is_foreground_command()
            && (tool.status == ToolStatus::Running || tool.stdout_preview.is_some()))
    {
        if tool
            .command_result()
            .is_some_and(|result| result.stderr.is_empty())
        {
            &[Stream::Stdout, Stream::Raw]
        } else {
            &[Stream::Stdout, Stream::Stderr, Stream::Raw]
        }
    } else if tool
        .output
        .as_deref()
        .is_some_and(|output| {
            serde_json::from_str::<Value>(output).is_ok()
                && structured(&tool.name, output) != pretty(output)
        })
    {
        &[Stream::Result, Stream::Raw]
    } else {
        &[Stream::Result]
    }
}

pub fn default_stream(tool: &ToolRun) -> Stream {
    let pages = streams(tool);
    if pages.contains(&Stream::Stderr)
        && tool
            .command_result()
            .is_some_and(|result| result.stdout.is_empty() && !result.stderr.is_empty())
    {
        Stream::Stderr
    } else {
        pages[0]
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
        && tool.name == "CommandOutput"
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

/// Complete request fields for the fixed inspector header. Multiline strings stay literal;
/// nested values keep their JSON structure. Display metadata never becomes executable input.
pub fn parameters(tool: &ToolRun, workspace: &str) -> String {
    let Ok(Value::Object(mut fields)) = serde_json::from_str::<Value>(&tool.arguments) else {
        return tool.arguments.clone();
    };
    if tool.name == "Exec" && !workspace.is_empty() {
        fields.insert(
            "cwd".into(),
            Value::String(request_directory(tool, workspace)),
        );
    }
    let mut lines = Vec::new();
    if let Some(purpose) = tool
        .purpose
        .as_deref()
        .filter(|purpose| !purpose.is_empty())
    {
        lines.push(format!("目的: {purpose}"));
    }
    for key in ["command", "path", "url"] {
        if let Some(value) = fields.remove(key) {
            lines.push(parameter(key, &value));
        }
    }
    lines.extend(
        fields
            .iter()
            .map(|(key, value)| parameter(key, value)),
    );
    if lines.is_empty() {
        "无参数".into()
    } else {
        lines.join("\n")
    }
}

fn parameter(key: &str, value: &Value) -> String {
    let value = value
        .as_str()
        .map_or_else(|| pretty(&value.to_string()), str::to_owned);
    format!("{key}: {value}")
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
                .map_or_else(String::new, |code| format!("退出码 {code}"))
        };
        return [label, format!("{} ms", result.elapsed.as_millis())]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");
    }
    if tool.name == "Exec" && tool.status == ToolStatus::Background && tool.run_finished {
        return "结果未确认".into();
    }
    if tool.status == ToolStatus::Complete {
        String::new()
    } else {
        tool.elapsed_ms.map_or_else(
            || tool.status.label().into(),
            |elapsed| format!("{} · {elapsed} ms", tool.status.label()),
        )
    }
}

pub fn note(tool: &ToolRun, stream: Stream) -> String {
    if tool.name == "CommandOutput"
        && let Some(output) = &tool.output
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(output)
    {
        return format!(
            "{} · 字节 {}–{} / {}{}{}",
            value["stream"]
                .as_str()
                .unwrap_or("输出"),
            value["offset"],
            value["next_offset"],
            value["total_retained_bytes"],
            if value["has_more"] == true || value["truncated"] == true {
                " · 可续读"
            } else {
                ""
            },
            if value["capture_complete"] == false {
                " · 原始采集不完整"
            } else {
                ""
            },
        );
    }
    if tool.name != "Exec" {
        if tool.output.is_none() && tool.status == ToolStatus::Running {
            return "工具调用正在进行，尚未收到结果。".into();
        }
        return tool
            .output
            .as_deref()
            .and_then(|output| serde_json::from_str::<Value>(output).ok())
            .map_or_else(String::new, |value| pagination(&value));
    }
    if let Some(result) = tool.command_result() {
        let mut notes = Vec::new();
        if result.output_truncated {
            notes.push("输出为每路最多 32KiB 的尾部".to_owned());
        }
        if result.retained_output_truncated {
            notes.push("原始日志保留不完整".to_owned());
        }
        if let Some(error) = result.retained_output_error {
            notes.push(error);
        }
        return notes.join(" · ");
    }
    if tool.status == ToolStatus::Background || !tool.is_foreground_command() {
        if tool.run_finished {
            return "当前任务已结束，未收到后台命令的完成结果；此处保留启动回执。".into();
        }
        return "后台启动回执；等待完成事件，此页不提供后台实时日志。".into();
    }
    if tool.status != ToolStatus::Running {
        return String::new();
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
        "ReadFile" | "ReadDocument" | "CommandOutput" | "WebFetch" => {
            if let Some(content) = value["content"].as_str() {
                return content.to_owned();
            }
        }
        "Grep" => {
            if let Some(matches) = value["matches"].as_array() {
                let mut text = String::new();
                for found in matches {
                    let path = found["path"]
                        .as_str()
                        .unwrap_or_default();
                    match value["output_mode"].as_str() {
                        Some("files") => text.push_str(&format!("{path}\n")),
                        Some("count") => text.push_str(&format!("{path}: {}\n", found["count"])),
                        _ => {
                            text.push_str(&format!(
                                "{path}:{}:{}\n",
                                found["line"], found["column"]
                            ));
                            let mut lines = std::collections::BTreeMap::new();
                            if let Some(context) = found["context"].as_array() {
                                for line in context {
                                    if let (Some(number), Some(content)) =
                                        (line["line"].as_u64(), line["text"].as_str())
                                    {
                                        lines.insert(number, content);
                                    }
                                }
                            }
                            if let Some(number) = found["line"].as_u64() {
                                lines.insert(
                                    number,
                                    found["text"]
                                        .as_str()
                                        .unwrap_or_default(),
                                );
                            }
                            for (number, content) in lines {
                                let marker = if found["line"] == number { '>' } else { ' ' };
                                text.push_str(&format!("{marker} {number}: {content}\n"));
                            }
                            text.push('\n');
                        }
                    }
                }
                if matches.is_empty() {
                    text.push_str("没有匹配项");
                }
                return text;
            }
        }
        "ListFiles" => {
            if let Some(entries) = value["entries"].as_array() {
                let text = entries
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
                return text;
            }
        }
        _ => {}
    }
    let mut text = String::new();
    describe(&value, 0, &mut text);
    text.trim_end().to_owned()
}

fn pagination(value: &Value) -> String {
    let mut parts = Vec::new();
    if value["truncated"] == true || value["has_more"] == true {
        let next = ["next_start_char", "next_offset", "next_start_line"]
            .iter()
            .find_map(|key| {
                value[key]
                    .as_u64()
                    .map(|offset| format!("{key}={offset}"))
            });
        parts.push(next.map_or_else(|| "结果有后页".into(), |next| format!("可续读 · {next}")));
    }
    if value["scan_truncated"] == true {
        parts.push("扫描达到上限".into());
    }
    parts.join(" · ")
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
    fn search_modes_and_context_lines_survive_shared_projection() {
        let rows = json!({
            "matches": [{
                "path": "sample.rs", "line": 2, "column": 1, "text": "hit",
                "context": [{ "line": 1, "text": "before" }, { "line": 3, "text": "after" }]
            }]
        });
        let content = structured("Grep", &rows.to_string());
        assert!(content.find("1: before").unwrap() < content.find("2: hit").unwrap());
        assert!(content.find("2: hit").unwrap() < content.find("3: after").unwrap());
        for (mode, expected) in [("files", "sample.rs\n"), ("count", "sample.rs: 3\n")] {
            let result =
                json!({"output_mode": mode, "matches": [{"path": "sample.rs", "count": 3}]});
            assert_eq!(structured("Grep", &result.to_string()), expected);
        }
    }

    #[test]
    fn diff_uses_confirmed_changes_instead_of_requested_text() {
        let call = ToolCall::function("change-1", "EditFile", json!({"path": "generated.rs"}));
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
    fn command_default_page_selects_stderr_only_when_stdout_is_empty() {
        let mut tool = ToolRun::new(&ToolCall::function("fixture", "Exec", json!({})));
        for (stdout, stderr, expected, pages) in [
            ("", "", Stream::Stdout, &[Stream::Stdout, Stream::Raw][..]),
            (
                "out",
                "",
                Stream::Stdout,
                &[Stream::Stdout, Stream::Raw][..],
            ),
            (
                "",
                "err",
                Stream::Stderr,
                &[Stream::Stdout, Stream::Stderr, Stream::Raw][..],
            ),
            (
                "out",
                "err",
                Stream::Stdout,
                &[Stream::Stdout, Stream::Stderr, Stream::Raw][..],
            ),
        ] {
            tool.set_output(
                ToolStatus::Complete,
                json!({
                    "exit_code": 0,
                    "elapsed": {"secs": 0, "nanos": 0},
                    "stdout": stdout,
                    "stderr": stderr,
                    "stdout_total_bytes": stdout.len(),
                    "stderr_total_bytes": stderr.len(),
                    "timed_out": false,
                    "cancelled": false,
                    "output_truncated": false
                })
                .to_string(),
            );
            assert_eq!(streams(&tool), pages);
            assert_eq!(
                default_stream(&tool),
                expected,
                "stdout={stdout:?}, stderr={stderr:?}"
            );
        }
    }

    #[test]
    fn navigation_requires_confirmed_tool_output_and_valid_session_ids() {
        let mut tool = ToolRun::new(&ToolCall::function(
            "generated",
            "ReadImage",
            json!({"path": "sample.png"}),
        ));
        assert!(target(&tool).is_none());
        tool.set_output(
            ToolStatus::Complete,
            json!({"path": "sample.png"}).to_string(),
        );
        assert_eq!(target(&tool), Some(Target::Image("sample.png".into())));
        tool.name = "SpawnAgent".into();
        tool.output = Some(json!({"agent_id": "../unrelated"}).to_string());
        assert!(target(&tool).is_none());
        tool.output = Some(json!({"agent_id": "a".repeat(32)}).to_string());
        assert_eq!(target(&tool), Some(Target::Agent("a".repeat(32))));
    }
}
