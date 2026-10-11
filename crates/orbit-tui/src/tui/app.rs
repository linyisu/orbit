mod activity;

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
    UserWithImages {
        text: String,
        images: Vec<orbit_protocol::MessageImage>,
    },
    Assistant(String),
    Thinking(ThinkingRow),
    Tool(ToolRow),
    Error(String),
    Notice(String),
    /// Low-priority lifecycle information rendered as an unobtrusive divider
    /// instead of pretending to be an assistant or command response.
    Maintenance(String),
    CommandError(String),
}

#[derive(Debug)]
pub struct UiState {
    pub(super) pending_input: super::pending_input::State,
    pub(super) feedback: super::feedback::Feedback,
    pub(super) connection_label: Option<String>,
    pub(super) media: super::media::Context,
    pub(super) media_preview: Option<super::media::Preview>,
    pub tabs: Vec<SessionTab>,
    pub new_tab_area: ratatui::layout::Rect,

    pub tab_close_areas: Vec<(ratatui::layout::Rect, String)>,
    pub(super) tab_hover: Option<(super::TabAction, Instant)>,
    pub(super) tab_hover_tick: u128,
    pub tab_areas: Vec<(ratatui::layout::Rect, String)>,
    pub(super) navigation: super::navigation::Navigation,
    pub approval: Option<orbit_tools::approval::ApprovalPreview>,
    pub approval_area: ratatui::layout::Rect,
    pub approval_scroll: usize,
    pub approval_max_scroll: usize,
    pub approval_shown: bool,
    pub approval_choice: usize,
    pub permissions: super::permissions::State,
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
    pub(super) footnote_focus: Option<super::markdown::footnotes::Focus>,
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
    pub(super) overlay_layer: Option<super::overlay::Layer>,
    pub(super) overlay_needs_layout: bool,
    pub(super) cursor_motion: super::cursor::CursorMotion,
    pub(super) user_input: super::user_input::State,
    pub(super) planning: super::planning::State,
    pub restore_picker: Option<super::restore::RestorePicker>,
    pub restore_selection: Option<Vec<String>>,
    pub workspace_wizard: Option<super::workspace_client::wizard::Wizard>,
    pub ssh_prompt: Option<super::workspace_client::auth::Prompt>,
    pub stored_completions: Vec<super::completion::Candidate>,
    pub completion_query: Option<String>,
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
    pub info_dialog: Option<super::dialogs::InfoDialog>,
    pub(super) usage: super::usage::State,
    pub(super) btw: super::btw::Questions,
    pub(super) subagent_dialog: Option<Box<super::subagents::Viewer>>,
    pub picker: Option<super::dialogs::SessionPicker>,
    pub resume_request: Option<String>,
    pub(super) subagent_parent: Option<String>,
    pub(super) subagent_parent_area: ratatui::layout::Rect,
    pub(super) subagent_parent_hovered: bool,
    pub(super) subagent_parent_pressed: bool,
    pub delete_request: Option<String>,
    pub resume_loading: Option<String>,
    pub model_picker: Option<super::dialogs::ModelPicker>,
    pub model_update_requested: bool,
    pub model_update_area: Option<ratatui::layout::Rect>,
    pub model_update_hover: bool,
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
    pub(super) expanded_tool_groups: std::collections::BTreeSet<usize>,
    pub max_scroll: usize,
    pub(super) transcript_layout: super::transcript::TranscriptLayout,
    pub elapsed: std::time::Duration,
    remote_replay: bool,
    pub input: InputState,
    pub transcript: Vec<TranscriptItem>,
    pub status: RunStatus,
    pub activity: activity::Activity,
    pub turn: usize,
    pub tool_calls: usize,
    pub scroll: usize,
    pub follow_bottom: bool,
    pub started_at: Option<Instant>,
    paused_at: Option<Instant>,
    paused_elapsed: std::time::Duration,
}

impl UiState {
    pub(super) fn is_home(&self) -> bool {
        !self.has_conversation() && self.subagent_parent.is_none() && self.resume_loading.is_none()
    }

    pub(super) fn dialog_open(&self) -> bool {
        super::overlay::active(self).is_some()
    }

    /// Operational feedback is separate from agent conversation history.
    pub(super) fn report_local(&mut self, item: TranscriptItem) {
        if let TranscriptItem::Error(text) | TranscriptItem::CommandError(text) = &item {
            self.feedback.error(text);
            return;
        }
        match item {
            TranscriptItem::Notice(text) => self.feedback.info("提示", &text),
            TranscriptItem::Maintenance(_) if self.is_home() => {}
            _ => self.transcript.push(item),
        }
    }

    pub(super) fn handle_feedback(&mut self, event: &crossterm::event::Event) -> bool {
        match self.feedback.handle(event) {
            super::feedback::Action::None => false,
            super::feedback::Action::Redraw => true,
            super::feedback::Action::Details { title, text } => {
                self.info_dialog = Some(super::dialogs::InfoDialog::new(&title, text));
                true
            }
        }
    }

    pub(super) fn show_output(&mut self, title: &str, content: String) {
        if content.lines().nth(1).is_some() {
            self.info_dialog = Some(super::dialogs::InfoDialog::new(title, content));
        } else {
            self.feedback.info(title, &content);
        }
    }

    pub(super) fn has_conversation(&self) -> bool {
        self.transcript.iter().any(|item| {
            matches!(
                item,
                TranscriptItem::User(_)
                    | TranscriptItem::UserWithImages { .. }
                    | TranscriptItem::Assistant(_)
                    | TranscriptItem::Thinking(_)
                    | TranscriptItem::Tool(_)
            )
        })
    }

    pub fn invalidate_transcript(&mut self, first: usize) {
        self.transcript_layout.invalidate_from(first);
    }

    pub(super) fn start_remote(
        &mut self,
        prompt: String,
        presentation: orbit_protocol::RunPresentation,
    ) {
        self.remote_replay = presentation.replayed;
        let (scroll, follow) = (self.scroll, self.follow_bottom);
        if !prompt.is_empty() && self.input.history.last() != Some(&prompt) {
            self.input.history.push(prompt.clone());
        }
        if presentation.compact || prompt == "/compact" || presentation.queued_input {
            self.begin_run();
            if presentation.replayed {
                self.scroll = scroll;
                self.follow_bottom = follow;
            }
            return;
        }
        if !presentation.image_assets.is_empty() {
            self.submit_with_images(prompt, presentation.image_assets);
            if presentation.replayed {
                self.scroll = scroll;
                self.follow_bottom = follow;
            }
            return;
        }
        let images = presentation
            .images
            .iter()
            .map(|name| format!("[图片：{name}]"))
            .collect::<Vec<_>>()
            .join(" ");
        let display = match (prompt.is_empty(), images.is_empty()) {
            (_, true) => prompt,
            (true, false) => images,
            (false, false) => format!("{prompt}\n{images}"),
        };
        self.submit(display);
        if presentation.replayed {
            self.scroll = scroll;
            self.follow_bottom = follow;
        }
    }

    pub(super) fn finish_remote(&mut self, status: &str, elapsed_ms: Option<u64>) {
        self.finish_clock();
        if let Some(elapsed_ms) = elapsed_ms {
            self.elapsed = std::time::Duration::from_millis(elapsed_ms);
        }
        self.status = match status {
            "completed" => RunStatus::Completed,
            "cancelled" => RunStatus::Cancelled,
            _ => RunStatus::Failed,
        };
        let first = self
            .transcript
            .iter()
            .rposition(|item| {
                matches!(
                    item,
                    TranscriptItem::User(_) | TranscriptItem::UserWithImages { .. }
                )
            })
            .unwrap_or(0);
        for item in &mut self.transcript[first..] {
            if let TranscriptItem::Tool(tool) = item
                && tool.status == ToolStatus::Running
                && (self.remote_replay || status != "completed")
            {
                tool.status = ToolStatus::Failed;
                tool.output = "运行已结束或中断，未保存工具结果；不会自动重试。".into();
            }
        }
        self.invalidate_transcript(first);
    }

    pub(super) fn sync_clock(&mut self, timing: orbit_protocol::RunTiming) {
        let now = Instant::now();
        self.elapsed = std::time::Duration::from_millis(timing.elapsed_ms);
        self.started_at = Some(now.checked_sub(self.elapsed).unwrap_or(now));
        self.paused_at = timing.paused.then_some(now);
        self.paused_elapsed = std::time::Duration::ZERO;
    }

    pub(super) fn apply_tool_progress(&mut self, updates: Vec<orbit_protocol::ToolProgress>) {
        for update in updates {
            let Some((index, tool)) =
                self.transcript
                    .iter_mut()
                    .enumerate()
                    .rev()
                    .find_map(|(index, item)| match item {
                        TranscriptItem::Tool(tool)
                            if tool.id == update.call_id && tool.status == ToolStatus::Running =>
                        {
                            Some((index, tool))
                        }
                        _ => None,
                    })
            else {
                continue;
            };
            let mut output = serde_json::from_str::<serde_json::Value>(&tool.output)
                .ok()
                .filter(serde_json::Value::is_object)
                .unwrap_or_else(|| serde_json::json!({}));
            if let Some(stdout) = update.stdout {
                output["stdout"] = stdout.into();
                output["running"] = true.into();
            }
            if let Some(elapsed) = update.elapsed_ms {
                output["elapsed_seconds"] = (elapsed / 1000).into();
            }
            let output = output.to_string();
            if output != tool.output {
                tool.output = output;
                self.transcript_layout.invalidate_from(index);
            }
        }
    }

    pub fn calibration(&self) -> orbit_agent::calibration::TokenCalibration {
        self.calibrations
            .get(&self.model)
            .cloned()
            .unwrap_or_default()
    }

    pub fn new(model: impl Into<String>) -> Self {
        Self {
            pending_input: Default::default(),
            feedback: Default::default(),
            connection_label: None,
            media: Default::default(),
            media_preview: None,
            tabs: Vec::new(),
            new_tab_area: Default::default(),

            tab_close_areas: Vec::new(),
            tab_hover: None,
            tab_hover_tick: 0,
            tab_areas: Vec::new(),
            navigation: Default::default(),
            approval: None,
            approval_area: ratatui::layout::Rect::default(),
            approval_scroll: 0,
            approval_max_scroll: 0,
            approval_shown: false,
            approval_choice: 0,
            permissions: super::permissions::State::default(),
            selection: super::selection::Selection::default(),
            context_tokens: 128_000,
            input_context_tokens: 96_000,
            context_used_tokens: 0,
            calibrations: Default::default(),
            context_indicator_area: ratatui::layout::Rect::default(),
            context_hovered: false,
            context_details_visible: false,
            link_hovered: None,
            footnote_focus: None,
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
            overlay_layer: None,
            overlay_needs_layout: false,
            cursor_motion: Default::default(),
            user_input: Default::default(),
            planning: Default::default(),
            restore_picker: None,
            restore_selection: None,
            workspace_wizard: None,
            ssh_prompt: None,
            stored_completions: Vec::new(),
            completion_query: None,
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
            info_dialog: None,
            usage: Default::default(),
            btw: Default::default(),
            subagent_dialog: None,
            picker: None,
            resume_request: None,
            subagent_parent: None,
            subagent_parent_area: Default::default(),
            subagent_parent_hovered: false,
            subagent_parent_pressed: false,
            delete_request: None,
            resume_loading: None,
            model_picker: None,
            model_update_requested: false,
            model_update_area: None,
            model_update_hover: false,
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
            expanded_tool_groups: Default::default(),
            max_scroll: 0,
            transcript_layout: Default::default(),
            elapsed: std::time::Duration::ZERO,
            remote_replay: false,
            input: InputState::default(),
            transcript: Vec::new(),
            status: RunStatus::Idle,
            activity: activity::Activity::default(),
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

    pub fn submit_with_images(&mut self, text: String, images: Vec<orbit_protocol::MessageImage>) {
        if images.is_empty() {
            self.submit(text);
        } else {
            self.transcript
                .push(TranscriptItem::UserWithImages { text, images });
            self.begin_run();
        }
    }

    pub fn submit(&mut self, prompt: String) {
        self.transcript.push(TranscriptItem::User(prompt));
        self.begin_run();
    }

    pub fn begin_run(&mut self) {
        self.status = RunStatus::Running;
        self.activity = activity::Activity::default();
        self.show_help = false;
        self.info_dialog = None;
        self.elapsed = std::time::Duration::ZERO;
        self.scroll = 0;
        self.turn = 0;
        self.tool_calls = 0;
        self.started_at = Some(Instant::now());
        self.paused_at = None;
        self.paused_elapsed = std::time::Duration::ZERO;
        self.follow_bottom = true;
    }

    pub fn apply(&mut self, event: AgentEvent) {
        self.activity.apply(&event);
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
            AgentEvent::InputReceived {
                message_id,
                content,
            } => {
                self.pending_input.received(message_id);
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
                if !self.remote_replay {
                    self.feedback.error(&error);
                }
                self.transcript.push(TranscriptItem::Error(error));
            }

            AgentEvent::Cancelled => {
                self.finish_pending_tools("运行已取消；已请求取消未完成命令。");
                self.status = RunStatus::Cancelled;
                self.finish_clock();
            }
        }
    }

    pub fn expire_footnote_focus(&mut self) -> bool {
        if self
            .footnote_focus
            .as_ref()
            .is_some_and(|focus| focus.expired())
        {
            self.footnote_focus = None;
            true
        } else {
            false
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

    pub(super) fn directory_completion_pending(&self) -> bool {
        self.connection_label.is_some()
            && self.input.text.starts_with("/cd ")
            && self.completion_query.as_deref() != Some(&self.input.text)
            && !self.running()
            && !self.menu_dismissed
            && !self.dialog_open()
            && self.workspace_wizard.is_none()
            && self.ssh_prompt.is_none()
            && self.resume_loading.is_none()
    }

    pub fn menu(&self) -> Vec<super::completion::Candidate> {
        if self.workspace_wizard.is_some()
            || self.ssh_prompt.is_some()
            || self.dialog_open()
            || self.restore_picker.is_some()
            || self.attachment_cursor.is_some()
            || self.menu_dismissed
            || self.resume_loading.is_some()
        {
            return Vec::new();
        }

        let command = self.input.text.split_whitespace().next().unwrap_or("");
        if self.running()
            && self.input.text.contains(char::is_whitespace)
            && !super::commands::available_while_running(command)
        {
            return Vec::new();
        }

        if self.connection_label.is_none()
            && let Some(argument) = self.input.text.strip_prefix("/cd ")
        {
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

        if self.completion_query.as_deref() == Some(&self.input.text)
            && !self.stored_completions.is_empty()
        {
            return self.stored_completions.clone();
        }

        if let Some(previous) = &self.completion_query
            && let Some(entries) = super::completion::cached_directories(
                &self.input.text,
                previous,
                &self.stored_completions,
            )
        {
            return entries;
        }

        super::completion::matching(
            &self.input.text,
            &self.model_completions,
            &self.skill_completions,
        )
        .into_iter()
        .filter(|candidate| {
            if self.connection_label.is_some() {
                super::commands::available_in_workspace(
                    candidate.name.split_whitespace().next().unwrap_or(""),
                )
            } else {
                candidate.name != "/reconnect"
            }
        })
        .filter(|candidate| {
            !self.running()
                || super::commands::available_while_running(
                    candidate.name.split_whitespace().next().unwrap_or(""),
                )
        })
        .collect()
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
                        // Replayed events carry recorded timings; replay speed is not model time.
                        started_at: (!self.remote_replay).then(Instant::now),
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
mod tests;
