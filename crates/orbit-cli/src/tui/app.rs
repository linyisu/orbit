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

#[derive(Clone, Debug)]
pub struct SessionTab {
    pub waiting_approval: bool,
    pub elapsed: std::time::Duration,
    pub id: String,
    pub title: String,
    pub status: RunStatus,
    pub active: bool,
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

/// A reasoning segment has its own lifetime, independent of the whole run.
#[derive(Clone, Debug)]
pub struct ThinkingRow {
    pub content: String,
    pub finished: bool,
    pub elapsed: Option<std::time::Duration>,
    started_at: Option<Instant>,
}

impl From<String> for ThinkingRow {
    fn from(content: String) -> Self {
        Self {
            content,
            finished: false,
            elapsed: None,
            started_at: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum TranscriptItem {
    User(String),
    Assistant(String),
    Thinking(ThinkingRow),
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
    pub tabs: Vec<SessionTab>,
    pub new_tab_area: ratatui::layout::Rect,
    pub new_tab_request: bool,
    pub close_tab_request: Option<String>,
    pub tab_close_areas: Vec<(ratatui::layout::Rect, String)>,
    pub(super) tab_hover: Option<(super::TabAction, Instant)>,
    pub(super) tab_hover_tick: u128,
    pub tab_areas: Vec<(ratatui::layout::Rect, String)>,
    pub(super) navigation: super::navigation::Navigation,
    pub approval: Option<orbit_tools::approval::ApprovalPreview>,
    pub approval_area: ratatui::layout::Rect,
    pub approval_scroll: usize,
    pub approval_expanded: bool,
    pub approval_max_scroll: usize,
    pub(super) approval_diff: Option<super::approval_diff::Cache>,
    pub approval_shown: bool,
    pub approval_choice: usize,
    pub session_allowed_files: std::collections::HashSet<String>,
    pub session_commands: std::collections::HashSet<String>,
    pub session_git_commands: std::collections::HashSet<String>,
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
    pub thinking_hover_started: Option<Instant>,
    pub transcript_pointer: Option<(u16, u16)>,
    active_thinking: Option<usize>,
    model_started_at: Option<Instant>,
    background_waiting: bool,
    /// Last drawn link hint; it must be cleared before the transcript redraws.
    pub link_tooltip_area: ratatui::layout::Rect,
    pub auto_compact: bool,
    /// Archive reference emitted immediately before a context compaction event.
    pending_compaction_archive: Option<String>,
    pub max_output_tokens: u32,
    pub model: String,
    pub reasoning_efforts: Vec<orbit_model::types::ReasoningEffort>,
    pub reasoning_effort: Option<orbit_model::types::ReasoningEffort>,
    pub supports_vision: bool,
    pub attachments: Vec<Attachment>,
    pub attachment_cursor: Option<usize>,
    pub attachment_areas: Vec<ratatui::layout::Rect>,
    pub workspace: String,
    pub branch_workspace: String,
    pub branch: Option<String>,
    pub input_area: ratatui::layout::Rect,
    pub input_scroll: usize,
    pub input_dragging: bool,
    pub input_width: usize,
    pub ime_cursor: Option<ratatui::layout::Position>,
    pub(super) cursor_motion: super::cursor::CursorMotion,
    pub restore_picker: Option<super::restore::RestorePicker>,
    pub restore_selection: Option<Vec<String>>,
    pub stored_completions: Vec<super::completion::Candidate>,
    pub menu_area: ratatui::layout::Rect,
    pub menu_rows: Vec<(ratatui::layout::Rect, usize)>,
    pub dialog_rows: Vec<(ratatui::layout::Rect, usize)>,
    pub approval_options: Vec<(ratatui::layout::Rect, usize)>,
    pub menu_index: usize,
    pub menu_offset: usize,
    pub menu_manual_scroll: bool,
    pub dialog_manual_scroll: bool,
    pub pending_list_hover: Option<crossterm::event::MouseEvent>,
    pub menu_dismissed: bool,
    pub show_help: bool,
    pub help_keys: bool,
    pub help_scroll: usize,
    pub picker: Option<super::dialogs::SessionPicker>,
    pub resume_request: Option<String>,
    pub(super) subagent_parent: Option<String>,
    pub(super) subagent_parent_area: ratatui::layout::Rect,
    pub(super) subagent_parent_hovered: bool,
    pub(super) subagent_parent_pressed: bool,
    pub delete_request: Option<String>,
    pub compact_after_model_switch: bool,
    pub resume_loading: Option<String>,
    pub model_picker: Option<super::dialogs::ModelPicker>,
    pub marketplace_dialog: Option<super::dialogs::MarketplaceDialog>,
    pub pending_marketplace_source: Option<String>,
    pub pending_project_plugin_change: Option<super::dialogs::ProjectPluginChange>,
    pub chosen_model: Option<String>,
    pub chosen_effort: Option<orbit_model::types::ReasoningEffort>,
    pub model_completions: Vec<String>,
    pub completion_home: Option<std::path::PathBuf>,
    pub skill_completions: Vec<(String, String)>,
    pub directory_completions: super::completion::DirectoryCache,
    pub expanded: std::collections::BTreeSet<usize>,
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
    pub(super) fn has_conversation(&self) -> bool {
        self.transcript.iter().any(|item| {
            matches!(
                item,
                TranscriptItem::User(_)
                    | TranscriptItem::Assistant(_)
                    | TranscriptItem::Thinking(_)
                    | TranscriptItem::Tool(_)
            )
        })
    }

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
            tabs: Vec::new(),
            new_tab_area: Default::default(),
            new_tab_request: false,
            close_tab_request: None,
            tab_close_areas: Vec::new(),
            tab_hover: None,
            tab_hover_tick: 0,
            tab_areas: Vec::new(),
            navigation: Default::default(),
            approval: None,
            approval_area: ratatui::layout::Rect::default(),
            approval_scroll: 0,
            approval_expanded: false,
            approval_max_scroll: 0,
            approval_diff: None,
            approval_shown: false,
            approval_choice: 0,
            session_allowed_files: std::collections::HashSet::new(),
            session_commands: Default::default(),
            session_git_commands: Default::default(),
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
            thinking_hover_started: None,
            transcript_pointer: None,
            active_thinking: None,
            model_started_at: None,
            background_waiting: false,
            link_tooltip_area: ratatui::layout::Rect::default(),
            auto_compact: true,
            pending_compaction_archive: None,
            max_output_tokens: 32_000,
            model: model.into(),
            reasoning_efforts: Vec::new(),
            reasoning_effort: None,
            supports_vision: false,
            attachments: Vec::new(),
            attachment_cursor: None,
            attachment_areas: Vec::new(),
            workspace: String::new(),
            branch_workspace: String::new(),
            branch: None,
            input_area: Default::default(),
            input_scroll: 0,
            input_dragging: false,
            input_width: 74,
            ime_cursor: None,
            cursor_motion: Default::default(),
            restore_picker: None,
            restore_selection: None,
            stored_completions: Vec::new(),
            menu_area: Default::default(),
            menu_rows: Vec::new(),
            dialog_rows: Vec::new(),
            approval_options: Vec::new(),
            menu_index: 0,
            menu_offset: 0,
            menu_manual_scroll: false,
            dialog_manual_scroll: false,
            pending_list_hover: None,
            menu_dismissed: false,
            show_help: false,
            help_keys: false,
            help_scroll: 0,
            picker: None,
            resume_request: None,
            subagent_parent: None,
            subagent_parent_area: Default::default(),
            subagent_parent_hovered: false,
            subagent_parent_pressed: false,
            delete_request: None,
            compact_after_model_switch: false,
            resume_loading: None,
            model_picker: None,
            marketplace_dialog: None,
            pending_marketplace_source: None,
            pending_project_plugin_change: None,
            chosen_model: None,
            chosen_effort: None,
            model_completions: Vec::new(),
            completion_home: None,
            skill_completions: Vec::new(),
            directory_completions: Default::default(),
            expanded: Default::default(),
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

    pub fn restore_editor_draft(&mut self, text: String) {
        self.input.set(text);
        self.selection.clear();
        self.menu_index = 0;
        self.menu_dismissed = true;
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

    pub fn cancel(&mut self) {
        if self.status == RunStatus::Running {
            self.status = RunStatus::Cancelling;
        }
    }

    pub fn apply(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStarted { turn } => {
                if self.background_waiting {
                    self.background_waiting = false;
                    if matches!(self.transcript.last(), Some(TranscriptItem::Assistant(_))) {
                        self.transcript
                            .push(TranscriptItem::Assistant(String::new()));
                        self.transcript_layout
                            .invalidate_from(self.transcript.len().saturating_sub(1));
                    }
                }
                if self.status != RunStatus::Cancelling {
                    self.status = RunStatus::Running;
                }

                self.turn = turn;
                self.ensure_assistant_item();
            }

            AgentEvent::ContextUsage { estimated_input } => {
                self.model_started_at = Some(Instant::now());
                self.context_used_tokens = self.calibration().estimate(estimated_input);
            }

            AgentEvent::RequestMetrics {
                profile,
                estimated_input,
                usage,
                ..
            } => {
                let reported_input = usage.as_ref().and_then(|usage| usage.prompt_tokens);
                self.calibrations
                    .entry(profile)
                    .or_default()
                    .observe(estimated_input, reported_input);
                self.context_used_tokens = reported_input
                    .and_then(|tokens| usize::try_from(tokens).ok())
                    .filter(|tokens| *tokens > 0)
                    .unwrap_or_else(|| self.calibration().estimate(estimated_input));
            }

            AgentEvent::CompactionRecorded { record } => {
                self.pending_compaction_archive = record.archive;
            }
            AgentEvent::VerificationRecorded { .. } => {}
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

                let archive_note =
                    if strategy == orbit_agent::memory::CompactionStrategy::ArchiveProjection {
                        self.pending_compaction_archive
                            .take()
                            .and_then(|reference| {
                                reference
                                    .strip_prefix("read_history archive_id=")
                                    .and_then(|value| value.split(';').next())
                                    .map(|id| format!(" · 归档 #{id} · 旧约束需按需回读"))
                            })
                            .unwrap_or_else(|| " · 旧约束需按需回读".into())
                    } else {
                        self.pending_compaction_archive = None;
                        String::new()
                    };

                self.transcript.push(TranscriptItem::Maintenance(format!(
                    "{action}  {before_tokens} → {after_tokens} tokens{detail}{archive_note}"
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

            AgentEvent::ReasoningFinished { elapsed_ms } => {
                let index = self.active_thinking;
                self.finish_thinking();
                if let Some(TranscriptItem::Thinking(row)) =
                    index.and_then(|i| self.transcript.get_mut(i))
                {
                    row.elapsed = Some(std::time::Duration::from_millis(elapsed_ms));
                }
            }
            AgentEvent::Model(event) => self.apply_model_event(event),

            AgentEvent::ToolStarted { call } => {
                self.finish_thinking();
                self.finish_assistant();
                self.tool_calls += 1;
                self.transcript.push(TranscriptItem::Tool(ToolRow {
                    id: call.id,
                    name: call.function.name,
                    arguments: call.function.arguments,
                    status: ToolStatus::Running,
                    output: String::new(),
                }));
                self.transcript_layout
                    .invalidate_from(self.transcript.len().saturating_sub(2));
            }

            AgentEvent::ToolFinished { call, content, .. } => {
                self.update_tool(&call, ToolStatus::Finished, content);
            }
            AgentEvent::ToolBackgrounded { call, content, .. } => {
                self.update_tool(&call, ToolStatus::Running, content);
            }
            AgentEvent::InputReceived { content, .. } => {
                self.finish_thinking();
                self.finish_assistant();
                let first = self.transcript.len();
                self.transcript.push(TranscriptItem::User(content));
                self.transcript_layout.invalidate_from(first);
            }
            AgentEvent::WaitingForCommands { .. } | AgentEvent::WaitingForTasks { .. } => {
                self.background_waiting = true;
                self.finish_thinking();
                self.model_started_at = None;
                self.finish_assistant();
            }
            AgentEvent::ToolFailed { call, error } => {
                self.update_tool(&call, ToolStatus::Failed, error);
            }
            AgentEvent::Completed { .. } => {
                self.finish_assistant();
                self.status = RunStatus::Completed;
                self.finish_clock();
            }
            AgentEvent::Failed { error } => {
                self.finish_pending_tools("运行已中断；已请求取消未完成命令。");
                self.status = RunStatus::Failed;
                self.finish_clock();
                self.transcript.push(TranscriptItem::Error(error));
            }
            AgentEvent::Cancelled => {
                self.finish_pending_tools("运行已取消；已请求取消未完成命令。");
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

    pub fn menu(&self) -> Vec<super::completion::Candidate> {
        if self.restore_picker.is_some() || self.menu_dismissed || self.resume_loading.is_some() {
            return Vec::new();
        }

        let command = self.input.text.split_whitespace().next().unwrap_or("");
        if self.running()
            && self.input.text.contains(char::is_whitespace)
            && !super::commands::available_while_running(command)
        {
            return Vec::new();
        }

        if let Some(argument) = self.input.text.strip_prefix("/cd ") {
            return self.directory_completions.matching(
                argument,
                std::path::Path::new(&self.workspace),
                self.completion_home.as_deref(),
            );
        }

        if let Some(query) = self.input.text.strip_prefix("/reasoning ") {
            return super::search::ranked(self.reasoning_efforts.iter(), |effort| {
                super::search::rank(effort.as_str(), query.trim())
            })
            .into_iter()
            .map(|effort| super::completion::Candidate {
                name: format!("/reasoning {}", effort.as_str()),
                display: Some(effort.as_str().into()),
                description: String::new(),
                continue_input: false,
            })
            .collect();
        }

        if !self.stored_completions.is_empty() {
            return self.stored_completions.clone();
        }

        super::completion::matching(
            &self.input.text,
            &self.model_completions,
            &self.skill_completions,
        )
        .into_iter()
        .filter(|candidate| {
            !self.running()
                || super::commands::available_while_running(
                    candidate.name.split_whitespace().next().unwrap_or(""),
                )
        })
        .collect()
    }

    pub(super) fn runtime_snapshot(&self) -> orbit_store::tui_sessions::TuiRuntime {
        orbit_store::tui_sessions::TuiRuntime {
            model: self.model.clone(),
            effort: self
                .reasoning_effort
                .map(|effort| effort.as_str().to_owned()),
            context_used: self.context_used_tokens,
            context_limit: self.context_tokens,
            input_limit: self.input_context_tokens,
            max_output: self.max_output_tokens,
            elapsed_ms: self.active_elapsed().as_millis().min(u64::MAX as u128) as u64,
            sampled_at_ms: super::session_sync::now_ms(),
            paused: self.paused_at.is_some(),
            status: match self.status {
                RunStatus::Idle => "idle",
                RunStatus::Running => "running",
                RunStatus::Cancelling => "cancelling",
                RunStatus::Completed => "completed",
                RunStatus::Failed => "failed",
                RunStatus::Cancelled => "cancelled",
            }
            .into(),
            turn: self.turn,
            tool_calls: self.tool_calls,
        }
    }

    pub(super) fn apply_runtime(&mut self, runtime: &orbit_store::tui_sessions::TuiRuntime) {
        if self.model == runtime.model
            && self.reasoning_effort.map(|e| e.as_str()) == runtime.effort.as_deref()
        {
            self.context_used_tokens = runtime.context_used;
            self.context_tokens = runtime.context_limit;
            self.input_context_tokens = runtime.input_limit;
            self.max_output_tokens = runtime.max_output;
        }
        let live = matches!(self.status, RunStatus::Running | RunStatus::Cancelling)
            && matches!(runtime.status.as_str(), "running" | "cancelling");
        let terminal = matches!(
            (&self.status, runtime.status.as_str()),
            (RunStatus::Completed, "completed")
                | (RunStatus::Failed, "failed")
                | (RunStatus::Cancelled, "cancelled")
        );
        if !live && !terminal {
            return;
        }
        let elapsed = runtime
            .elapsed_ms
            .saturating_add(if live && !runtime.paused {
                super::session_sync::now_ms().saturating_sub(runtime.sampled_at_ms)
            } else {
                0
            });
        self.elapsed = std::time::Duration::from_millis(elapsed);
        let now = Instant::now();
        self.started_at = live.then(|| now.checked_sub(self.elapsed).unwrap_or(now));
        self.paused_at = (live && runtime.paused).then_some(now);
        self.paused_elapsed = std::time::Duration::ZERO;
        self.turn = runtime.turn;
        self.tool_calls = runtime.tool_calls;
        if live && runtime.status == "cancelling" {
            self.status = RunStatus::Cancelling;
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
        self.finish_thinking();
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
                if text.is_empty() {
                    return;
                }

                self.finish_thinking();
                self.model_started_at = None;
                self.ensure_assistant_item();

                if let Some(TranscriptItem::Assistant(content)) = self.transcript.last_mut() {
                    content.push_str(&text);
                }
            }
            StreamEvent::ReasoningDelta(text) => {
                if text.is_empty() {
                    return;
                }

                if let Some(TranscriptItem::Thinking(row)) = self
                    .active_thinking
                    .and_then(|index| self.transcript.get_mut(index))
                {
                    row.content.push_str(&text);
                } else {
                    self.model_started_at = None;
                    self.active_thinking = Some(self.transcript.len());
                    self.transcript.push(TranscriptItem::Thinking(ThinkingRow {
                        started_at: Some(Instant::now()),
                        ..text.into()
                    }));
                }

                if let Some(index) = self.active_thinking {
                    self.transcript_layout.invalidate_from(index);
                }
            }
            StreamEvent::ToolCallDelta(_) | StreamEvent::Completed { .. } => self.finish_thinking(),
        }
    }

    /// Historical events have no reasoning timestamps. Never time their replay.
    pub fn apply_replayed(&mut self, event: AgentEvent) {
        let has_timing = matches!(event, AgentEvent::ReasoningFinished { .. });
        self.apply(event);
        self.model_started_at = None;

        if let Some(TranscriptItem::Thinking(row)) = self.transcript.last_mut() {
            row.started_at = None;
            if !has_timing && !row.finished {
                row.elapsed = None;
            }
        }
    }

    fn finish_thinking(&mut self) {
        let Some(index) = self.active_thinking.take() else {
            return;
        };
        let Some(TranscriptItem::Thinking(row)) = self.transcript.get_mut(index) else {
            return;
        };

        row.finished = true;
        row.elapsed = row.started_at.take().map(|start| start.elapsed());
        self.transcript_layout.invalidate_from(index);
    }

    fn finish_assistant(&mut self) {
        let index = self.transcript.len().saturating_sub(1);
        let Some(TranscriptItem::Assistant(content)) = self.transcript.last_mut() else {
            return;
        };
        let trimmed = content.trim();

        if trimmed.is_empty() {
            self.transcript.pop();
        } else if trimmed.len() != content.len() {
            *content = trimmed.to_owned();
        } else {
            return;
        }

        self.transcript_layout.reset_from(index);
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

    fn finish_pending_tools(&mut self, message: &str) {
        for (index, item) in self.transcript.iter_mut().enumerate() {
            if let TranscriptItem::Tool(row) = item
                && row.status == ToolStatus::Running
            {
                row.status = ToolStatus::Failed;
                row.output = message.into();
                self.transcript_layout.invalidate_from(index);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn background_tools_finish_the_original_row_and_cancel_pending_rows() {
        let mut state = UiState::new("orbit-test");
        let first = call("orbit-first");
        let second = call("orbit-second");
        state.apply(AgentEvent::ToolStarted {
            call: first.clone(),
        });
        state.apply(AgentEvent::ToolBackgrounded {
            call: first.clone(),
            job_id: "job-orbit-first".into(),
            content: "{\"running\":true}".into(),
        });
        state.apply(AgentEvent::ToolStarted { call: second });
        state.apply(AgentEvent::Model(StreamEvent::TextDelta(
            "Other Orbit work".into(),
        )));
        state.apply(AgentEvent::ToolFinished {
            call: first,
            content: "Orbit finished".into(),
            changes: vec![],
        });
        assert_eq!(state.tool_calls, 2);
        let TranscriptItem::Tool(first) = &state.transcript[0] else {
            panic!("missing first command");
        };
        assert_eq!(first.status, ToolStatus::Finished);
        assert_eq!(first.output, "Orbit finished");
        let TranscriptItem::Tool(second) = &state.transcript[1] else {
            panic!("missing second command");
        };
        assert_eq!(second.status, ToolStatus::Running);
        state.apply(AgentEvent::Cancelled);
        let TranscriptItem::Tool(second) = &state.transcript[1] else {
            panic!("missing second command");
        };
        assert_eq!(second.status, ToolStatus::Failed);
        assert!(second.output.contains("取消"));
    }

    #[test]
    fn resuming_after_command_wait_starts_a_separate_assistant_message() {
        let mut state = UiState::new("orbit-test");
        state.apply(AgentEvent::TurnStarted { turn: 1 });
        state.apply(AgentEvent::Model(StreamEvent::TextDelta(
            "Orbit is building.".into(),
        )));
        state.apply(AgentEvent::WaitingForCommands {
            job_ids: vec!["job-orbit".into()],
        });
        state.apply(AgentEvent::TurnStarted { turn: 2 });
        state.apply(AgentEvent::Model(StreamEvent::TextDelta(
            "Orbit build passed.".into(),
        )));
        assert!(
            matches!(&state.transcript[0], TranscriptItem::Assistant(text) if text == "Orbit is building.")
        );
        assert!(
            matches!(&state.transcript[1], TranscriptItem::Assistant(text) if text == "Orbit build passed.")
        );
    }

    #[test]
    fn reasoning_completion_only_offers_current_model_capabilities() {
        use orbit_model::types::ReasoningEffort;

        let mut state = super::UiState::new("provider::model");
        state.reasoning_efforts = vec![ReasoningEffort::Low, ReasoningEffort::High];
        state.input.set("/reasoning ".into());

        let names = state
            .menu()
            .into_iter()
            .map(|item| item.name)
            .collect::<Vec<_>>();

        assert_eq!(names, ["/reasoning low", "/reasoning high"]);
        state.input.set("/reasoning hi".into());
        assert_eq!(state.menu()[0].name, "/reasoning high");
    }
    use super::*;
    use orbit_model::types::FunctionCall;

    #[test]
    fn reasoning_finishes_before_text_tools_and_terminal_events() {
        let endings = [
            AgentEvent::Model(StreamEvent::TextDelta("answer".into())),
            AgentEvent::ToolStarted { call: call("tool") },
            AgentEvent::Completed {
                turns: 1,
                tool_calls: 0,
            },
            AgentEvent::Failed {
                error: "network".into(),
            },
            AgentEvent::Cancelled,
        ];

        for ending in endings {
            let mut state = UiState::new("mock");
            state.apply(AgentEvent::Model(StreamEvent::ReasoningDelta(
                "first".into(),
            )));
            state.apply(AgentEvent::Model(StreamEvent::ReasoningDelta(
                " second".into(),
            )));
            let TranscriptItem::Thinking(row) = &mut state.transcript[0] else {
                panic!("missing reasoning");
            };
            assert!(!row.finished);
            row.started_at = Some(Instant::now() - std::time::Duration::from_millis(7500));

            state.apply(ending);

            let TranscriptItem::Thinking(row) = &state.transcript[0] else {
                panic!("missing reasoning");
            };
            assert!(row.finished);
            assert_eq!(row.content, "first second");
            assert!(row.elapsed.unwrap() >= std::time::Duration::from_millis(7500));
            let elapsed = row.elapsed;
            state.finish_clock();
            let TranscriptItem::Thinking(row) = &state.transcript[0] else {
                unreachable!()
            };
            assert_eq!(
                row.elapsed, elapsed,
                "later completion must not extend reasoning time"
            );
        }
    }

    #[test]
    fn thinking_duration_measures_reasoning_stream_time() {
        let mut state = UiState::new("mock");
        state.apply(AgentEvent::ContextUsage { estimated_input: 1 });
        state.model_started_at = Some(Instant::now() - std::time::Duration::from_secs(3));

        state.apply(AgentEvent::Model(StreamEvent::ReasoningDelta(
            "buffered reasoning".into(),
        )));
        state.apply(AgentEvent::Model(StreamEvent::TextDelta("answer".into())));

        let TranscriptItem::Thinking(row) = &state.transcript[0] else {
            panic!("missing reasoning");
        };
        assert!(row.elapsed.unwrap() < std::time::Duration::from_secs(2));
        assert!(state.model_started_at.is_none());
    }

    #[test]
    fn replay_preserves_recorded_reasoning_time() {
        let events = vec![
            AgentEvent::Model(StreamEvent::ReasoningDelta("reasoning".into())),
            AgentEvent::ReasoningFinished { elapsed_ms: 7500 },
            AgentEvent::Model(StreamEvent::Completed {
                finish_reason: None,
                usage: None,
            }),
        ];
        let stored = serde_json::to_string(&events).unwrap();
        let restored: Vec<AgentEvent> = serde_json::from_str(&stored).unwrap();
        let mut state = UiState::new("mock");

        for event in restored {
            state.apply_replayed(event);
        }

        let TranscriptItem::Thinking(row) = &state.transcript[0] else {
            panic!("missing reasoning");
        };
        assert_eq!(row.elapsed, Some(std::time::Duration::from_millis(7500)));
    }

    #[test]
    fn replay_does_not_invent_reasoning_duration() {
        let mut state = UiState::new("mock");
        state.apply_replayed(AgentEvent::Model(StreamEvent::ReasoningDelta(
            "old thought".into(),
        )));
        state.apply_replayed(AgentEvent::Model(StreamEvent::TextDelta(
            "old answer".into(),
        )));

        let TranscriptItem::Thinking(row) = &state.transcript[0] else {
            panic!("missing reasoning");
        };
        assert!(row.finished);
        assert_eq!(row.elapsed, None);
    }

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
            metadata: None,
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

    #[test]
    fn archive_projection_explains_that_older_constraints_need_retrieval() {
        use orbit_agent::memory::{CompactionMode, CompactionRecord, FileActivity, TaskSummary};

        let mut state = UiState::new("mock");
        state.apply(AgentEvent::CompactionRecorded {
            record: CompactionRecord {
                version: 2,
                mode: CompactionMode::Manual,
                source_messages: 8,
                retained_from: 4,
                pinned_user: None,
                before_tokens: 200_000,
                after_tokens: 40_000,
                strategy: orbit_agent::memory::CompactionStrategy::ArchiveProjection,
                summary_requests: 1,
                summary_input_tokens: 2_048,
                archive: Some("read_history archive_id=12; search with query".into()),
                fallback: true,
                fallback_reason: None,
                summary: TaskSummary {
                    goal: String::new(),
                    constraints: vec![],
                    decisions: vec![],
                    completed: vec![],
                    remaining: vec![],
                    verification: vec![],
                    uncertainty: vec![],
                },
                files: FileActivity::default(),
            },
        });
        state.apply(AgentEvent::ContextCompacted {
            strategy: orbit_agent::memory::CompactionStrategy::ArchiveProjection,
            before_tokens: 200_000,
            after_tokens: 40_000,
            fallback: true,
            fallback_reason: None,
        });

        assert!(matches!(
            state.transcript.as_slice(),
            [TranscriptItem::Maintenance(text)]
                if text.contains("摘要未完成") && text.contains("归档 #12") && text.contains("旧约束需按需回读")
        ));
    }

    #[test]
    fn context_indicator_tracks_each_model_request_during_a_run() {
        let mut state = UiState::new("mock");
        state.begin_run();
        state.apply(AgentEvent::ContextUsage {
            estimated_input: 4_000,
        });

        assert_eq!(state.context_used_tokens, 4_000);

        state.apply(AgentEvent::ContextUsage {
            estimated_input: 12_000,
        });

        assert_eq!(state.context_used_tokens, 12_000);

        state.apply(AgentEvent::RequestMetrics {
            profile: "mock".into(),
            estimated_input: 12_000,
            retries: None,
            usage: Some(orbit_model::types::Usage {
                prompt_tokens: Some(13_000),
                completion_tokens: None,
                total_tokens: None,
            }),
            elapsed_ms: 100,
        });

        assert_eq!(state.context_used_tokens, 13_000);
    }
}
