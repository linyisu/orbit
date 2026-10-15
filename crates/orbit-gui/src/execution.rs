//! A read-only projection of durable agent events. The service owns execution;
//! views consume this projection without inferring success from a text message.
use orbit_client::protocol::Update;
use orbit_client::protocol::events::AgentEvent;
use orbit_client::protocol::model::{StreamEvent, ToolCall};
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
    /// A read-only description of the requested operation. Execution status is
    /// deliberately absent: only the service receipt can establish its result.
    pub(crate) fn presentation(&self) -> ToolPresentation {
        let args = serde_json::from_str::<serde_json::Value>(&self.arguments).ok();
        let args = args.as_ref();
        let path = argument(args, &["path"]);
        let (label, target, directory, file) = match self.name.as_str() {
            "run_command" => (
                "命令",
                argument(args, &["command", "cmd"]),
                argument(args, &["cwd"]),
                false,
            ),
            "read_file" => ("读取文件", path, None, true),
            "write_file" => ("写入文件", path, None, true),
            "edit_file" => ("编辑文件", path, None, true),
            "read_image" => ("查看图片", path, None, true),
            "read_document" => ("读取文档", path, None, true),
            "list_files" => ("浏览目录", path, None, true),
            "project_overview" => ("项目概览", argument(args, &["query", "path"]), None, false),
            "search_text" => (
                "搜索代码",
                argument(args, &["query", "pattern"]),
                path,
                false,
            ),
            "web_search" => ("搜索网页", argument(args, &["query"]), None, false),
            "web_fetch" => ("读取网页", argument(args, &["url"]), None, false),
            "spawn_agent" => (
                "启动 Agent",
                argument(args, &["task", "prompt"]),
                None,
                false,
            ),
            "send_agent_message" => (
                "发送给 Agent",
                argument(args, &["message", "prompt", "agent_id"]),
                argument(args, &["agent_id"]),
                false,
            ),
            "read_agent" => ("查看 Agent", argument(args, &["agent_id"]), None, false),
            "wait_agents" => ("等待 Agent", None, None, false),
            "cancel_agents" => ("停止 Agent", None, None, false),
            _ => (self.name.as_str(), None, None, false),
        };
        let target = target.unwrap_or_default();
        let fallback = argument(
            args,
            &[
                "path", "command", "cmd", "query", "pattern", "task", "prompt", "agent_id",
            ],
        )
        .unwrap_or(&self.detail);
        let (title, directory) = if file && !target.is_empty() {
            path_parts(target)
        } else {
            (target, directory.unwrap_or_default())
        };
        let title = compact_line(if title.is_empty() { label } else { title });
        let subtitle = if directory.is_empty() {
            if label == self.name && target.is_empty() {
                compact_line(fallback)
            } else {
                label.to_owned()
            }
        } else {
            format!("{label} · {}", compact_line(directory))
        };
        let mut tooltip = if label == self.name {
            self.name.clone()
        } else {
            format!("{label} · {}", self.name)
        };
        if !target.is_empty() {
            tooltip.push('\n');
            tooltip.push_str(target);
        } else if !fallback.is_empty() {
            tooltip.push('\n');
            tooltip.push_str(fallback);
        }
        if !file && !directory.is_empty() && directory != target {
            tooltip.push('\n');
            tooltip.push_str(directory);
        }
        ToolPresentation {
            title,
            subtitle,
            tooltip,
        }
    }

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

/// Compact row copy with the complete requested command or path available on
/// hover. It contains no independently retained or inferred execution state.
pub(crate) struct ToolPresentation {
    title: String,
    subtitle: String,
    tooltip: String,
}

impl ToolPresentation {
    pub(crate) fn title(&self) -> &str {
        &self.title
    }

    pub(crate) fn subtitle(&self) -> &str {
        &self.subtitle
    }

    pub(crate) fn tooltip(&self) -> &str {
        &self.tooltip
    }
}

fn argument<'a>(args: Option<&'a serde_json::Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        args?.get(key)?.as_str().filter(|text| {
            text.chars()
                .any(|character| !character.is_whitespace() && !character.is_control())
        })
    })
}

fn compact_line(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| {
            line.chars()
                .any(|character| !character.is_whitespace() && !character.is_control())
        })
        .unwrap_or_default();
    let mut characters = line
        .split_whitespace()
        .flat_map(|word| std::iter::once(' ').chain(word.chars()))
        .skip(1)
        .filter(|character| !character.is_control());
    let mut compact: String = characters.by_ref().take(160).collect();
    if characters.next().is_some() {
        compact.push('…');
    }
    compact
}

// Tool paths belong to the service's platform, which can differ from the GUI's
// platform. Preserve their spelling instead of resolving them on this host.
fn path_parts(path: &str) -> (&str, &str) {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() || (trimmed.ends_with(':') && trimmed.len() == 2) {
        return (path, "");
    }
    let Some(separator) = trimmed.rfind(['/', '\\']) else {
        return (trimmed, "");
    };
    let parent_end = if separator == 0 || (separator == 2 && trimmed.as_bytes()[1] == b':') {
        separator + 1
    } else {
        separator
    };
    (&trimmed[separator + 1..], &trimmed[..parent_end])
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
    task: Option<String>,
    authoritative_run: Option<i64>,
}

impl ChildAgent {
    /// The actual task supplied when this child was spawned. Result text and
    /// follow-up messages never replace this identity with a guessed title.
    pub(crate) fn task(&self) -> Option<&str> {
        self.task.as_deref()
    }

    pub(crate) fn task_summary(&self) -> Option<String> {
        self.task.as_deref().map(compact_line)
    }
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

    pub(crate) fn sync_agents(&mut self, agents: &[orbit_protocol::subagents::Agent]) {
        for current in agents {
            if let Some(agent) = self
                .agents
                .iter_mut()
                .find(|agent| agent.id == current.id)
            {
                if agent
                    .authoritative_run
                    .is_some_and(|run| run > current.run)
                {
                    continue;
                }
                agent.status = child_status(&current.status).into();
                agent.authoritative_run = Some(current.run);
                if !current.task.trim().is_empty() {
                    agent.task = Some(current.task.clone());
                }
            } else {
                self.agents.push(ChildAgent {
                    id: current.id.clone(),
                    role: child_role(&current.role).into(),
                    status: child_status(&current.status).into(),
                    task: (!current.task.trim().is_empty()).then(|| current.task.clone()),
                    authoritative_run: Some(current.run),
                });
            }
        }
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
                self.tools
                    .retain(|tool| tool.id != call.id);
                self.tool(call, ToolStatus::Running);
            }
            AgentEvent::ToolFinished {
                call,
                changes,
                content,
                ..
            } => {
                if !self.accepts(call) {
                    return;
                }
                self.tool(call, ToolStatus::Complete);
                self.output(call, content);
                self.files.extend(
                    changes
                        .iter()
                        .map(|change| change.path.clone()),
                );
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
        if let Some(tool) = self
            .tools
            .iter_mut()
            .find(|tool| tool.id == call.id)
        {
            tool.output = Some(content.to_owned());
            tool.live = false;
            if let Some(result) = tool.command_result() {
                tool.status = result.status();
                tool.elapsed_ms = Some(
                    result
                        .elapsed
                        .as_millis()
                        .min(u64::MAX as u128) as u64,
                );
            }
        }
        if call.function.name == "spawn_agent"
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(content)
            && let Some(id) = value["agent_id"].as_str()
        {
            let task = serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                .ok()
                .and_then(|args| argument(Some(&args), &["task", "prompt"]).map(str::to_owned));
            let status = child_status(
                value["status"]
                    .as_str()
                    .unwrap_or_default(),
            )
            .to_owned();
            if let Some(agent) = self
                .agents
                .iter_mut()
                .find(|agent| agent.id == id)
            {
                // Replayed spawn receipts describe launch time, not the latest
                // stored run. They cannot overwrite the live catalogue.
                if agent.authoritative_run.is_none() {
                    agent.status = status;
                    if task.is_some() {
                        agent.task = task;
                    }
                }
            } else {
                let role = child_role(
                    value["role"]
                        .as_str()
                        .unwrap_or_default(),
                );
                self.agents.push(ChildAgent {
                    id: id.to_owned(),
                    role: role.to_owned(),
                    status,
                    task,
                    authoritative_run: None,
                });
            }
        }
    }

    fn tool(&mut self, call: &ToolCall, status: ToolStatus) {
        if let Some(tool) = self
            .tools
            .iter_mut()
            .find(|tool| tool.id == call.id)
        {
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

fn child_status(status: &str) -> &'static str {
    match status {
        "running" | "queued" => "运行中",
        "completed" => "已完成",
        "failed" => "失败",
        "cancelled" | "interrupted" => "已停止",
        _ => "查看状态",
    }
}

fn child_role(role: &str) -> &'static str {
    match role {
        "review" => "审查",
        "explore" => "探索",
        "develop" => "实现",
        _ => "Agent",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_child_runs_refresh_terminal_states_and_ignore_launch_replay() {
        use orbit_protocol::subagents::Agent;
        let mut state = Execution::default();
        let call = ToolCall::function(
            "spawn",
            "spawn_agent",
            serde_json::json!({
                "task": "检查项目"
            }),
        );
        let running = serde_json::json!({
            "agent_id": "child",
            "role": "review",
            "status": "running"
        })
        .to_string();
        state.agent(&AgentEvent::ToolFinished {
            call: call.clone(),
            content: running.clone(),
            changes: vec![],
            files: None,
        });
        let mut stored = Agent {
            id: "child".into(),
            role: "review".into(),
            model: "fixture".into(),
            task: "检查项目".into(),
            workspace: "/workspace".into(),
            status: "completed".into(),
            run: 10,
        };
        for (status, label) in [
            ("completed", "已完成"),
            ("failed", "失败"),
            ("cancelled", "已停止"),
            ("interrupted", "已停止"),
        ] {
            stored.status = status.into();
            state.sync_agents(&[stored.clone()]);
            state.agent(&AgentEvent::ToolFinished {
                call: call.clone(),
                content: running.clone(),
                changes: vec![],
                files: None,
            });
            assert_eq!(state.agents()[0].status, label);
            assert_eq!(state.agents()[0].task(), Some("检查项目"));
        }
        stored.run = 11;
        stored.status = "running".into();
        state.sync_agents(&[stored.clone()]);
        assert_eq!(state.agents()[0].status, "运行中");
        stored.run = 10;
        stored.status = "completed".into();
        state.sync_agents(&[stored]);
        assert_eq!(state.agents()[0].status, "运行中");
        state.sync_agents(&[]);
        assert_eq!(state.agents()[0].status, "运行中");
    }

    fn update(value: serde_json::Value) -> Update {
        serde_json::from_value(value).unwrap()
    }

    fn presented_tool(name: &str, arguments: serde_json::Value) -> ToolRun {
        let mut execution = Execution::default();
        execution.agent(&AgentEvent::ToolStarted {
            call: ToolCall::function("presentation", name, arguments),
        });
        execution.tools.remove(0)
    }

    #[test]
    fn command_presentation_uses_first_nonempty_line_without_changing_the_request() {
        let command = "\n\t \n  python3  -c \"\nprint('已完成')\n\"";
        let tool = presented_tool(
            "run_command",
            serde_json::json!({
                "command": command,
                "cwd": "/home/用户/项目"
            }),
        );
        let original = tool.arguments.clone();
        let presentation = tool.presentation();
        assert_eq!(presentation.title(), "python3 -c \"");
        assert_eq!(presentation.subtitle(), "命令 · /home/用户/项目");
        assert!(presentation.tooltip().contains(command));
        assert!(
            presentation
                .tooltip()
                .ends_with("/home/用户/项目")
        );
        assert_eq!(tool.arguments, original);
        assert_eq!(tool.status, ToolStatus::Running);
    }

    #[test]
    fn presentation_bounds_long_unicode_titles_but_keeps_the_complete_tooltip() {
        let command = format!("printf '{}'", "数据🦀".repeat(300));
        let tool = presented_tool(
            "run_command",
            serde_json::json!({
                "command": command
            }),
        );
        let presentation = tool.presentation();
        assert_eq!(presentation.title().chars().count(), 161);
        assert!(presentation.title().ends_with('…'));
        assert!(
            presentation
                .tooltip()
                .ends_with(&command)
        );
        assert!(
            !presentation
                .title()
                .contains('\u{fffd}')
        );
    }

    #[test]
    fn file_presentation_preserves_remote_path_spelling_and_root_directories() {
        for (path, title, directory) in [
            ("/home/用户/项目/main.rs", "main.rs", "/home/用户/项目"),
            ("/main.rs", "main.rs", "/"),
            ("src/测试文件.rs", "测试文件.rs", "src"),
            ("C:\\项目\\main.rs", "main.rs", "C:\\项目"),
            ("C:\\main.rs", "main.rs", "C:\\"),
            ("main.rs", "main.rs", ""),
            ("/", "/", ""),
            ("C:\\", "C:\\", ""),
        ] {
            let tool = presented_tool(
                "read_file",
                serde_json::json!({
                    "path": path
                }),
            );
            let presentation = tool.presentation();
            assert_eq!(presentation.title(), title);
            assert_eq!(
                presentation.subtitle(),
                if directory.is_empty() {
                    "读取文件".to_owned()
                } else {
                    format!("读取文件 · {directory}")
                }
            );
            assert!(presentation.tooltip().ends_with(path));
        }
    }

    #[test]
    fn empty_invalid_and_unknown_arguments_leave_an_identifiable_row() {
        for arguments in [
            serde_json::json!({}),
            serde_json::json!({
                "command": " \n\t"
            }),
        ] {
            let tool = presented_tool("run_command", arguments);
            assert_eq!(tool.presentation().title(), "命令");
            assert!(
                tool.presentation()
                    .tooltip()
                    .contains("run_command")
            );
        }
        let mut tool = presented_tool(
            "mcp__catalog__search",
            serde_json::json!({
                "query": "齿轮"
            }),
        );
        assert_eq!(tool.presentation().title(), "mcp__catalog__search");
        assert_eq!(tool.presentation().subtitle(), "齿轮");
        tool.arguments = "not JSON".into();
        assert_eq!(tool.presentation().title(), "mcp__catalog__search");
        assert_eq!(tool.status, ToolStatus::Running);
    }

    #[test]
    fn search_and_delegation_present_the_actual_requested_target() {
        for (name, args, title, label) in [
            (
                "search_text",
                serde_json::json!({
                    "query": "渲染 错误",
                    "path": "src"
                }),
                "渲染 错误",
                "搜索代码 · src",
            ),
            (
                "web_search",
                serde_json::json!({
                    "query": "GPUI tooltip"
                }),
                "GPUI tooltip",
                "搜索网页",
            ),
            (
                "web_fetch",
                serde_json::json!({
                    "url": "https://example.com/文档"
                }),
                "https://example.com/文档",
                "读取网页",
            ),
            (
                "spawn_agent",
                serde_json::json!({
                    "task": "检查布局\n保留用户改动"
                }),
                "检查布局",
                "启动 Agent",
            ),
        ] {
            let presentation = presented_tool(name, args).presentation();
            assert_eq!(presentation.title(), title);
            assert_eq!(presentation.subtitle(), label);
            assert!(presentation.tooltip().contains(name));
        }
    }

    #[test]
    fn projection_tracks_real_tools_compaction_and_committed_files() {
        let mut state = Execution::default();
        let call = serde_json::json!({
            "id": "write-1",
            "function": {
                "name": "write_file",
                "arguments": "{\"path\":\"src/main.rs\"}"
            }
        });
        state.apply(&update(serde_json::json!({
            "Started": {
                "prompt": "fix"
            }
        })));
        state.apply(&update(serde_json::json!({
            "Agent": {
                "CompactionStarted": null
            }
        })));
        assert_eq!(state.phase(), Phase::Compacting);
        state.apply(&update(serde_json::json!({
            "Agent": {
                "ToolStarted": {
                    "call": call
                }
            }
        })));
        assert!(state.files().is_empty());
        state.apply(&update(serde_json::json!({
            "Agent": {
                "ToolFinished": {
                    "call": call,
                    "content": "ok",
                    "changes": [{
                        "path": "src/main.rs",
                        "before": null,
                        "after": "fn main() {}"
                    }]
                }
            }
        })));
        assert_eq!(state.tools().len(), 1);
        assert_eq!(state.tools()[0].status, ToolStatus::Complete);
        assert!(state.files().contains("src/main.rs"));
        state.apply(&update(serde_json::json!({
            "Finished": {
                "status": "completed"
            }
        })));
        assert_eq!(state.phase(), Phase::Complete);
        state.apply(&update(serde_json::json!({
            "Started": {
                "prompt": "next"
            }
        })));
        assert!(state.tools().is_empty());
        assert_eq!(state.files().len(), 1);
    }

    #[test]
    fn failed_run_never_leaves_a_tool_marked_running() {
        let mut state = Execution::default();
        state.apply(&update(serde_json::json!({
            "Agent": {
                "ToolStarted": {
                    "call": {
                        "id": "one",
                        "function": {
                            "name": "run_command",
                            "arguments": "{}"
                        }
                    }
                }
            }
        })));
        state.apply(&update(serde_json::json!({
            "Finished": {
                "status": "failed"
            }
        })));
        assert_eq!(state.tools()[0].status, ToolStatus::Failed);
        assert_eq!(state.phase(), Phase::Failed);
    }

    #[test]
    fn delegated_work_stays_pending_until_its_own_completion_event() {
        let mut state = Execution::default();
        let call = serde_json::json!({
            "id": "delegate",
            "function": {
                "name": "spawn_agent",
                "arguments": "{\"task\":\"review\"}"
            }
        });
        let running = serde_json::json!({
            "agent_id": "review-1",
            "role": "review",
            "status": "running"
        })
        .to_string();
        state.apply(&update(serde_json::json!({
            "Agent": {
                "ToolBackgrounded": {
                    "call": call,
                    "job_id": "job-1",
                    "content": running
                }
            }
        })));
        state.apply(&update(serde_json::json!({
            "Finished": {
                "status": "completed"
            }
        })));
        assert_eq!(state.tools()[0].status, ToolStatus::Background);
        assert_eq!(state.agents()[0].status, "运行中");
        let completed = serde_json::json!({
            "agent_id": "review-1",
            "role": "review",
            "status": "completed"
        })
        .to_string();
        state.apply(&update(serde_json::json!({
            "Agent": {
                "ToolFinished": {
                    "call": call,
                    "content": completed,
                    "changes": []
                }
            }
        })));
        assert_eq!(state.tools().len(), 1);
        assert_eq!(state.tools()[0].status, ToolStatus::Complete);
        assert_eq!(state.agents().len(), 1);
        assert_eq!(state.agents()[0].status, "已完成");
    }

    #[test]
    fn child_task_identity_survives_completion_and_follow_up_tool_receipts() {
        let task = "\n检查  侧栏布局\n保留当前项目与用户草稿";
        let mut state = Execution::default();
        let call = ToolCall::function(
            "spawn",
            "spawn_agent",
            serde_json::json!({
                "task": task
            }),
        );
        let receipt = |status: &str| {
            serde_json::json!({
                "agent_id": "child",
                "role": "review",
                "status": status
            })
            .to_string()
        };
        state.agent(&AgentEvent::ToolBackgrounded {
            call: call.clone(),
            job_id: "job".into(),
            content: receipt("running"),
        });
        assert_eq!(state.agents()[0].task(), Some(task));
        assert_eq!(
            state.agents()[0]
                .task_summary()
                .as_deref(),
            Some("检查 侧栏布局")
        );
        state.agent(&AgentEvent::ToolFinished {
            call,
            content: receipt("completed"),
            changes: vec![],
            files: None,
        });
        for name in ["read_agent", "send_agent_message", "wait_agents"] {
            state.agent(&AgentEvent::ToolFinished {
                call: ToolCall::function(
                    name,
                    name,
                    serde_json::json!({
                        "agent_id": "child",
                        "message": "补充说明"
                    }),
                ),
                content: receipt("running"),
                changes: vec![],
                files: None,
            });
            assert_eq!(state.agents()[0].task(), Some(task));
        }
        // Partial historical receipts must not erase an already known task.
        state.agent(&AgentEvent::ToolFinished {
            call: ToolCall::function("historical", "spawn_agent", serde_json::json!({})),
            content: receipt("completed"),
            changes: vec![],
            files: None,
        });
        assert_eq!(state.agents().len(), 1);
        assert_eq!(state.agents()[0].task(), Some(task));
        assert_eq!(state.agents()[0].status, "已完成");
    }

    #[test]
    fn child_tasks_accept_legacy_prompts_but_never_invent_missing_tasks() {
        for (arguments, task) in [
            (
                serde_json::json!({
                    "prompt": "审查改动"
                }),
                Some("审查改动"),
            ),
            (
                serde_json::json!({
                    "task": " \n\t"
                }),
                None,
            ),
            (serde_json::json!({}), None),
        ] {
            let mut state = Execution::default();
            state.agent(&AgentEvent::ToolFinished {
                call: ToolCall::function("spawn", "spawn_agent", arguments),
                content: serde_json::json!({
                    "agent_id": "child",
                    "role": "review",
                    "status": "completed",
                    "task": "结果中的其他文本"
                })
                .to_string(),
                changes: vec![],
                files: None,
            });
            assert_eq!(state.agents()[0].task(), task);
            assert_eq!(
                state.agents()[0]
                    .task_summary()
                    .as_deref(),
                task
            );
        }
    }

    fn command_result(exit_code: serde_json::Value, timed_out: bool, cancelled: bool) -> String {
        serde_json::json!({
            "exit_code": exit_code,
            "elapsed": {
                "secs": 1,
                "nanos": 234_000_000
            },
            "stdout": "out\n",
            "stderr": "err\n",
            "timed_out": timed_out,
            "cancelled": cancelled,
            "output_truncated": false,
            "stdout_total_bytes": 4,
            "stderr_total_bytes": 4,
            "retained_output_truncated": false
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
                serde_json::json!({
                    "command": "example"
                }),
            );
            state.agent(&AgentEvent::ToolStarted { call: call.clone() });
            state.agent(&AgentEvent::ToolFinished {
                call,
                content: command_result(exit, timed_out, cancelled),
                changes: vec![],
                files: None,
            });
            assert_eq!(state.tools()[0].status, expected);
            assert_eq!(state.tools()[0].elapsed_ms, Some(1_234));
        }
    }

    #[test]
    fn stdout_progress_replaces_tail_preserves_omitted_data_and_never_revives_completion() {
        let mut state = Execution::default();
        let call = ToolCall::function(
            "command",
            "run_command",
            serde_json::json!({
                "command": "example"
            }),
        );
        state.agent(&AgentEvent::ToolStarted { call: call.clone() });
        let progress = |stdout: Option<&str>| orbit_protocol::ToolProgress {
            call_id: "command".into(),
            stdout: stdout.map(str::to_owned),
            elapsed_ms: Some(50),
        };
        state.apply_progress(&[progress(Some("first\n"))]);
        state.apply_progress(&[progress(None)]);
        assert_eq!(
            state.tools()[0]
                .stdout_preview
                .as_deref(),
            Some("first\n")
        );
        state.apply_progress(&[progress(Some("second\n"))]);
        assert_eq!(
            state.tools()[0]
                .stdout_preview
                .as_deref(),
            Some("second\n")
        );
        assert!(state.tools()[0].live);
        state.agent(&AgentEvent::ToolFinished {
            call,
            content: command_result(0.into(), false, false),
            changes: vec![],
            files: None,
        });
        state.apply_progress(&[progress(Some("stale"))]);
        assert!(!state.tools()[0].live);
        assert_eq!(
            state.tools()[0]
                .stdout_preview
                .as_deref(),
            Some("second\n")
        );
        assert_eq!(
            state.tools()[0]
                .command_result()
                .unwrap()
                .stdout,
            "out\n"
        );
    }

    #[test]
    fn background_and_reused_call_ids_cannot_inherit_a_live_foreground_tail() {
        let mut state = Execution::default();
        let old = ToolCall::function(
            "same-id",
            "run_command",
            serde_json::json!({
                "command": "first"
            }),
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
            serde_json::json!({
                "command": "next",
                "background": true
            }),
        );
        state.agent(&AgentEvent::ToolStarted {
            call: background.clone(),
        });
        assert_ne!(state.tools()[0].generation, generation);
        assert!(
            state.tools()[0]
                .stdout_preview
                .is_none()
        );
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
            files: None,
        });
        assert!(state.tools()[0].output.is_none());
        let mut restored = Execution::default();
        restored.agent(&AgentEvent::ToolStarted { call: background });
        assert_ne!(restored.tools()[0].generation, state.tools()[0].generation);
    }
}
