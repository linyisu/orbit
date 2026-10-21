//! Controller presentation of persisted events; never decides execution or authorization.
use orbit_protocol::{Page, Update};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    User,
    Text,
    Thought,
    Tool,
    Notice,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub kind: Kind,
    pub title: String,
    pub content: String,
    pub arguments: String,
    pub state: String,
}

impl Item {
    /// Presents structured payloads without displaying JSON string escapes as prose.
    pub fn details(&self) -> String {
        fn payload(source: &str) -> String {
            match serde_json::from_str::<Value>(source) {
                Ok(Value::String(text)) => text,
                Ok(Value::Object(fields)) => fields
                    .into_iter()
                    .map(|(key, value)| match value {
                        Value::String(text) => format!("### {key}\n\n```\n{text}\n```"),
                        value => format!(
                            "### {key}\n\n```json\n{}\n```",
                            serde_json::to_string_pretty(&value).unwrap_or_default()
                        ),
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                Ok(value) => format!(
                    "```json\n{}\n```",
                    serde_json::to_string_pretty(&value).unwrap_or_default()
                ),
                Err(_) => source.to_owned(),
            }
        }
        format!(
            "{}\n\n## 参数\n\n{}\n\n## 输出\n\n{}",
            self.state,
            payload(&self.arguments),
            payload(&self.content)
        )
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub struct Conversation {
    pub items: Vec<Item>,
    pub busy: bool,
    pub model: String,
    next_id: u64,
}

impl Conversation {
    fn append(&mut self, kind: Kind, title: &str, content: String) {
        let id = format!("message-{}", self.next_id);
        self.next_id += 1;
        self.items.push(Item {
            id,
            kind,
            title: title.into(),
            content,
            arguments: String::new(),
            state: String::new(),
        });
    }

    fn delta(&mut self, kind: Kind, value: &Value) {
        let Some(delta) = value.as_str() else { return };
        if let Some(last) = self
            .items
            .last_mut()
            .filter(|item| item.kind == kind)
        {
            last.content.push_str(delta);
        } else {
            self.append(kind, "", delta.into());
        }
    }

    pub fn apply(&mut self, page: &Page<Value, Value>) {
        self.busy = page.busy;
        self.model.clone_from(&page.model);
        for update in &page.updates {
            match update {
                Update::Started { prompt, .. } => self.append(Kind::User, "你", prompt.clone()),
                Update::Report(report) => {
                    self.append(Kind::Notice, &report.title, report.text.clone())
                }
                Update::Agent(event) => self.agent(event),
                Update::Finished { .. } => {}
                Update::Profile(_) => {}
            }
        }
        for progress in &page.tool_progress {
            if let Some(item) = self
                .items
                .iter_mut()
                .find(|item| item.id == format!("tool-{}", progress.call_id))
            {
                let output = [progress.stdout.as_deref(), progress.stderr.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("\n");
                if !output.is_empty() {
                    item.content = output;
                }
            }
        }
    }

    fn agent(&mut self, event: &Value) {
        if let Some(model) = event.get("Model") {
            if let Some(delta) = model.get("TextDelta") {
                self.delta(Kind::Text, delta);
            } else if let Some(delta) = model.get("ReasoningDelta") {
                self.delta(Kind::Thought, delta);
            }
        }
        for (key, state) in [
            ("ToolStarted", "执行中"),
            ("ToolBackgrounded", "后台运行"),
            ("ToolFinished", "已返回"),
            ("ToolFailed", "失败"),
        ] {
            let Some(value) = event.get(key) else {
                continue;
            };
            let Some(id) = value
                .pointer("/call/id")
                .and_then(Value::as_str)
            else {
                continue;
            };
            let title = value
                .pointer("/call/function/name")
                .and_then(Value::as_str)
                .unwrap_or("工具");
            let arguments = value
                .pointer("/call/function/arguments")
                .and_then(Value::as_str)
                .unwrap_or("");
            let content = value
                .get("content")
                .or_else(|| value.get("error"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(item) = self
                .items
                .iter_mut()
                .find(|item| item.id == format!("tool-{id}"))
            {
                item.content = content.into();
                item.state = state.into();
            } else {
                self.items.push(Item {
                    id: format!("tool-{id}"),
                    kind: Kind::Tool,
                    title: title.into(),
                    content: content.into(),
                    arguments: arguments.into(),
                    state: state.into(),
                });
            }
        }
        if let Some(error) = event
            .pointer("/Failed/error")
            .and_then(Value::as_str)
        {
            self.append(Kind::Notice, "任务失败", error.into());
        }
        if let Some(content) = event
            .pointer("/InputReceived/content")
            .and_then(Value::as_str)
        {
            self.append(Kind::User, "你", content.into());
        }
    }
}

/// Readable fields preserve the exact approval data without interpreting tool policy.
pub fn approval(preview: &Value) -> (String, String) {
    let Some((kind, data)) = preview
        .as_object()
        .and_then(|object| object.iter().next())
    else {
        return ("操作审批".into(), preview.to_string());
    };
    let title = match kind.as_str() {
        "Write" => "写入文件",
        "Delete" => "删除文件",
        "Command" => "执行命令",
        "Mcp" => "调用 MCP 工具",
        "Capability" => "请求访问权限",
        _ => "操作审批",
    };
    let content = data
        .as_object()
        .map(|fields| {
            fields
                .iter()
                .map(|(key, value)| {
                    let label = match key.as_str() {
                        "command" => "命令",
                        "cwd" => "执行目录",
                        "path" => "文件",
                        "before" => "原内容",
                        "after" => "写入内容",
                        "timeout_seconds" => "超时（秒）",
                        "network_allowed" => "允许网络访问",
                        "workspace_access" => "工作区访问",
                        "external_capabilities" => "额外权限",
                        "external_write" => "外部写入",
                        _ => key,
                    };
                    let value = value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| serde_json::to_string_pretty(value).unwrap_or_default());
                    let fence = "`".repeat(
                        value
                            .split(|c| c != '`')
                            .map(str::len)
                            .max()
                            .unwrap_or(0)
                            .max(2)
                            + 1,
                    );
                    format!("**{label}**\n\n{fence}\n{value}\n{fence}")
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_else(|| data.to_string());
    (title.into(), content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_details_decode_multiline_strings_without_changing_raw_output() {
        let raw = json!({"content": "first line\nsecond line"}).to_string();
        let item = Item {
            id: "generated-tool".into(),
            kind: Kind::Tool,
            title: "Inspect".into(),
            content: raw.clone(),
            arguments: "{}".into(),
            state: "已返回".into(),
        };
        assert!(
            item.details()
                .contains("first line\nsecond line")
        );
        assert_eq!(item.content, raw);
    }

    #[test]
    fn streaming_and_tool_completion_keep_chronological_order() {
        let mut view = Conversation::default();
        view.agent(&json!({"Model": {"TextDelta": "First"}}));
        view.agent(&json!({"Model": {"TextDelta": " sentence."}}));
        let call =
            json!({"id": "generated-call", "function": {"name": "Inspect", "arguments": "{}"}});
        view.agent(&json!({"ToolStarted": {"call": call}}));
        view.agent(&json!({"Model": {"TextDelta": "Following paragraph."}}));
        view.agent(&json!({"ToolFinished": {"call": call, "content": "result"}}));
        assert_eq!(view.items.len(), 3);
        assert_eq!(view.items[0].content, "First sentence.");
        assert_eq!(view.items[1].content, "result");
        assert_eq!(view.items[2].content, "Following paragraph.");
    }
}
