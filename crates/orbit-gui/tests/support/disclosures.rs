//! Long completed tool details, including expansion during a running group.
use serde_json::{Value, json};
use std::collections::VecDeque;

pub(super) fn events(workspace: &str, live: bool) -> VecDeque<Value> {
    let mut events = VecDeque::new();
    for index in 1..=5 {
        if index > 1 {
            events.push_back(json!({"Agent":{"Model":{"ReasoningDelta":format!(
                "检查第 {index} 个命令前的状态。"
            )}}}));
        }
        let file = matches!(index, 1 | 5);
        let arguments = if file {
            json!({"path":format!("src/disclosure-{index:02}.txt")})
        } else {
            json!({
                "command":format!("printf 'disclosure-{index:02}\\n'"),
                "cwd":workspace,
                "timeout_seconds":30,
                "background":false,
            })
        };
        let call = json!({
            "id":format!("disclosure-{index:02}"),
            "function":{
                "name":if file { "read_file" } else { "run_command" },
                "arguments":serde_json::to_string(&arguments).unwrap(),
            },
        });
        events.push_back(json!({"Agent":{"ToolStarted":{"call":call}}}));
        let lines = match index {
            1 => 50,
            5 => 30,
            _ => 8,
        };
        let stdout: String = (1..=lines)
            .map(|line| format!("DISCLOSURE-{index:02} line {line:02}: 工具输出定位标记\n"))
            .collect();
        let content = if file {
            stdout
        } else {
            serde_json::to_string(&json!({
                "exit_code":0,
                "elapsed":{"secs":0,"nanos":250000000},
                "stdout":stdout,
                "stderr":"",
                "timed_out":false,
                "cancelled":false,
                "output_truncated":false,
                "retained_output_truncated":false,
                "sandbox":"native",
            }))
            .unwrap()
        };
        events.push_back(json!({"Agent":{"ToolFinished":{
            "call":call,"content":content,"changes":[],
        }}}));
        if live && index == 1 {
            // One event is delivered per poll, leaving the first completed
            // result available for expansion while the group keeps running.
            events.extend((0..120).map(|_| json!({"Agent":{"Model":{"ReasoningDelta":""}}})));
        }
    }
    events.push_back(json!({"Agent":{"Model":{"TextDelta":"DISCLOSURES-END：五个命令已完成。"}}}));
    events
}
