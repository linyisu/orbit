use serde_json::{Value, json};
use std::collections::VecDeque;

pub(super) fn events(workspace: &str, ticks: usize) -> VecDeque<Value> {
    let call = json!({
        "id": "output-command",
        "function": {
            "name": "run_command",
            "arguments": serde_json::to_string(&json!({
                "command": "cargo check --all-targets",
                "cwd": workspace,
                "timeout_seconds": 30,
                "background": false
            }))
            .unwrap()
        }
    });
    let mut events: VecDeque<_> = [json!({
        "Agent": {
            "ToolStarted": {
                "call": call
            }
        }
    })]
    .into();
    events.extend((0..ticks).map(|_| {
        json!({
            "Agent": {
                "Model": {
                    "ReasoningDelta": ""
                }
            }
        })
    }));
    let result = json!({
        "exit_code": 2,
        "elapsed": {
            "secs": 30,
            "nanos": 125_000_000
        },
        "stdout": "final stdout tail\n最后一行标准输出\n",
        "stderr": "error: compilation failed\n最后一行错误输出\n",
        "timed_out": false,
        "cancelled": false,
        "output_truncated": true,
        "stdout_total_bytes": 10_800,
        "stderr_total_bytes": 2_048,
        "retained_output_truncated": false,
        "retained_output_error": null,
    });
    events.push_back(json!({
        "Agent": {
            "ToolFinished": {
                "call": call,
                "content": serde_json::to_string(&result).unwrap(),
                "changes": []
            }
        }
    }));
    events.push_back(json!({
        "Agent": {
            "Model": {
                "TextDelta": "命令返回退出码 2，请检查错误输出。"
            }
        }
    }));
    events
}

pub(super) fn long_events(workspace: &str) -> VecDeque<Value> {
    let values: String = (1..=48)
        .map(|line| format!("    ('register_{line:03}', {line}),\n"))
        .collect();
    let long_line = "long-command-value-".repeat(16);
    let command = format!(
        "python3 - <<'PY'\nvalues = [\n{values}]\nprint({long_line:?})\nfor name, value in \
            values:\n    print(f'{{name}}: {{value}}')\nprint('COMMAND-END')\nPY"
    );
    let call = json!({
        "id": "output-command",
        "function": {
            "name": "run_command",
            "arguments": serde_json::to_string(&json!({
                "command": command,
                "cwd": workspace,
                "timeout_seconds": 30,
                "background": false
            }))
            .unwrap()
        }
    });
    let stdout: String = (1..=100)
        .map(|line| {
            format!(
                "STDOUT-{line:03}: 标准输出定位标记{}\n",
                if line == 50 { &long_line } else { "" }
            )
        })
        .collect();
    let stderr: String = (1..=40)
        .map(|line| format!("STDERR-{line:03}: error: 示例错误输出，保留原始命令结果\n"))
        .collect();
    let result = json!({
        "exit_code": 2,
        "elapsed": {
            "secs": 30,
            "nanos": 125_000_000
        },
        "stdout_total_bytes": stdout.len(),
        "stderr_total_bytes": stderr.len(),
        "stdout": stdout,
        "stderr": stderr,
        "timed_out": false,
        "cancelled": false,
        "output_truncated": false,
        "retained_output_truncated": false,
        "retained_output_error": null,
    });
    let mut events: VecDeque<_> = [json!({
        "Agent": {
            "ToolStarted": {
                "call": call
            }
        }
    })]
    .into();
    events.extend((0..120).map(|_| {
        json!({
            "Agent": {
                "Model": {
                    "ReasoningDelta": ""
                }
            }
        })
    }));
    events.push_back(json!({
        "Agent": {
            "ToolFinished": {
                "call": call,
                "content": serde_json::to_string(&result).unwrap(),
                "changes": []
            }
        }
    }));
    events.push_back(json!({
        "Agent": {
            "Model": {
                "TextDelta": "长命令返回退出码 2。检查命令、stdout 和 stderr 的首尾标记。"
            }
        }
    }));
    events
}

pub(super) fn progress(events: &[Value]) -> Vec<Value> {
    let Some(start) = events
        .iter()
        .rposition(|event| event["Agent"]["ToolStarted"]["call"]["id"] == "output-command")
    else {
        return vec![];
    };
    if events[start..].iter().any(|event| {
        event["Agent"]["ToolFinished"]["call"]["id"] == "output-command"
            || event.get("Finished").is_some()
    }) {
        return vec![];
    }
    let count = events.len() - start;
    let stdout: String = (count.saturating_sub(18)..count)
        .map(|line| format!("build output line {line}\n"))
        .collect();
    vec![json!({
        "call_id": "output-command",
        "stdout": stdout,
        "elapsed_ms": count * 250
    })]
}
