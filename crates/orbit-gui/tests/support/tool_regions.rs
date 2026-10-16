//! Independent lifecycle samples for the reasoning, tool list, and answer regions.
use serde_json::{Value, json};
use std::collections::VecDeque;

pub(super) fn events() -> VecDeque<Value> {
    let read = json!({
        "id": "regions-read",
        "purpose": "查看示例模块入口",
        "function": {
            "name": "read_file", "arguments": json!({"path": "sample.rs"}).to_string()
        }
    });
    let command = json!({
        "id": "regions-command",
        "purpose": "验证生成的目录清单",
        "function": {
            "name": "run_command", "arguments": json!({"command": "printf catalog"}).to_string()
        }
    });
    [
        json!({"Agent": {"Model": {
            "ReasoningDelta": "先确认示例模块的入口，再检查目录清单。\
                此段为独立生成的界面验收文字。"
        }}}),
        json!({"Agent": {"ReasoningFinished": {"elapsed_ms": 800}}}),
        json!({"Agent": {"Model": {"TextDelta": "正在检查示例模块。"}}}),
        json!({"Agent": {"ToolStarted": {"call": read}}}),
        json!({"Agent": {"ToolFinished": {
            "call": read,
            "content": json!({
                "path": "sample.rs", "content": "pub fn catalog() -> usize { 7 }\n"
            }).to_string(),
            "changes": []
        }}}),
        json!({"Agent": {"Model": {"TextDelta": "入口已确认，继续验证目录清单。"}}}),
        json!({"Agent": {"ToolStarted": {"call": command}}}),
        json!({"Agent": {"ToolFinished": {
            "call": command,
            "content": json!({
                "exit_code": 0, "elapsed": {"secs": 0, "nanos": 40_000_000},
                "stdout": "catalog: 7 entries\nverification complete\n", "stderr": "",
                "timed_out": false, "cancelled": false, "output_truncated": false,
                "stdout_total_bytes": 41, "stderr_total_bytes": 0,
                "retained_output_truncated": false, "sandbox": "native"
            }).to_string(),
            "changes": []
        }}}),
        json!({"Agent": {"Model": {
            "TextDelta": "检查完成。示例目录包含 **7 个条目**，入口与验证结果一致。\n\n\
                可以继续检查下一组模块。"
        }}}),
    ]
    .into()
}

pub(super) fn held(workspace: &str, event: &Value) -> bool {
    let stage = if event["Agent"]
        .get("ReasoningFinished")
        .is_some()
    {
        "reasoning"
    } else if event["Agent"]["ToolFinished"]["call"]["id"] == "regions-command" {
        "tool"
    } else {
        return false;
    };
    std::path::Path::new(workspace)
        .join(format!(".fixture-regions-{stage}"))
        .exists()
}
