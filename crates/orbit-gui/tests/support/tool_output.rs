use serde_json::{Value, json};
use std::collections::VecDeque;

pub(super) fn events(workspace: &str, ticks: usize) -> VecDeque<Value> {
    let call = json!({
        "id": "output-command",
        "function": {
            "name": "Exec",
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
            "name": "Exec",
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
        "stderr": format!("diagnostic frame {count}\n"),
        "elapsed_ms": count * 250
    })]
}

pub(super) fn complete_history(workspace: &str) -> Vec<Value> {
    let mut events = vec![json!({"Started": {"prompt": "检查生成的命令与网页记录"}})];
    for mut event in long_events(workspace) {
        if event["Agent"]["Model"]["ReasoningDelta"] == "" {
            continue;
        }
        if let Some(content) = event["Agent"]["ToolFinished"]["content"].as_str() {
            let mut result: Value = serde_json::from_str(content).unwrap();
            result["stdout"] = "preview stdout tail".into();
            result["stderr"] = "preview stderr tail".into();
            result["output_truncated"] = true.into();
            result["output_id"] = "cmd-inspector".into();
            event["Agent"]["ToolFinished"]["content"] = result.to_string().into();
        }
        events.push(event);
    }
    let call = json!({
        "id": "web-inspector",
        "function": {
            "name": "WebFetch",
            "arguments": json!({"url": "https://example.org/generated"}).to_string()
        }
    });
    events.push(json!({"Agent": {"ToolStarted": {"call": call}}}));
    events.push(json!({
        "Agent": {
            "ToolFinished": {
                "call": call,
                "content": json!({
                    "content": "preview webpage",
                    "snapshot_id": "web-inspector",
                    "has_more": true
                }).to_string(),
                "changes": []
            }
        }
    }));
    events.push(json!({"Finished": {"status": "completed", "elapsed_ms": 2}}));
    events
}

pub(super) fn retained_page(request: &Value) -> Value {
    let source = &request["source"];
    let content = if source["kind"] == "web" && source["snapshot_id"] == "web-inspector" {
        (1..=80)
            .map(|line| format!("DOCUMENT-{line:03}: generated document paragraph\n"))
            .collect::<String>()
    } else if source["kind"] == "command" && source["output_id"] == "cmd-inspector" {
        let events = long_events("");
        let result = events
            .iter()
            .find_map(|event| event["Agent"]["ToolFinished"]["content"].as_str())
            .unwrap();
        let result: Value = serde_json::from_str(result).unwrap();
        result[source["stream"]
            .as_str()
            .unwrap_or("stdout")]
        .as_str()
        .unwrap_or_default()
        .to_owned()
    } else {
        return json!({"type": "error", "value": "missing fixture output"});
    };
    let start = request["offset"].as_u64().unwrap_or(0) as usize;
    if start > content.len() || !content.is_char_boundary(start) {
        return json!({"type": "error", "value": "invalid output offset"});
    }
    let mut end = (start + 700).min(content.len());
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    json!({
        "type": "tool_output",
        "value": {
            "content": content[start..end],
            "next_offset": (end < content.len()).then_some(end),
            "read_offset": end
        }
    })
}

pub(super) fn specialized_history(workspace: &str) -> Vec<Value> {
    let image_path = std::path::Path::new(workspace).join("generated-checker.png");
    let image = image::RgbImage::from_fn(96, 64, |x, y| {
        image::Rgb(if (x / 16 + y / 16) % 2 == 0 {
            [60, 160, 190]
        } else {
            [220, 180, 70]
        })
    });
    image.save(image_path).unwrap();
    let source = (97..=156)
        .map(|line| {
            format!(
                "{line}:     let generated_{line} = \"{}\";\n",
                "word ".repeat(12)
            )
        })
        .collect::<String>();
    let mut history = vec![json!({"Started": {"prompt": "Inspect generated tool views"}})];
    for (id, name, args, output) in [
        (
            "wrapped-command",
            "Exec",
            json!({
                "command": format!(
                    "samplectl inspect {} --label '换行末尾 COMMAND-END'",
                    (1..=14).map(|n| format!("--record=sample-{n:02} ")).collect::<String>()
                ),
                "cwd": workspace
            }),
            json!({
                "exit_code": 0,
                "elapsed": {"secs": 0, "nanos": 12_000_000},
                "stdout": "Generated command preview\n",
                "stderr": "",
                "timed_out": false,
                "cancelled": false
            }),
        ),
        (
            "short-command",
            "Exec",
            json!({"command": "samplectl status", "cwd": workspace}),
            json!({
                "exit_code": 0,
                "elapsed": {"secs": 0, "nanos": 12_000_000},
                "stdout": "Ready\n",
                "stderr": "",
                "timed_out": false,
                "cancelled": false
            }),
        ),
        (
            "source",
            "ReadFile",
            json!({"path": "generated.rs", "start_line": 97}),
            json!({"start_line": 97, "content": source}),
        ),
        (
            "tasks",
            "ListTasks",
            json!({}),
            json!({
                "tasks": [{
                    "id": 2,
                    "title": "Generated review",
                    "details": "Check generated content",
                    "status": "in_progress"
                }]
            }),
        ),
        (
            "evidence",
            "CheckEvidence",
            json!({}),
            json!({"baseline_available": true, "command_count": 0, "commands": [], "changes": []}),
        ),
        (
            "image",
            "ReadImage",
            json!({"path": "generated-checker.png"}),
            json!({"path": "generated-checker.png", "loaded": true}),
        ),
    ] {
        let call = json!({"id": id, "function": {"name": name, "arguments": args.to_string()}});
        history.extend([
            json!({"Agent": {"ToolStarted": {"call": call}}}),
            json!({
                "Agent": {
                    "ToolFinished": {
                        "call": call,
                        "content": output.to_string(),
                        "changes": []
                    }
                }
            }),
        ]);
    }
    let call = json!({
        "id": "invalid-input",
        "function": {
            "name": "ReadFile",
            "arguments": json!({"path": "generated.rs", "start_line": 0}).to_string()
        }
    });
    history.extend([
        json!({"Agent": {"ToolStarted": {"call": call}}}),
        json!({
            "Agent": {
                "ToolFailed": {
                    "call": call,
                    "error": json!({
                        "error": {
                            "code": "invalid_arguments",
                            "message": "start_line must be greater than zero",
                            "recovery": "correct_arguments",
                            "retryable": false,
                            "message_truncated": false
                        }
                    }).to_string()
                }
            }
        }),
    ]);
    history
}
