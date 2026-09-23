use std::time::Instant;

use orbit_agent::events::AgentEvent;
use orbit_model::types::{Attachment, StreamEvent, ToolCall};

use super::input::InputState;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunStatus {
    Idle,
    Running,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Finished,
    Failed,
}

#[derive(Clone, Debug)]
pub struct ToolRow {
    pub id: String,
    pub name: String,
    pub arguments: String,
    pub status: ToolStatus,
    pub output: String,
}

#[derive(Clone, Debug)]
pub enum TranscriptItem {
    User(String),
    Command(String),
    Assistant(String),
    Thinking(String),
    Tool(ToolRow),
    Error(String),
    Notice(String),
    /// Low-priority lifecycle information rendered as an unobtrusive divider
    /// instead of pretending to be an assistant or command response.
    Maintenance(String),
    CommandResult(String),
    CommandError(String),
}

#[derive(Debug)]
pub struct UiState {
    pub(super) navigation: super::navigation::Navigation,
    pub approval: Option<orbit_tools::approval::ApprovalPreview>,
    pub approval_area: ratatui::layout::Rect,
    pub approval_scroll: usize,
    pub approval_expanded: bool,
    pub approval_max_scroll: usize,
    pub(super) approval_diff: Option<super::approval_diff::Cache>,
    pub approval_shown: bool,
    pub approval_choice: usize,
    pub permission_mode: super::permissions::Mode,
    pub selection: super::selection::Selection,
    /// Request budget, including output capacity where the provider counts it.
    pub context_tokens: usize,
    /// Input-only window used for the context indicator and compaction budget.
    pub input_context_tokens: usize,
    /// Same approximate request-context count displayed by `/context`.
    pub context_used_tokens: usize,
    pub calibrations:
        std::collections::BTreeMap<String, orbit_agent::calibration::TokenCalibration>,
    /// Footer area that exposes the current context budget on hover.
    pub context_indicator_area: ratatui::layout::Rect,
    pub context_hovered: bool,
    pub context_details_visible: bool,
    /// Pointer position while it rests on a clickable transcript link.
    pub link_hovered: Option<(u16, u16)>,
    /// Last drawn link hint; it must be cleared before the transcript redraws.
    pub link_tooltip_area: ratatui::layout::Rect,
    pub auto_compact: bool,
    pub max_output_tokens: u32,
    pub model: String,
    pub reasoning_effort: Option<orbit_model::types::ReasoningEffort>,
    pub supports_vision: bool,
    pub attachments: Vec<Attachment>,
    pub attachment_cursor: Option<usize>,
    pub attachment_areas: Vec<ratatui::layout::Rect>,
    pub workspace: String,
    pub input_area: ratatui::layout::Rect,
    pub input_scroll: usize,
    pub input_width: usize,
    pub ime_cursor: Option<ratatui::layout::Position>,
    pub menu_index: usize,
    pub menu_dismissed: bool,
    pub show_help: bool,
    pub help_keys: bool,
    pub help_scroll: usize,
    pub picker: Option<super::dialogs::SessionPicker>,
    pub resume_request: Option<String>,
    pub compact_after_model_switch: bool,
    pub resume_loading: Option<String>,
    pub model_picker: Option<super::dialogs::ModelPicker>,
    pub marketplace_dialog: Option<super::dialogs::MarketplaceDialog>,
    pub pending_marketplace_source: Option<String>,
    pub pending_project_plugin_change: Option<super::dialogs::ProjectPluginChange>,
    pub chosen_model: Option<String>,
    pub chosen_effort: Option<orbit_model::types::ReasoningEffort>,
    pub model_completions: Vec<String>,
    pub model_completion_index: usize,
    pub expanded: Option<usize>,
    pub max_scroll: usize,
    pub(super) transcript_layout: super::transcript::TranscriptLayout,
    pub elapsed: std::time::Duration,
    pub input: InputState,
    pub transcript: Vec<TranscriptItem>,
    pub status: RunStatus,
    pub turn: usize,
    pub tool_calls: usize,
    pub scroll: usize,
    pub follow_bottom: bool,
    pub started_at: Option<Instant>,
    paused_at: Option<Instant>,
    paused_elapsed: std::time::Duration,
}

impl UiState {
    pub fn invalidate_transcript(&mut self, first: usize) {
        self.transcript_layout.invalidate_from(first);
    }

    pub fn calibration(&self) -> orbit_agent::calibration::TokenCalibration {
        self.calibrations
            .get(&self.model)
            .cloned()
            .unwrap_or_default()
    }

    pub fn new(model: impl Into<String>) -> Self {
        Self {
            navigation: Default::default(),
            approval: None,
            approval_area: ratatui::layout::Rect::default(),
            approval_scroll: 0,
            approval_expanded: false,
            approval_max_scroll: 0,
            approval_diff: None,
            approval_shown: false,
            approval_choice: 0,
            permission_mode: super::permissions::Mode::default(),
            selection: super::selection::Selection::default(),
            context_tokens: 128_000,
            input_context_tokens: 96_000,
            context_used_tokens: 0,
            calibrations: Default::default(),
            context_indicator_area: ratatui::layout::Rect::default(),
            context_hovered: false,
            context_details_visible: false,
            link_hovered: None,
            link_tooltip_area: ratatui::layout::Rect::default(),
            auto_compact: true,
            max_output_tokens: 32_000,
            model: model.into(),
            reasoning_effort: None,
            supports_vision: false,
            attachments: Vec::new(),
            attachment_cursor: None,
            attachment_areas: Vec::new(),
            workspace: String::new(),
            input_area: Default::default(),
            input_scroll: 0,
            input_width: 74,
            ime_cursor: None,
            menu_index: 0,
            menu_dismissed: false,
            show_help: false,
            help_keys: false,
            help_scroll: 0,
            picker: None,
            resume_request: None,
            compact_after_model_switch: false,
            resume_loading: None,
            model_picker: None,
            marketplace_dialog: None,
            pending_marketplace_source: None,
            pending_project_plugin_change: None,
            chosen_model: None,
            chosen_effort: None,
            model_completions: Vec::new(),
            model_completion_index: 0,
            expanded: None,
            max_scroll: 0,
            transcript_layout: Default::default(),
            elapsed: std::time::Duration::ZERO,
            input: InputState::default(),
            transcript: Vec::new(),
            status: RunStatus::Idle,
            turn: 0,
            tool_calls: 0,
            scroll: 0,
            follow_bottom: true,
            started_at: None,
            paused_at: None,
            paused_elapsed: std::time::Duration::ZERO,
        }
    }

    pub fn submit(&mut self, prompt: String) {
        self.transcript.push(TranscriptItem::User(prompt));
        self.begin_run();
    }

    pub fn begin_run(&mut self) {
        self.status = RunStatus::Running;
        self.show_help = false;
        self.elapsed = std::time::Duration::ZERO;
        self.scroll = 0;
        self.turn = 0;
        self.tool_calls = 0;
        self.started_at = Some(Instant::now());
        self.paused_at = None;
        self.paused_elapsed = std::time::Duration::ZERO;
        self.follow_bottom = true;
    }

    pub fn fail(&mut self, error: impl Into<String>) {
        self.status = RunStatus::Failed;
        self.finish_clock();
        self.transcript.push(TranscriptItem::Error(error.into()));
    }

    pub fn cancel(&mut self) {
        if self.status == RunStatus::Running {
            self.status = RunStatus::Cancelling;
        }
    }

    pub fn apply(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStarted { turn } => {
                if self.status != RunStatus::Cancelling {
                    self.status = RunStatus::Running;
                }
                self.turn = turn;
                self.ensure_assistant_item();
            }
            AgentEvent::RequestMetrics {
                profile,
                estimated_input,
                usage,
                ..
            } => {
                self.calibrations.entry(profile).or_default().observe(
                    estimated_input,
                    usage.as_ref().and_then(|u| u.prompt_tokens),
                );
            }
            AgentEvent::CompactionRecorded { .. } | AgentEvent::VerificationRecorded { .. } => {}
            // Compaction is an implementation detail while it runs. Showing a
            // second transcript row for it made every automatic checkpoint
            // look like a noisy command result.
            AgentEvent::CompactionStarted => {}
            AgentEvent::ContextCompacted {
                before_tokens,
                after_tokens,
                strategy,
                fallback,
                ..
            } => {
                self.context_used_tokens = self.calibration().estimate(after_tokens);
                let detail = if fallback
                    && strategy != orbit_agent::memory::CompactionStrategy::ArchiveProjection
                {
                    " · 保留最近工作内容"
                } else {
                    ""
                };
                let action = match strategy {
                    orbit_agent::memory::CompactionStrategy::ToolPruning => "旧工具输出已精简",
                    orbit_agent::memory::CompactionStrategy::Summary => "上下文已整理",
                    orbit_agent::memory::CompactionStrategy::ArchiveProjection => {
                        "旧历史已归档，摘要未完成"
                    }
                };
                self.transcript.push(TranscriptItem::Maintenance(format!(
                    "{action}  {before_tokens} → {after_tokens} tokens{detail}"
                )));
            }
            AgentEvent::CompactionFailed { kind, .. } => {
                self.transcript
                    .push(TranscriptItem::Maintenance(match kind {
                        Some(kind) => {
                            format!("上下文整理未完成：{}。原上下文已保留。", kind.label())
                        }
                        None => "上下文整理未完成，原上下文已保留".into(),
                    }))
            }
            AgentEvent::Model(event) => self.apply_model_event(event),
            AgentEvent::ToolStarted { call } => {
                self.tool_calls += 1;
                self.transcript.push(TranscriptItem::Tool(ToolRow {
                    id: call.id,
                    name: call.function.name,
                    arguments: call.function.arguments,
                    status: ToolStatus::Running,
                    output: String::new(),
                }));
            }
            AgentEvent::ToolFinished { call, content, .. } => {
                self.update_tool(&call, ToolStatus::Finished, content);
            }
            AgentEvent::ToolFailed { call, error } => {
                self.update_tool(&call, ToolStatus::Failed, error);
            }
            AgentEvent::Completed { .. } => {
                self.status = RunStatus::Completed;
                self.finish_clock();
            }
            AgentEvent::Failed { error } => {
                self.status = RunStatus::Failed;
                self.finish_clock();
                self.transcript.push(TranscriptItem::Error(error));
            }
            AgentEvent::Cancelled => {
                self.status = RunStatus::Cancelled;
                self.finish_clock();
            }
        }
    }

    pub fn scroll_up(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_add(amount).min(self.max_scroll);
        self.follow_bottom = false;
    }

    pub fn scroll_down(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_sub(amount);
        if self.scroll == 0 {
            self.follow_bottom = true;
        }
    }

    pub fn running(&self) -> bool {
        matches!(self.status, RunStatus::Running | RunStatus::Cancelling)
    }

    pub fn menu(&self) -> Vec<&'static super::commands::Command> {
        if self.menu_dismissed || self.resume_loading.is_some() {
            Vec::new()
        } else {
            super::commands::matching(&self.input.text)
                .into_iter()
                .filter(|command| {
                    !self.running() || super::commands::available_while_running(command.name)
                })
                .collect()
        }
    }

    pub fn active_elapsed(&self) -> std::time::Duration {
        let Some(start) = self.started_at else {
            return self.elapsed;
        };
        self.paused_at
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(start)
            .saturating_sub(self.paused_elapsed)
    }

    pub fn pause_clock(&mut self) {
        if self.started_at.is_some() && self.paused_at.is_none() {
            self.paused_at = Some(Instant::now());
        }
    }

    pub fn resume_clock(&mut self) {
        if let Some(paused_at) = self.paused_at.take() {
            self.paused_elapsed += paused_at.elapsed();
        }
    }

    pub fn finish_clock(&mut self) {
        self.elapsed = self.active_elapsed();
        self.started_at = None;
        self.paused_at = None;
        self.paused_elapsed = std::time::Duration::ZERO;
    }

    fn apply_model_event(&mut self, event: StreamEvent) {
        self.transcript_layout
            .invalidate_from(self.transcript.len().saturating_sub(1));
        match event {
            StreamEvent::TextDelta(text) => {
                self.ensure_assistant_item();
                if let Some(TranscriptItem::Assistant(content)) = self.transcript.last_mut() {
                    content.push_str(&text);
                }
            }
            StreamEvent::ReasoningDelta(text) => match self.transcript.last_mut() {
                Some(TranscriptItem::Thinking(content)) => content.push_str(&text),
                _ => self.transcript.push(TranscriptItem::Thinking(text)),
            },
            StreamEvent::ToolCallDelta(_) | StreamEvent::Completed { .. } => {}
        }
    }

    fn ensure_assistant_item(&mut self) {
        if !matches!(self.transcript.last(), Some(TranscriptItem::Assistant(_))) {
            self.transcript
                .push(TranscriptItem::Assistant(String::new()));
        }
    }

    fn update_tool(&mut self, call: &ToolCall, status: ToolStatus, output: String) {
        if let Some((index, TranscriptItem::Tool(row))) = self
            .transcript
            .iter_mut()
            .enumerate()
            .rev()
            .find(|(_, item)| matches!(item, TranscriptItem::Tool(row) if row.id == call.id))
        {
            self.transcript_layout.invalidate_from(index);
            row.status = status;
            row.output = output;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_model::types::FunctionCall;

    #[test]
    fn approval_wait_is_excluded_from_response_clock() {
        let mut state = UiState::new("mock");
        state.begin_run();
        state.started_at = Some(Instant::now() - std::time::Duration::from_secs(12));
        state.pause_clock();
        state.paused_at = Some(Instant::now() - std::time::Duration::from_secs(8));
        let active = state.active_elapsed();
        assert!(active >= std::time::Duration::from_secs(3));
        assert!(active < std::time::Duration::from_secs(5));
        state.resume_clock();
        state.finish_clock();
        assert!(state.elapsed >= std::time::Duration::from_secs(3));
        assert!(state.elapsed < std::time::Duration::from_secs(5));
    }

    fn call(id: &str) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            kind: "function".to_owned(),
            function: FunctionCall {
                name: "echo".to_owned(),
                arguments: "{}".to_owned(),
            },
        }
    }

    #[test]
    fn reducer_tracks_stream_and_tool_lifecycle() {
        let mut state = UiState::new("mock");
        state.submit("hello".to_owned());
        state.apply(AgentEvent::TurnStarted { turn: 1 });
        state.apply(AgentEvent::Model(StreamEvent::TextDelta("hi".to_owned())));
        state.apply(AgentEvent::ToolStarted { call: call("one") });
        state.apply(AgentEvent::ToolFinished {
            call: call("one"),
            content: "done".to_owned(),
            changes: Vec::new(),
        });
        state.apply(AgentEvent::Completed {
            turns: 1,
            tool_calls: 1,
        });

        assert_eq!(state.status, RunStatus::Completed);
        assert!(matches!(
            &state.transcript[1],
            TranscriptItem::Assistant(content) if content == "hi"
        ));
        assert!(matches!(
            &state.transcript[2],
            TranscriptItem::Tool(row) if row.status == ToolStatus::Finished && row.output == "done"
        ));
    }

    #[test]
    fn running_menu_keeps_only_safe_local_commands() {
        let mut state = UiState::new("mock");
        state.status = RunStatus::Running;
        state.input.insert("/");
        let commands = state.menu();
        assert!(commands.iter().any(|command| command.name == "/model"));
        assert!(commands.iter().any(|command| command.name == "/status"));
        assert!(!commands.iter().any(|command| command.name == "/new"));
        assert!(!commands.iter().any(|command| command.name == "/compact"));
    }

    #[test]
    fn compaction_uses_one_quiet_maintenance_row() {
        let mut state = UiState::new("mock");
        state.apply(AgentEvent::CompactionStarted);
        assert!(state.transcript.is_empty());
        state.apply(AgentEvent::ContextCompacted {
            strategy: Default::default(),
            before_tokens: 195_754,
            after_tokens: 96_000,
            fallback: true,
            fallback_reason: Some("摘要请求超时".into()),
        });
        assert_eq!(state.context_used_tokens, 96_000);
        assert!(matches!(
            state.transcript.as_slice(),
            [TranscriptItem::Maintenance(text)]
                if text.contains("195754 → 96000") && text.contains("保留最近工作内容")
        ));
    }
}
