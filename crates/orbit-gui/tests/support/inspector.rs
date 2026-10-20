//! Generated, model-free tool, plan, change and subtask evidence.
use serde_json::{Value, json};
use std::{collections::VecDeque, path::Path};

pub(super) fn seed(workspace: &str) -> (Vec<Value>, VecDeque<Value>, Value) {
    let mut history = vec![
        json!({
            "Started": {
                "prompt": "检查会话详情的执行状态、计划、文件变更和协作任务。"
            }
        }),
        json!({
            "Agent": {
                "TurnStarted": {
                    "turn": 8
                }
            }
        }),
        json!({
            "Agent": {
                "Model": {
                    "TextDelta": "已检查项目入口与模块依赖，正在验证会话详情的交互。\n\n右侧保留本轮执行证据：命令结果、文件变更和协作任务都可以直接打开。"
                }
            }
        }),
    ];
    for (id, name, arguments, content, changes) in [
        (
            "inspector-manifest",
            "ReadFile",
            json!({
                "path": "Cargo.toml"
            }),
            "[workspace]\nmembers = [\"crates/orbit-gui\", \"crates/orbit-tui\"]\n".into(),
            json!([]),
        ),
        (
            "inspector-status",
            "Exec",
            json!({
                "command": "git status --short --branch",
                "cwd": workspace
            }),
            command_result(0, 125, "## main\n M src/workspace/inspector.rs\n", ""),
            json!([]),
        ),
        (
            "inspector-search-failed",
            "Exec",
            json!({
                "command": "rg -n --glob '*.rs' 'execution_state|inspector_width' \
                    src/workspace/inspector.rs src/workbench/storage.rs",
                "cwd": workspace
            }),
            command_result(
                2,
                46,
                "",
                "rg: src/workbench/storage.rs: No such file or directory (os error 2)\n",
            ),
            json!([]),
        ),
        (
            "inspector-layout",
            "WriteFile",
            json!({
                "path": "src/workspace/inspector.rs"
            }),
            "已更新执行记录布局。".into(),
            json!([{
                "path": "src/workspace/inspector.rs",
                "before": "// previous layout\n",
                "after": "// compact inspector layout\n"
            }]),
        ),
        (
            "inspector-long-path",
            "WriteFile",
            json!({
                "path": "crates/orbit-gui/src/workspace/components/execution/very_long_panel_name_f\
                    or_truncation.rs"
            }),
            "已统一窄面板的文本边界。".into(),
            json!([{
                "path": "crates/orbit-gui/src/workspace/components/execution/very_long_panel_na\
                        me_for_truncation.rs",
                "before": null,
                "after": "// bounded content\n"
            }]),
        ),
        (
            "inspector-docs",
            "WriteFile",
            json!({
                "path": "docs/界面设计/会话详情交互规范.md"
            }),
            "已补充使用说明。".into(),
            json!([{
                "path": "docs/界面设计/会话详情交互规范.md",
                "before": null,
                "after": "# 会话详情\n"
            }]),
        ),
    ] {
        let call = call(id, name, arguments);
        history.push(json!({
            "Agent": {
                "ToolStarted": {
                    "call": call
                }
            }
        }));
        history.push(json!({
            "Agent": {
                "ToolFinished": {
                    "call": call,
                    "content": content,
                    "changes": changes
                }
            }
        }));
    }
    // Enough rows to exercise the local scrollbar at compact window heights.
    for index in 1..=8 {
        let call = call(
            &format!("inspector-source-{index}"),
            "ReadFile",
            json!({
                "path": format!("src/workspace/module_{index}.rs")
            }),
        );
        history.push(json!({
            "Agent": {
                "ToolStarted": {
                    "call": call
                }
            }
        }));
        history.push(json!({
            "Agent": {
                "ToolFinished": {
                    "call": call,
                    "content": "模块状态、布局边界和键盘入口已检查。",
                    "changes": []
                }
            }
        }));
    }
    for (id, role, status, task) in [
        ("child", "explore", "completed", "梳理会话详情的数据来源"),
        (
            "child-running",
            "explore",
            "running",
            "检查窄窗口与大字体的交互",
        ),
        (
            "child-develop",
            "develop",
            "completed",
            "统一状态图标与命令摘要",
        ),
    ] {
        let call = call(
            &format!("inspector-spawn-{id}"),
            "SpawnAgent",
            json!({
                "role": role,
                "task": task
            }),
        );
        let content = json!({
            "agent_id": id,
            "role": role,
            "status": status
        })
        .to_string();
        history.push(json!({
            "Agent": {
                "ToolStarted": {
                    "call": call
                }
            }
        }));
        if status == "running" {
            history.push(json!({
                "Agent": {
                    "ToolBackgrounded": {
                        "call": call,
                        "job_id": id,
                        "content": content
                    }
                }
            }));
        } else {
            history.push(json!({
                "Agent": {
                    "ToolFinished": {
                        "call": call,
                        "content": content,
                        "changes": []
                    }
                }
            }));
        }
    }
    let call = call(
        "inspector-check",
        "Exec",
        json!({
            "command": "cargo test -p orbit-gui --features test-support --locked",
            "cwd": workspace,
            "timeout_seconds": 120,
            "background": false
        }),
    );
    history.push(json!({
        "Agent": {
            "ToolStarted": {
                "call": call
            }
        }
    }));
    let pending = [
        json!({
            "Agent": {
                "ToolFinished": {
                    "call": call,
                    "content": command_result(0,
                        4_280,
                        "test result: ok. 24 passed; 0 failed\n",
                        ""),
                    "changes": []
                }
            }
        }),
        json!({"Agent":{"Model":{"TextDelta":"\n\n验证完成。执行记录中保留了一次失败的搜索及后续修正；最终检查已通过。"}}}),
        json!({"Finished":{"status":"completed"}}),
    ]
    .into();
    let planning = json!({
        "generation": 1,
        "mode": "build",
        "plan": {
            "id": "inspector-plan",
            "workspace": workspace,
            "revision": 2,
            "title": "完善会话详情的执行证据与交互",
            "markdown": "## 会话详情\n\n1. 统一工具状态和命令摘要。\n2. 保留计划、文件变更和协作任务的直接入口。\n3. 验证窄窗口、大字体、滚动和键盘操作。",
            "steps": [
                "统一工具状态和命令摘要",
                "保留计划、文件变更和协作任务的直接入口",
                "验证窄窗口、大字体、滚动和键盘操作"
            ],
            "status": "approved"
        }
    });
    (history, pending, planning)
}

pub(super) fn held(workspace: &str, event: &Value) -> bool {
    event["Agent"]["ToolFinished"]["call"]["id"] == "inspector-check"
        && Path::new(workspace)
            .join(".fixture-inspector-hold")
            .exists()
}

fn call(id: &str, name: &str, arguments: Value) -> Value {
    json!({
        "id": id,
        "function": {
            "name": name,
            "arguments": arguments.to_string()
        }
    })
}

fn command_result(code: i32, elapsed_ms: u64, stdout: &str, stderr: &str) -> String {
    json!({
        "exit_code": code,
        "elapsed": {
            "secs": elapsed_ms / 1_000,
            "nanos": (elapsed_ms % 1_000) * 1_000_000
        },
        "stdout": stdout,
        "stderr": stderr,
        "stdout_total_bytes": stdout.len(),
        "stderr_total_bytes": stderr.len(),
        "timed_out": false,
        "cancelled": false,
        "output_truncated": false,
        "retained_output_truncated": false
    })
    .to_string()
}
