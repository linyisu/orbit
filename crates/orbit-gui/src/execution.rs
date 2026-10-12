//! A read-only projection of durable agent events. The service owns execution;
//! views consume this projection without inferring success from a text message.
use orbit_agent::events::AgentEvent;
use orbit_client::protocol::Update;
use orbit_model::types::{StreamEvent, ToolCall};
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicU64, Ordering},
};

// A restored projection is a new observer. Never reattach an old output sheet
// to a replayed call merely because a provider reused its call ID.
static TOOL_GENERATION: AtomicU64 = AtomicU64::new(1);

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
            Self::Complete => "已返回",
            Self::Failed => "失败",
            Self::Stopped => "已停止",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ToolRun {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) detail: String,
    pub(crate) status: ToolStatus,
    pub(crate) output: Option<String>,
    pub(crate) arguments: String,
    pub(crate) generation: u64,
    pub(crate) stdout_preview: Option<String>,
    pub(crate) elapsed_ms: Option<u64>,
    pub(crate) live: bool,
    pub(crate) run_finished: bool,
}

impl ToolRun {
    pub(crate) fn is_foreground_command(&self) -> bool {
        self.name == "run_command"
            && serde_json::from_str::<serde_json::Value>(&self.arguments)
                .ok()
                .is_some_and(|args| args["background"] != true)
    }

    pub(crate) fn command_result(&self) -> Option<CommandResult> {
        (self.name == "run_command")
            .then_some(())
            .and_then(|_| serde_json::from_str(self.output.as_deref()?).ok())
    }
}

/// The command tool's result schema, not a guessed shell-output wrapper. The
/// remaining runtime fields are optional metadata and stay in the raw result.
#[derive(Clone, Debug, serde::Deserialize)]
pub(crate) struct CommandResult {
    pub(crate) exit_code: Option<i32>,
    pub(crate) elapsed: std::time::Duration,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) timed_out: bool,
    pub(crate) cancelled: bool,
    pub(crate) output_truncated: bool,
    pub(crate) stdout_total_bytes: u64,
    pub(crate) stderr_total_bytes: u64,
    #[serde(default)]
    pub(crate) retained_output_truncated: bool,
    #[serde(default)]
    pub(crate) retained_output_error: Option<String>,
}

impl CommandResult {
    pub(crate) fn status(&self) -> ToolStatus {
        if self.cancelled {
            ToolStatus::Stopped
        } else if self.timed_out || self.exit_code != Some(0) {
            ToolStatus::Failed
        } else {
            ToolStatus::Complete
        }
    }
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

    /// Progress contains replacement stdout tails, not append-only chunks.
    /// Background receipts and historical completed calls are never live.
    pub(crate) fn apply_progress(&mut self, updates: &[orbit_protocol::ToolProgress]) {
        for update in updates {
            if let Some(tool) = self.tools.iter_mut().find(|tool| {
                tool.id == update.call_id
                    && tool.status == ToolStatus::Running
                    && tool.is_foreground_command()
            }) {
                if let Some(stdout) = &update.stdout {
                    tool.stdout_preview = Some(stdout.clone());
                    tool.live = true;
                }
                if let Some(elapsed) = update.elapsed_ms {
                    tool.elapsed_ms = Some(elapsed);
                }
            }
        }
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
                    tool.live = false;
                    tool.run_finished = true;
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
                self.tools.retain(|tool| tool.id != call.id);
                self.tool(call, ToolStatus::Running);
            }
            AgentEvent::ToolFinished {
                call,
                changes,
                content,
            } => {
                if !self.accepts(call) {
                    return;
                }
                self.tool(call, ToolStatus::Complete);
                self.output(call, content);
                self.files
                    .extend(changes.iter().map(|change| change.path.clone()));
            }
            AgentEvent::ToolFailed { call, error } => {
                if !self.accepts(call) {
                    return;
                }
                self.tool(call, ToolStatus::Failed);
                self.output(call, error);
            }
            AgentEvent::ToolBackgrounded { call, content, .. } => {
                if !self.accepts(call) {
                    return;
                }
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

    fn accepts(&self, call: &ToolCall) -> bool {
        self.tools
            .iter()
            .find(|tool| tool.id == call.id)
            .is_none_or(|tool| {
                tool.name == call.function.name && tool.arguments == call.function.arguments
            })
    }

    fn output(&mut self, call: &ToolCall, content: &str) {
        if let Some(tool) = self.tools.iter_mut().find(|tool| tool.id == call.id) {
            tool.output = Some(content.to_owned());
            tool.live = false;
            if let Some(result) = tool.command_result() {
                tool.status = result.status();
                tool.elapsed_ms = Some(result.elapsed.as_millis().min(u64::MAX as u128) as u64);
            }
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
                arguments: call.function.arguments.clone(),
                generation: TOOL_GENERATION.fetch_add(1, Ordering::Relaxed),
                stdout_preview: None,
                elapsed_ms: None,
                live: false,
                run_finished: false,
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

    fn command_result(exit_code: serde_json::Value, timed_out: bool, cancelled: bool) -> String {
        serde_json::json!({
            "exit_code":exit_code,"elapsed":{"secs":1,"nanos":234000000},
            "stdout":"out\n","stderr":"err\n","timed_out":timed_out,"cancelled":cancelled,
            "output_truncated":false,"stdout_total_bytes":4,"stderr_total_bytes":4,
            "retained_output_truncated":false
        })
        .to_string()
    }

    #[test]
    fn command_status_uses_the_real_result_including_exit_timeout_and_cancellation() {
        for (exit, timed_out, cancelled, expected) in [
            (serde_json::json!(0), false, false, ToolStatus::Complete),
            (serde_json::json!(7), false, false, ToolStatus::Failed),
            (serde_json::Value::Null, true, false, ToolStatus::Failed),
            (serde_json::Value::Null, false, true, ToolStatus::Stopped),
        ] {
            let mut state = Execution::default();
            let call = ToolCall::function(
                "command",
                "run_command",
                serde_json::json!({"command":"example"}),
            );
            state.agent(&AgentEvent::ToolStarted { call: call.clone() });
            state.agent(&AgentEvent::ToolFinished {
                call,
                content: command_result(exit, timed_out, cancelled),
                changes: vec![],
            });
            assert_eq!(state.tools()[0].status, expected);
            assert_eq!(state.tools()[0].elapsed_ms, Some(1234));
        }
    }

    #[test]
    fn stdout_progress_replaces_tail_preserves_omitted_data_and_never_revives_completion() {
        let mut state = Execution::default();
        let call = ToolCall::function(
            "command",
            "run_command",
            serde_json::json!({"command":"example"}),
        );
        state.agent(&AgentEvent::ToolStarted { call: call.clone() });
        let progress = |stdout: Option<&str>| orbit_protocol::ToolProgress {
            call_id: "command".into(),
            stdout: stdout.map(str::to_owned),
            elapsed_ms: Some(50),
        };
        state.apply_progress(&[progress(Some("first\n"))]);
        state.apply_progress(&[progress(None)]);
        assert_eq!(state.tools()[0].stdout_preview.as_deref(), Some("first\n"));
        state.apply_progress(&[progress(Some("second\n"))]);
        assert_eq!(state.tools()[0].stdout_preview.as_deref(), Some("second\n"));
        assert!(state.tools()[0].live);
        state.agent(&AgentEvent::ToolFinished {
            call,
            content: command_result(0.into(), false, false),
            changes: vec![],
        });
        state.apply_progress(&[progress(Some("stale"))]);
        assert!(!state.tools()[0].live);
        assert_eq!(state.tools()[0].stdout_preview.as_deref(), Some("second\n"));
        assert_eq!(state.tools()[0].command_result().unwrap().stdout, "out\n");
    }

    #[test]
    fn background_and_reused_call_ids_cannot_inherit_a_live_foreground_tail() {
        let mut state = Execution::default();
        let old = ToolCall::function(
            "same-id",
            "run_command",
            serde_json::json!({"command":"first"}),
        );
        state.agent(&AgentEvent::ToolStarted { call: old.clone() });
        state.apply_progress(&[orbit_protocol::ToolProgress {
            call_id: "same-id".into(),
            stdout: Some("old tail".into()),
            elapsed_ms: Some(50),
        }]);
        let generation = state.tools()[0].generation;
        let background = ToolCall::function(
            "same-id",
            "run_command",
            serde_json::json!({"command":"next","background":true}),
        );
        state.agent(&AgentEvent::ToolStarted {
            call: background.clone(),
        });
        assert_ne!(state.tools()[0].generation, generation);
        assert!(state.tools()[0].stdout_preview.is_none());
        state.apply_progress(&[orbit_protocol::ToolProgress {
            call_id: "same-id".into(),
            stdout: Some("background".into()),
            elapsed_ms: Some(60),
        }]);
        assert!(!state.tools()[0].live);
        state.agent(&AgentEvent::ToolFinished {
            call: old,
            content: command_result(0.into(), false, false),
            changes: vec![],
        });
        assert!(state.tools()[0].output.is_none());
        let mut restored = Execution::default();
        restored.agent(&AgentEvent::ToolStarted { call: background });
        assert_ne!(restored.tools()[0].generation, state.tools()[0].generation);
    }
}
