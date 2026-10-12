//! A read-only projection of durable agent events. The service owns execution;
//! views consume this projection without inferring success from a text message.
use orbit_agent::events::AgentEvent;
use orbit_client::protocol::Update;
use orbit_model::types::{StreamEvent, ToolCall};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Phase {
    #[default]
    Ready,
    Waiting,
    Thinking,
    Responding,
    Working,
    Compacting,
    Background,
    Complete,
    Failed,
    Stopped,
}
impl Phase {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Ready => "就绪",
            Self::Waiting => "等待模型响应",
            Self::Thinking => "正在思考",
            Self::Responding => "正在回复",
            Self::Working => "正在执行工具",
            Self::Compacting => "正在整理上下文",
            Self::Background => "等待后台任务",
            Self::Complete => "已完成",
            Self::Failed => "执行失败",
            Self::Stopped => "已停止",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolStatus {
    Running,
    Background,
    Complete,
    Failed,
    Stopped,
}
impl ToolStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Running => "执行中",
            Self::Background => "后台运行",
            Self::Complete => "已完成",
            Self::Failed => "失败",
            Self::Stopped => "已停止",
        }
    }
}

pub(crate) struct ToolRun {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) detail: String,
    pub(crate) status: ToolStatus,
    pub(crate) output: Option<String>,
}

pub(crate) struct ChildAgent {
    pub(crate) id: String,
    pub(crate) role: String,
    pub(crate) status: String,
}

#[derive(Default)]
pub(crate) struct Execution {
    phase: Phase,
    tools: Vec<ToolRun>,
    files: BTreeSet<String>,
    turns: usize,
    agents: Vec<ChildAgent>,
}
impl Execution {
    pub(crate) fn phase(&self) -> Phase {
        self.phase
    }
    pub(crate) fn tools(&self) -> &[ToolRun] {
        &self.tools
    }
    pub(crate) fn files(&self) -> &BTreeSet<String> {
        &self.files
    }
    pub(crate) fn turns(&self) -> usize {
        self.turns
    }
    pub(crate) fn agents(&self) -> &[ChildAgent] {
        &self.agents
    }

    pub(crate) fn apply(&mut self, update: &Update) {
        match update {
            Update::Started { presentation, .. } => {
                if presentation.compact {
                    self.phase = Phase::Compacting;
                } else {
                    self.phase = Phase::Waiting;
                    self.tools.clear();
                    self.turns = 0;
                }
            }
            Update::Finished { status, .. } => {
                self.phase = match status.as_str() {
                    "completed" => Phase::Complete,
                    "cancelled" => Phase::Stopped,
                    _ => Phase::Failed,
                };
                for tool in &mut self.tools {
                    if tool.status == ToolStatus::Running {
                        tool.status = if self.phase == Phase::Failed {
                            ToolStatus::Failed
                        } else {
                            ToolStatus::Stopped
                        };
                    }
                }
            }
            Update::Agent(event) => self.agent(event),
            _ => {}
        }
    }

    fn agent(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::TurnStarted { turn } => {
                self.turns = *turn;
                self.phase = Phase::Waiting;
            }
            AgentEvent::Model(StreamEvent::ReasoningDelta(_)) => self.phase = Phase::Thinking,
            AgentEvent::Model(StreamEvent::TextDelta(_)) => self.phase = Phase::Responding,
            AgentEvent::CompactionStarted => self.phase = Phase::Compacting,
            AgentEvent::ContextCompacted { .. } => self.phase = Phase::Waiting,
            AgentEvent::ToolStarted { call } => {
                self.phase = Phase::Working;
                self.tool(call, ToolStatus::Running);
            }
            AgentEvent::ToolFinished {
                call,
                changes,
                content,
            } => {
                self.tool(call, ToolStatus::Complete);
                self.output(call, content);
                self.files
                    .extend(changes.iter().map(|change| change.path.clone()));
            }
            AgentEvent::ToolFailed { call, error } => {
                self.tool(call, ToolStatus::Failed);
                self.output(call, error);
            }
            AgentEvent::ToolBackgrounded { call, content, .. } => {
                self.tool(call, ToolStatus::Background);
                self.output(call, content);
            }
            AgentEvent::WaitingForCommands { .. } | AgentEvent::WaitingForTasks { .. } => {
                self.phase = Phase::Background
            }
            AgentEvent::Failed { .. } | AgentEvent::CompactionFailed { .. } => {
                self.phase = Phase::Failed
            }
            AgentEvent::Cancelled => self.phase = Phase::Stopped,
            _ => {}
        }
    }

    fn output(&mut self, call: &ToolCall, content: &str) {
        if let Some(tool) = self.tools.iter_mut().find(|tool| tool.id == call.id) {
            tool.output = Some(content.to_owned());
        }
        if call.function.name == "spawn_agent"
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(content)
            && let Some(id) = value["agent_id"].as_str()
        {
            let status = match value["status"].as_str() {
                Some("running" | "queued") => "运行中",
                Some("completed") => "已完成",
                Some("failed") => "失败",
                Some("cancelled" | "interrupted") => "已停止",
                _ => "查看状态",
            }
            .to_owned();
            if let Some(agent) = self.agents.iter_mut().find(|agent| agent.id == id) {
                agent.status = status;
            } else {
                let role = match value["role"].as_str() {
                    Some("review") => "审查",
                    Some("explore") => "探索",
                    Some("develop") => "实现",
                    _ => "Agent",
                };
                self.agents.push(ChildAgent {
                    id: id.to_owned(),
                    role: role.to_owned(),
                    status,
                });
            }
        }
    }

    fn tool(&mut self, call: &ToolCall, status: ToolStatus) {
        if let Some(tool) = self.tools.iter_mut().find(|tool| tool.id == call.id) {
            tool.status = status;
        } else {
            let args = serde_json::from_str::<serde_json::Value>(&call.function.arguments).ok();
            let detail = [
                "path", "command", "cmd", "query", "pattern", "task", "prompt", "agent_id",
            ]
            .iter()
            .find_map(|key| args.as_ref()?.get(key)?.as_str())
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(140)
            .collect();
            self.tools.push(ToolRun {
                id: call.id.clone(),
                name: call.function.name.clone(),
                detail,
                status,
                output: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn update(value: serde_json::Value) -> Update {
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn projection_tracks_real_tools_compaction_and_committed_files() {
        let mut state = Execution::default();
        let call = serde_json::json!({"id":"write-1","function":{"name":"write_file","arguments":"{\"path\":\"src/main.rs\"}"}});
        state.apply(&update(serde_json::json!({"Started":{"prompt":"fix"}})));
        state.apply(&update(
            serde_json::json!({"Agent":{"CompactionStarted":null}}),
        ));
        assert_eq!(state.phase(), Phase::Compacting);
        state.apply(&update(
            serde_json::json!({"Agent":{"ToolStarted":{"call":call}}}),
        ));
        assert!(state.files().is_empty());
        state.apply(&update(serde_json::json!({"Agent":{"ToolFinished":{"call":call,"content":"ok","changes":[{"path":"src/main.rs","before":null,"after":"fn main() {}"}]}}})));
        assert_eq!(state.tools().len(), 1);
        assert_eq!(state.tools()[0].status, ToolStatus::Complete);
        assert!(state.files().contains("src/main.rs"));
        state.apply(&update(
            serde_json::json!({"Finished":{"status":"completed"}}),
        ));
        assert_eq!(state.phase(), Phase::Complete);
        state.apply(&update(serde_json::json!({"Started":{"prompt":"next"}})));
        assert!(state.tools().is_empty());
        assert_eq!(state.files().len(), 1);
    }
    #[test]
    fn failed_run_never_leaves_a_tool_marked_running() {
        let mut state = Execution::default();
        state.apply(&update(serde_json::json!({"Agent":{"ToolStarted":{"call":{"id":"one","function":{"name":"run_command","arguments":"{}"}}}}})));
        state.apply(&update(serde_json::json!({"Finished":{"status":"failed"}})));
        assert_eq!(state.tools()[0].status, ToolStatus::Failed);
        assert_eq!(state.phase(), Phase::Failed);
    }

    #[test]
    fn delegated_work_stays_pending_until_its_own_completion_event() {
        let mut state = Execution::default();
        let call = serde_json::json!({"id":"delegate","function":{"name":"spawn_agent","arguments":"{\"task\":\"review\"}"}});
        let running = serde_json::json!({"agent_id":"review-1","role":"review","status":"running"})
            .to_string();
        state.apply(&update(serde_json::json!({"Agent":{"ToolBackgrounded":{"call":call,"job_id":"job-1","content":running}}})));
        state.apply(&update(
            serde_json::json!({"Finished":{"status":"completed"}}),
        ));
        assert_eq!(state.tools()[0].status, ToolStatus::Background);
        assert_eq!(state.agents()[0].status, "运行中");
        let completed =
            serde_json::json!({"agent_id":"review-1","role":"review","status":"completed"})
                .to_string();
        state.apply(&update(serde_json::json!({"Agent":{"ToolFinished":{"call":call,"content":completed,"changes":[]}}})));
        assert_eq!(state.tools().len(), 1);
        assert_eq!(state.tools()[0].status, ToolStatus::Complete);
        assert_eq!(state.agents().len(), 1);
        assert_eq!(state.agents()[0].status, "已完成");
    }
}
