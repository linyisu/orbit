//! Failure presentation uses service receipts, never diagnostic text as execution state.
use super::{ToolRun, ToolStatus};
use serde_json::Value;

/// A failed or cancelled call's explanation, separate from command output and request content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    title: &'static str,
    message: String,
    guidance: &'static str,
}

impl Failure {
    pub fn title(&self) -> &'static str {
        self.title
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn guidance(&self) -> &'static str {
        self.guidance
    }
}

/// Projects typed failure envelopes and command receipts; legacy text stays unclassified.
/// This is display data only and does not authorize retries or alter the service outcome.
pub fn failure(tool: &ToolRun) -> Option<Failure> {
    if let Some(result) = tool.command_result() {
        let (title, message) = if result.cancelled {
            ("命令已取消", String::new())
        } else if result.timed_out {
            (
                "命令执行超时",
                format!("运行时间：{} ms", result.elapsed.as_millis()),
            )
        } else if let Some(signal) = result.signal {
            (
                "命令被信号终止",
                format!(
                    "信号 {signal}{}",
                    if result.core_dumped {
                        " · 已产生核心转储"
                    } else {
                        ""
                    }
                ),
            )
        } else if let Some(code) = result
            .exit_code
            .filter(|code| *code != 0)
        {
            ("命令执行失败", format!("退出码 {code}"))
        } else if result.exit_code.is_none() {
            ("命令结果未确认", String::new())
        } else {
            return None;
        };
        return Some(Failure {
            title,
            message,
            guidance: "",
        });
    }
    if !matches!(tool.status, ToolStatus::Failed | ToolStatus::Stopped) {
        return None;
    }
    let output = tool
        .output
        .as_deref()
        .unwrap_or_default();
    let value = serde_json::from_str::<Value>(output).ok();
    let error = value
        .as_ref()
        .map(|value| &value["error"]);
    let code = error.and_then(|error| error["code"].as_str());
    let (title, guidance) = match code {
        Some("invalid_arguments") => ("调用参数无效", "需要修正调用参数。"),
        Some("unknown_tool") => ("工具不可用", "需要选择当前可用的工具。"),
        Some("permission_denied") => ("访问被拒绝", ""),
        Some("authorization_required") => ("需要访问授权", "等待用户授权后再继续。"),
        Some("policy_denied") => ("当前策略不允许此操作", ""),
        Some("conflict") => ("输入已发生变化", "重新读取当前内容后再处理。"),
        Some("dependency_unavailable") => ("所需依赖不可用", "需要检查依赖或连接配置。"),
        Some("cancelled") => ("工具调用已取消", ""),
        _ if tool.status == ToolStatus::Stopped => ("工具调用已停止", ""),
        _ => ("工具执行失败", ""),
    };
    let message = error
        .and_then(|error| error["message"].as_str())
        .or_else(|| value.as_ref().and_then(Value::as_str))
        .unwrap_or(output)
        .to_owned();
    Some(Failure {
        title,
        message,
        guidance,
    })
}
