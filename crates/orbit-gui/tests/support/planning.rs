//! Review is deliberately held inside a busy run until the user approves.
use serde_json::{Value, json};
use std::collections::VecDeque;

pub(super) fn seed(workspace: &str) -> (Vec<Value>, VecDeque<Value>, Value) {
    let read = json!({
        "id": "plan-read",
        "function": {
            "name": "read_file",
            "arguments": "{\"path\":\"src/main.rs\"}"
        }
    });
    let submit = json!({
        "id": "plan-submit",
        "function": {
            "name": "submit_plan",
            "arguments": "{\"generation\":7}"
        }
    });
    let history = vec![
        json!({
            "Started": {
                "prompt": "先制定方案，等待我批准后执行。"
            }
        }),
        json!({
            "Agent": {
                "ToolStarted": {
                    "call": read
                }
            }
        }),
        json!({
            "Agent": {
                "ToolFinished": {
                    "call": read,
                    "content": "已检查入口。",
                    "changes": []
                }
            }
        }),
        json!({
            "Agent": {
                "Model": {
                    "TextDelta": "方案已整理，等待你审阅；批准后继续当前任务。"
                }
            }
        }),
        json!({
            "Agent": {
                "ToolStarted": {
                    "call": submit
                }
            }
        }),
    ];
    let pending = [
        json!({
            "Agent": {
                "ToolFinished": {
                    "call": submit,
                    "content": "用户已明确批准计划。",
                    "changes": []
                }
            }
        }),
        json!({
            "Agent": {
                "Model": {
                    "TextDelta": "PLAN-APPROVED: 已收到批准并继续执行。"
                }
            }
        }),
        json!({
            "Finished": {
                "status": "completed"
            }
        }),
    ]
    .into();
    let plan = json!({
        "generation": 7,
        "mode": "plan",
        "plan": {
            "id": "plan-review",
            "workspace": workspace,
            "revision": 1,
            "title": "批准后继续当前任务",
            "markdown": "## 实现步骤\n\n1. 检查项目入口。\n2. 批准后继续实现。\n3. 验证结果。",
            "steps": ["检查入口", "继续实现", "验证结果"],
            "status": "review"
        }
    });
    (history, pending, plan)
}
