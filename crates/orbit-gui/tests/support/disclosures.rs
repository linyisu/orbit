//! Long completed tool details, including expansion during a running group.
use serde_json::{Value, json};
use std::collections::VecDeque;

pub(super) fn events(workspace: &str, live: bool) -> VecDeque<Value> {
    let mut events = VecDeque::new();
    for index in 1..=5 {
        if index > 1 {
            events.push_back(json!({
                "Agent": {
                    "Model": {
                        "ReasoningDelta": format!("检查第 {index} 个命令前的状态。")
                    }
                }
            }));
        }
        let file = matches!(index, 1 | 5);
        let arguments = if file {
            json!({
                "path": format!("src/disclosure-{index:02}.txt")
            })
        } else {
            json!({
                "command": format!("printf 'disclosure-{index:02}\\n'"),
                "cwd": workspace,
                "timeout_seconds": 30,
                "background": false,
            })
        };
        let call = json!({
            "id": format!("disclosure-{index:02}"),
            "function": {
                "name": if file { "read_file" } else { "run_command" },
                "arguments": serde_json::to_string(&arguments).unwrap(),
            },
        });
        events.push_back(json!({
            "Agent": {
                "ToolStarted": {
                    "call": call
                }
            }
        }));
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
                "exit_code": 0,
                "elapsed": {
                    "secs": 0,
                    "nanos": 250_000_000
                },
                "stdout": stdout,
                "stderr": "",
                "timed_out": false,
                "cancelled": false,
                "output_truncated": false,
                "retained_output_truncated": false,
                "sandbox": "native",
            }))
            .unwrap()
        };
        events.push_back(json!({
            "Agent": {
                "ToolFinished": {
                    "call": call,
                    "content": content,
                    "changes": [],
                }
            }
        }));
        if live && index == 1 {
            // One event is delivered per poll, leaving the first completed
            // result available for expansion while the group keeps running.
            events.extend((0..120).map(|_| {
                json!({
                    "Agent": {
                        "Model": {
                            "ReasoningDelta": ""
                        }
                    }
                })
            }));
        }
    }
    events.push_back(json!({
        "Agent": {
            "Model": {
                "TextDelta": "DISCLOSURES-END：五个命令已完成。"
            }
        }
    }));
    events
}

pub(super) fn flow_events() -> VecDeque<Value> {
    let first = json!({
        "id": "disclosure-flow-first",
        "function": {
            "name": "read_file",
            "arguments": "{\"path\":\"src/main.rs\"}"
        },
    });
    let resumed = json!({
        "id": "disclosure-flow-resumed",
        "function": {
            "name": "read_file",
            "arguments": "{\"path\":\"Cargo.toml\"}"
        },
    });
    let reasoning = format!(
        "FLOW-REASONING-BEGIN: The provided text highlights a duplicated phrase within a new article, specifically copied from another previously corrected article. This indicates a potential copy-paste error during the new article's creation. The goal is to address this duplicated content while retaining its original meaning.\n\n\
         A second paragraph checks that English text wraps inside the reasoning container at narrow window sizes and that every continuation line preserves the same indentation.\n\n\
         Unbroken token: {}\n\n\
         中文换行检查：{}\n\nFLOW-REASONING-END",
        "reasoning_boundary_".repeat(24),
        "思考内容应保持在阅读区内，不能越过左边框，也不能溢出到聊天界面。".repeat(5),
    );
    [
        json!({"Agent":{"Model":{"ReasoningDelta":reasoning}}}),
        json!({"Agent":{"ToolStarted":{"call":first}}}),
        json!({
            "Agent": {
                "ToolFinished": {
                    "call": first,
                    "content": "FLOW-FIRST-TOOL: source inspected",
                    "changes": []
                }
            }
        }),
        // An empty model delta must not end the execution presentation.
        json!({"Agent":{"Model":{"TextDelta":""}}}),
        json!({"Agent":{"Model":{"TextDelta":"FLOW-FIRST: 初步检查已完成。"}}}),
        json!({"Agent":{"Model":{"ReasoningDelta":"FLOW-LATE-REASONING: 中途输出文字后，思考和工具记录应保持展开。"}}}),
        json!({"Agent":{"Model":{"TextDelta":" FLOW-CONTINUATION: 继续说明检查进展，任务还未完成。"}}}),
        json!({"Agent":{"ToolStarted":{"call":resumed}}}),
        json!({
            "Agent": {
                "ToolFinished": {
                    "call": resumed,
                    "content": "FLOW-RESUMED-TOOL: manifest inspected",
                    "changes": []
                }
            }
        }),
        json!({"Agent":{"Model":{"ReasoningDelta":"FLOW-RESUMED-REASONING: 后续工具执行继续，展开状态应保持稳定。"}}}),
        json!({"Agent":{"Model":{"TextDelta":"FLOW-FINAL: 补充检查完成，开始最终答复。"}}}),
    ]
    .into()
}

pub(super) fn flow_held(workspace: &str, event: &Value) -> bool {
    let text = event["Agent"]["Model"]["TextDelta"].as_str();
    let marker = if text.is_some_and(|text| text.starts_with("FLOW-FIRST:")) {
        "text"
    } else if text.is_some_and(|text| text.starts_with(" FLOW-CONTINUATION:")) {
        "stream"
    } else if event["Agent"]["ToolStarted"]["call"]["id"] == "disclosure-flow-resumed" {
        "resume"
    } else if text.is_some_and(|text| text.starts_with("FLOW-FINAL:")) {
        "final"
    } else {
        return false;
    };
    std::path::Path::new(workspace)
        .join(format!(".fixture-disclosure-flow-{marker}"))
        .exists()
}
