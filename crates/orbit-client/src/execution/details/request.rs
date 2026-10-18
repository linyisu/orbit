//! Operation-specific request presentation shared by both tool inspectors.
use super::{ToolRun, pretty, request_directory};
use serde_json::Value;

#[cfg(test)]
mod tests;

/// The primary operation and its supporting scope. This is descriptive, never executable input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    heading: &'static str,
    content: String,
    language: &'static str,
    context: String,
}

impl Request {
    /// Selects the operation's readable content and scope without modifying the recorded call.
    /// Unknown or malformed requests keep their complete original representation.
    pub fn new(tool: &ToolRun, workspace: &str) -> Self {
        let mut request = Self {
            heading: "调用内容",
            content: pretty(&tool.arguments),
            language: "json",
            context: String::new(),
        };
        let Ok(args @ Value::Object(_)) = serde_json::from_str::<Value>(&tool.arguments) else {
            return request;
        };
        let (heading, key, language, fields): (_, _, _, &[(&str, &str)]) = match tool.name.as_str()
        {
            "Exec" => ("命令", "command", "bash", &[]),
            "ReadFile" => (
                "文件",
                "path",
                "plaintext",
                &[
                    ("start_line", "起始行"),
                    ("max_lines", "行数"),
                    ("start_char", "起始字符"),
                ],
            ),
            "ReadImage" => ("图片", "path", "plaintext", &[]),
            "ReadDocument" => (
                "文档",
                "path",
                "plaintext",
                &[
                    ("start_page", "起始页"),
                    ("end_page", "结束页"),
                    ("images", "页图"),
                    ("sheet", "工作表"),
                    ("start_row", "起始行"),
                    ("max_rows", "行数"),
                    ("start_char", "起始字符"),
                ],
            ),
            "WriteFile" => ("写入内容", "content", "plaintext", &[("path", "文件")]),
            "EditFile" => ("替换内容", "", "diff", &[("path", "文件")]),
            "ListFiles" => (
                "目录",
                "path",
                "plaintext",
                &[("glob", "匹配"), ("depth", "深度")],
            ),
            "Grep" => (
                "搜索文本",
                "query",
                "plaintext",
                &[
                    ("path", "范围"),
                    ("glob", "文件"),
                    ("case_sensitive", "区分大小写"),
                    ("regex", "正则"),
                    ("output_mode", "返回"),
                ],
            ),
            "WebSearch" => (
                "搜索网页",
                "query",
                "plaintext",
                &[
                    ("language", "语言"),
                    ("region", "地区"),
                    ("freshness", "时间范围"),
                ],
            ),
            "WebFetch" => ("网页", "url", "plaintext", &[("snapshot_id", "快照")]),
            "ProjectOverview" => ("项目", "path", "plaintext", &[("query", "查询")]),
            "CodeSymbols" => ("符号", "query", "plaintext", &[("path", "范围")]),
            "CommandOutput" => (
                "命令日志",
                "output_id",
                "plaintext",
                &[("stream", "输出流")],
            ),
            "CommandStatus" | "WaitCommands" | "StopCommands" => (
                "后台命令",
                "job_ids",
                "plaintext",
                &[("mode", "等待方式"), ("timeout_seconds", "最长等待（秒）")],
            ),
            "SpawnAgent" => (
                "子任务",
                "task",
                "plaintext",
                &[
                    ("role", "角色"),
                    ("model", "模型"),
                    ("workspace_strategy", "工作区"),
                ],
            ),
            "SendAgentMessage" => ("消息", "message", "plaintext", &[("agent_id", "Agent")]),
            "ReadAgent" => ("Agent", "agent_id", "plaintext", &[]),
            "WaitAgents" | "StopAgents" => {
                ("Agent", "agent_ids", "plaintext", &[("all", "等待全部")])
            }
            "AskUser" => ("问题", "questions", "json", &[]),
            "EnterPlan" | "SubmitPlan" | "CancelPlan" => {
                ("计划", "", "plaintext", &[("generation", "版本")])
            }
            "UpdatePlan" => ("计划", "markdown", "markdown", &[("title", "标题")]),
            "RequestAccess" => (
                "访问范围",
                "path",
                "plaintext",
                &[("reason", "原因"), ("access", "权限")],
            ),
            _ => return request,
        };
        request.heading = heading;
        request.language = language;
        if let Some(value) = args.get(key) {
            request.content = literal(value);
        } else if matches!(tool.name.as_str(), "ListFiles" | "ProjectOverview") {
            request.content = ".".into();
        } else if tool.name == "EnterPlan" {
            request.content = "进入规划模式".into();
        } else if tool.name == "SubmitPlan" {
            request.content = "提交计划供审阅".into();
        } else if tool.name == "CancelPlan" {
            request.content = "放弃自主计划草稿".into();
        }
        let mut context: Vec<String> = fields
            .iter()
            .filter_map(|(key, label)| {
                args.get(*key)
                    .filter(|value| !value.is_null())
                    .map(|value| format!("{label}：{}", literal(value)))
            })
            .collect();
        if tool.name == "Exec" {
            let directory = request_directory(tool, workspace);
            if !directory.is_empty() {
                context.push(format!("目录：{directory}"));
            }
        }
        if tool.name == "EditFile" {
            let edits = args["edits"]
                .as_array()
                .map(Vec::as_slice);
            let edits = edits.unwrap_or_else(|| std::slice::from_ref(&args));
            let changes: Option<Vec<String>> = edits
                .iter()
                .map(|edit| {
                    let before = edit["old_text"].as_str()?;
                    let after = edit["new_text"].as_str()?;
                    Some(
                        similar::TextDiff::from_lines(before, after)
                            .unified_diff()
                            .header("原文", "替换为")
                            .to_string(),
                    )
                })
                .collect();
            if let Some(changes) = changes {
                request.content = changes.join("\n");
                if edits
                    .iter()
                    .any(|edit| edit["replace_all"] == true)
                {
                    context.push("包含全部匹配项替换".into());
                }
                if edits.len() > 1 {
                    context.push(format!("{} 项有序替换", edits.len()));
                }
            }
        }
        request.context = context.join(" · ");
        request
    }

    pub fn heading(&self) -> &'static str {
        self.heading
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn language(&self) -> &'static str {
        self.language
    }

    pub fn context(&self) -> &str {
        &self.context
    }
}

fn literal(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| pretty(&value.to_string()), str::to_owned)
}
