//! Owned children with stored status transitions, independent of parent replay.
use orbit_protocol::subagents::{Action, Agent, Snapshot};
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path};

struct Child {
    agent: Agent,
    updates: Vec<Value>,
    cancelling: usize,
    finishing: usize,
}

#[derive(Default)]
pub(super) struct FixtureSubagents {
    parents: HashMap<String, Vec<Child>>,
    message: i64,
}

impl FixtureSubagents {
    pub fn prepare(&mut self, workspace: &str, parent: &str) {
        if self.parents.contains_key(parent) {
            return;
        }
        let full = Path::new(workspace).join(".fixture-subagents").exists();
        let agents = if full && parent == "demo" {
            (0..35)
                .map(|index| {
                    let id = match index {
                        0 => "child".into(),
                        1 => "child-running".into(),
                        2 => "child-develop".into(),
                        _ => format!("stored-child-{index}"),
                    };
                    let agent = Agent {
                        id,
                        role: if index == 2 { "develop" } else { "explore" }.into(),
                        model: "fixture::local".into(),
                        task: format!("已委派子任务 {index}：检查对应工作区并报告验证结果"),
                        workspace: if index == 2 {
                            format!("{workspace}/worktrees/child-develop")
                        } else {
                            workspace.into()
                        },
                        status: if index == 1 { "running" } else { "completed" }.into(),
                        run: 101 + index as i64,
                    };
                    Child {
                        agent,
                        updates: vec![json!({"Agent":{"Model":{"TextDelta":"子任务初始回答"}}})],
                        cancelling: 0,
                        finishing: 0,
                    }
                })
                .collect()
        } else if full {
            vec![Child {
                agent: Agent {
                    id: format!("foreign-child-{parent}"),
                    role: "review".into(),
                    model: "fixture::local".into(),
                    task: "另一父会话的独立子任务".into(),
                    workspace: workspace.into(),
                    status: "completed".into(),
                    run: 500,
                },
                updates: vec![],
                cancelling: 0,
                finishing: 0,
            }]
        } else if self.parents.is_empty() {
            vec![Child {
                agent: Agent {
                    id: "child".into(),
                    role: "explore".into(),
                    model: "fixture::local".into(),
                    task: "检查项目结构".into(),
                    workspace: workspace.into(),
                    status: "completed".into(),
                    run: 101,
                },
                updates: vec![json!({"Agent":{"Model":{"TextDelta":"子任务初始回答"}}})],
                cancelling: 0,
                finishing: 0,
            }]
        } else {
            vec![]
        };
        self.parents.insert(parent.into(), agents);
    }

    pub fn execute(&mut self, workspace: &str, request: &Value) -> Value {
        let parent = request["session"].as_str().unwrap_or_default();
        self.prepare(workspace, parent);
        if request["expected_workspace"] != workspace {
            return error("工作区已切换，请重新打开子 Agent 列表");
        }
        let action: Action = match serde_json::from_value(request["action"].clone()) {
            Ok(action) => action,
            Err(problem) => return error(&problem.to_string()),
        };
        match action {
            Action::List { offset } => {
                if offset > 20_000 {
                    return error("子 Agent 列表页码超出限制");
                }
                let children = self.parents.get_mut(parent).unwrap();
                for child in children.iter_mut() {
                    if child.agent.id == "child-running"
                        && Path::new(workspace)
                            .join(".fixture-subagents-complete")
                            .exists()
                    {
                        child.agent.status = "completed".into();
                    }
                    if child.cancelling > 0 {
                        child.cancelling -= 1;
                        if child.cancelling == 0 {
                            child.agent.status = "cancelled".into();
                            child
                                .updates
                                .push(json!({"Finished":{"status":"cancelled"}}));
                        }
                    } else if child.finishing > 0 {
                        child.finishing -= 1;
                        if child.finishing == 0 {
                            child.agent.status = "completed".into();
                            child
                                .updates
                                .push(json!({"Finished":{"status":"completed"}}));
                        }
                    }
                }
                let agents: Vec<_> = children
                    .iter()
                    .skip(offset)
                    .take(32)
                    .map(|child| child.agent.clone())
                    .collect();
                let end = offset.saturating_add(agents.len());
                snapshot(Snapshot::List {
                    agents,
                    next: (end < children.len()).then_some(end),
                })
            }
            Action::Send { id, message } => self.send(workspace, parent, &id, &message, false),
            Action::Cancel { id, run } => {
                let Some(child) = self
                    .parents
                    .get_mut(parent)
                    .unwrap()
                    .iter_mut()
                    .find(|child| child.agent.id == id)
                else {
                    return error("子 Agent 不属于当前会话");
                };
                if child.agent.run != run || run <= 0 {
                    return error("子 Agent 已开始另一轮任务，请刷新后重试");
                }
                if child.agent.status != "running" {
                    return error("子 Agent 当前任务已结束，请刷新列表");
                }
                child.finishing = 0;
                child.cancelling = 2;
                snapshot(Snapshot::Stopping { id, run })
            }
        }
    }

    pub fn read(&mut self, workspace: &str, request: &Value) -> Value {
        let parent = request["session"].as_str().unwrap_or_default();
        self.prepare(workspace, parent);
        let id = request["agent"].as_str().unwrap_or_default();
        let Some(child) = self
            .parents
            .get(parent)
            .unwrap()
            .iter()
            .find(|child| child.agent.id == id)
        else {
            return error("该记录不是当前会话的子代理。");
        };
        let after = request["after"].as_u64().unwrap_or_default() as usize;
        if after > child.updates.len() {
            return error("子 Agent 游标已失效，请重新打开对话");
        }
        json!({"type":"subagent","value":{"next":child.updates.len(),"more":false,"busy":child.agent.status=="running","model":child.agent.model,"updates":&child.updates[after..],"approvals":[]}})
    }

    pub fn send_legacy(&mut self, workspace: &str, request: &Value) -> Value {
        let parent = request["session"].as_str().unwrap_or_default();
        self.prepare(workspace, parent);
        self.send(
            workspace,
            parent,
            request["agent"].as_str().unwrap_or_default(),
            request["message"].as_str().unwrap_or_default(),
            true,
        )
    }

    fn send(
        &mut self,
        workspace: &str,
        parent: &str,
        id: &str,
        message: &str,
        legacy: bool,
    ) -> Value {
        let Some(child) = self
            .parents
            .get_mut(parent)
            .unwrap()
            .iter_mut()
            .find(|child| child.agent.id == id)
        else {
            return error("子 Agent 不属于当前会话");
        };
        let message = message.trim();
        if message.is_empty() || message.len() > 32_768 {
            return error("消息需要 1–32768 字节");
        }
        if child.cancelling > 0 {
            return error("子 Agent 正在停止，请等待任务终态后重试");
        }
        if std::fs::remove_file(Path::new(workspace).join(".fixture-subagents-send-fail-once"))
            .is_ok()
        {
            return error("测试消息发送失败，草稿应保留后重试");
        }
        self.message += 1;
        let queued = child.agent.status == "running";
        if !queued {
            child.agent.run += 1000;
            child.agent.status = "running".into();
            child.finishing = 3;
        }
        child
            .updates
            .push(json!({"Agent":{"Model":{"TextDelta":message}}}));
        if legacy {
            json!({"type":"ok"})
        } else {
            snapshot(Snapshot::Sent {
                id: id.into(),
                status: if queued { "queued" } else { "running" }.into(),
                message_id: queued.then_some(self.message),
            })
        }
    }
}

fn snapshot(value: Snapshot) -> Value {
    json!({"type":"subagents","value":value})
}
fn error(message: &str) -> Value {
    json!({"type":"error","value":message})
}
