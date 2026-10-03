pub mod app;
mod approval;
mod approval_diff;
mod background;
mod changes;
mod code;
pub mod commands;
mod completion;
mod connect;
mod connect_custom;
mod connect_key;
mod dialogs;
mod directory;
mod editor;
mod frames;
pub mod input;
mod languages;
mod markdown;
mod navigation;
mod overlay;
mod permissions;
mod project_label;
pub mod render;
mod restore;
mod search;
mod selection;
mod session_sync;
mod tabs;
pub mod terminal;
pub mod theme;
mod tool_display;
mod transcript;
mod wheel;
mod workspace;

use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures_util::StreamExt;
use orbit_agent::{
    agent::{Agent, AgentConfig, AgentResult},
    error::AgentError,
    events::AgentEvent,
    memory::CompactionStrategy,
    model::ModelClientBackend,
    tool::ToolRegistry,
};
use orbit_model::{
    client::ModelClient,
    types::{Attachment, ChatMessage, ChatRequest},
};
use tokio::{select, sync::mpsc, task::JoinHandle, time};
use tokio_util::sync::CancellationToken;

use crate::session::SessionState;
use orbit_store::types::Run;

use self::{
    app::{TranscriptItem, UiState},
    terminal::TerminalGuard,
};

type RunTask = JoinHandle<Result<AgentResult, AgentError>>;

type LocalTask = JoinHandle<Result<String, orbit_agent::tool::ToolError>>;

type MarketplaceTask = JoinHandle<Result<crate::marketplaces::Marketplace>>;

type ResumeTask = JoinHandle<Result<(crate::session::PreparedResume, Vec<ChatMessage>)>>;

struct StartedRun {
    receiver: mpsc::Receiver<AgentEvent>,
    cancellation: CancellationToken,
    task: RunTask,
}

struct CommandContext<'a> {
    tools: &'a ToolRegistry,
    instructions: &'a crate::instructions::Instructions,
    skills: &'a crate::skills::Skills,
    mcp_statuses: &'a [orbit_tools::mcp::McpServerStatus],
    model_options: &'a [ModelOption],
}

type ModelRefreshOutput = (Vec<ModelOption>, Vec<String>);

type ModelRefreshTask = tokio_util::task::AbortOnDropHandle<Result<ModelRefreshOutput>>;

#[derive(Clone, Debug)]
pub struct ModelOption {
    pub max_output_tokens: u32,
    pub provider: String,
    pub model: String,
    /// Request budget passed to the agent, including the output upper bound.
    pub context_tokens: usize,
    /// Full usable input window shown to the user and used for compaction.
    pub input_context_tokens: usize,
    pub default_for_provider: bool,
    pub reasoning_efforts: Vec<orbit_model::types::ReasoningEffort>,
    pub supports_vision: bool,
    pub config: orbit_model::types::ModelConfig,
}

impl ModelOption {
    pub fn qualified_name(&self) -> String {
        format!("{}::{}", self.provider, self.model)
    }
}

pub struct RequestBudget {
    pub context_tokens: usize,
    pub input_context_tokens: usize,
    pub auto_compact: bool,
    pub max_output_tokens: u32,
    pub max_turns: usize,
    pub max_tool_calls: usize,
    pub instructions: crate::instructions::Instructions,
    pub skills: crate::skills::Skills,
    pub plugin_skill_diagnostics: Vec<String>,
    pub mcp_statuses: Vec<orbit_tools::mcp::McpServerStatus>,
    pub model_options: Vec<ModelOption>,
    pub compaction: orbit_agent::compaction::CompactionRuntime,
    pub memory_scope: crate::project_state::MemoryScope,
}

pub async fn run(
    setup: crate::PreparedTui,
    options: &crate::TuiOptions,
) -> Result<Option<crate::PreparedTui>> {
    let crate::PreparedTui {
        mut session,
        resources,
    } = setup;
    let crate::TuiResources {
        mut model,
        model_name,
        mut tools,
        mut undo_writer,
        mut approvals,
        budget,
        moved,
        ..
    } = resources;
    let next_workspace: Option<crate::TuiResources> = None;
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    let mut state = UiState::new(model_name);
    let RequestBudget {
        context_tokens,
        input_context_tokens,
        mut auto_compact,
        max_output_tokens,
        mut max_turns,
        mut max_tool_calls,
        mut instructions,
        mut skills,
        plugin_skill_diagnostics,
        mut mcp_statuses,
        mut model_options,
        mut memory_scope,
        mut compaction,
    } = budget;
    state.context_tokens = context_tokens;
    state.input_context_tokens = input_context_tokens;
    state.auto_compact = auto_compact;
    state.max_output_tokens = max_output_tokens;
    state.reasoning_effort = model.config().reasoning_effort;
    state.reasoning_efforts = model_options
        .iter()
        .find(|choice| choice.qualified_name() == state.model)
        .map(|choice| choice.reasoning_efforts.clone())
        .unwrap_or_default();
    state.supports_vision = model.supports_vision();
    state.model_completions = model_options
        .iter()
        .map(ModelOption::qualified_name)
        .collect();
    let mut history_replay =
        (!session.snapshot.runs.is_empty()).then(crate::session::HistoryReplay::default);

    if history_replay.is_some() {
        state.resume_loading = Some("正在显示会话历史…".into());
    }

    state.completion_home = options.home.clone();
    state.skill_completions = skills.completions();
    state.workspace = session.snapshot.session.workspace.clone();
    let mut move_notice = moved.then(|| format!("Moved to {}", state.workspace));
    if history_replay.is_none()
        && let Some(notice) = move_notice.take()
    {
        state.transcript.push(TranscriptItem::CommandResult(notice));
    }

    for diagnostic in plugin_skill_diagnostics {
        state.transcript.push(TranscriptItem::Notice(format!(
            "插件 Skill 未载入：{diagnostic}"
        )));
    }

    restore_model_profile(&mut state, &mut model, &model_options, &session);
    let mut history = session.snapshot.history.clone();
    crate::instructions::inject(&mut history, &instructions);
    skills.inject(&mut history);
    crate::project_state::inject(
        &mut history,
        &session.store,
        &session.snapshot.session.project_id,
        session.id(),
    )
    .await?;
    refresh_context_usage(&mut state, &history, &tools);
    session.enable_shared();
    session_sync::sync_mode(&session.store, session.id(), &mut state).await?;
    let mut tabs = tabs::Tabs::new(&session);
    tabs.persist(&session).await?;
    let mut session_sync = session_sync::SessionSync::default();
    let mut sync_checked = std::time::Instant::now();
    let editor = editor::Editor::new(options.editor_command.clone());
    let mut events = EventStream::new();
    let mut ticks = time::interval(Duration::from_millis(33));
    ticks.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut command_ticks = time::interval(Duration::from_millis(50));
    command_ticks.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut receiver: Option<mpsc::Receiver<AgentEvent>> = None;
    let mut cancellation: Option<CancellationToken> = None;
    let mut task: Option<RunTask> = None;
    let mut local_task: Option<LocalTask> = None;
    let mut local_cancellation: Option<CancellationToken> = None;
    let mut resume_task: Option<ResumeTask> = None;
    let mut marketplace_task: Option<MarketplaceTask> = None;
    let mut model_refresh: Option<ModelRefreshTask> = None;
    let mut pending_guidance = Vec::new();
    let mut retry_submit = false;
    let mut restore_queue = std::collections::VecDeque::new();
    let mut restore_errors = Vec::new();
    let mut refresh_before_retry = false;
    let mut active_context: Option<ChatMessage> = None;
    let mut pending: Option<orbit_tools::approval::ApprovalRequest> = None;
    let mut frames = frames::Frames::default();
    let mut wheel = wheel::Wheel::default();
    let mut branch_task: Option<JoinHandle<(String, Option<String>)>> = None;
    let mut branch_checked = std::time::Instant::now();
    let mut branch_workspace = String::new();
    let mut background_runs = background::Runs::new();
    let mut bound_session = session.id().to_owned();

    macro_rules! park_run {
        () => {
            background_runs.insert(
                session.id().to_owned(),
                background::BackgroundRun {
                    task: task.take(),
                    local_task: local_task.take(),
                    local_cancellation: local_cancellation.take(),
                    history_replay: history_replay.take(),
                    move_notice: move_notice.take(),
                    marketplace_task: marketplace_task.take(),
                    model_refresh: model_refresh.take(),
                    retry_submit: std::mem::take(&mut retry_submit),
                    refresh_before_retry: std::mem::take(&mut refresh_before_retry),
                    receiver: receiver.take(),
                    cancellation: cancellation.take(),
                    pending: pending.take(),
                    guidance: std::mem::take(&mut pending_guidance),
                    active_context: active_context.take(),
                    tools: tools.clone(),
                    undo_writer: undo_writer.clone(),
                    approvals: std::mem::replace(
                        &mut approvals,
                        orbit_tools::approval::channel().1,
                    ),
                    instructions: instructions.clone(),
                    skills: skills.clone(),
                    memory_scope: memory_scope.clone(),
                    auto_compact,
                    max_turns,
                    max_tool_calls,
                    compaction: compaction.clone(),
                    last_publish: std::time::Instant::now(),
                    mcp_statuses: mcp_statuses.clone(),
                    model_options: model_options.clone(),
                },
            );
        };
    }

    let result: Result<()> = async {
        let mut dirty = true;
        let mut completion_key = None;

        loop {
            if let Some(ids) = state.restore_selection.take() {
                restore_queue.extend(ids);
            }
            if state.resume_request.is_none() && resume_task.is_none() {
                state.resume_request = restore_queue.pop_front();
            }
            if let Some(run) = background_runs.remove(session.id()) {
                task = run.task;
                local_task = run.local_task;
                local_cancellation = run.local_cancellation;
                history_replay = run.history_replay;
                move_notice = run.move_notice;
                marketplace_task = run.marketplace_task;
                model_refresh = run.model_refresh;
                retry_submit = run.retry_submit;
                refresh_before_retry = run.refresh_before_retry;
                receiver = run.receiver;
                cancellation = run.cancellation;
                pending = run.pending;
                pending_guidance = run.guidance;
                active_context = run.active_context;
                tools = run.tools;
                undo_writer = run.undo_writer;
                approvals = run.approvals;
                instructions = run.instructions;
                skills = run.skills;
                memory_scope = run.memory_scope;
                auto_compact = run.auto_compact;
                max_turns = run.max_turns;
                max_tool_calls = run.max_tool_calls;
                compaction = run.compaction;
                mcp_statuses = run.mcp_statuses;
                model_options = run.model_options;
                bound_session = session.id().to_owned();
                session_sync.reset(session.id());
            }
            if state.resume_request.is_none() && resume_task.is_none() && history_replay.is_none() {
                for error in restore_errors.drain(..) {
                    state.transcript.push(TranscriptItem::CommandError(error));
                    dirty = true;
                }
            }
            // A peer can move this session while this window remains open.
            // Rebind the actual runtime before accepting the next input.
            if task.is_none() && local_task.is_none() && resume_task.is_none() && history_replay.is_none() {
                if retry_submit || std::mem::take(&mut refresh_before_retry) {
                    session_sync.reset(session.id());
                    session_sync.poll(&session.store, &mut session.snapshot, &mut state, &mut history).await?;
                    restore_model_profile(&mut state, &mut model, &model_options, &session);
                }
                let tab_workspace = std::path::Path::new(&session.snapshot.session.workspace);
                if bound_session != session.id() || session.audit_workspace.as_ref().is_none_or(|workspace| workspace.root() != tab_workspace) {
                    let prep = crate::prepare_tui_resources(options, tab_workspace, &session).await?;
                    workspace::bind(&prep, &mut session, &mut state, &mut model, &mut history).await?;
                    bound_session = session.id().to_owned();
                    tools = prep.tools;
                    instructions = prep.budget.instructions;
                    skills = prep.budget.skills;
                    undo_writer = prep.undo_writer;
                    approvals = prep.approvals;
                    mcp_statuses = prep.budget.mcp_statuses;
                    model_options = prep.budget.model_options;
                    memory_scope = prep.budget.memory_scope;
                    auto_compact = prep.budget.auto_compact;
                    max_turns = prep.budget.max_turns;
                    max_tool_calls = prep.budget.max_tool_calls;
                    compaction = prep.budget.compaction;
                    refresh_context_usage(&mut state, &history, &tools);
                    dirty = true;
                }
            }
            if model_refresh.as_ref().is_some_and(|task| task.is_finished()) {
                let result = model_refresh.take().expect("model refresh task").await;
                background::model_refresh_result(result, &mut state, &mut model_options);
                dirty = true;
            }
            let key = (session.id().to_owned(), state.input.text.clone());
            if completion_key.as_ref() != Some(&key) {
                state.stored_completions = completion::stored(&state.input.text, &session).await.unwrap_or_default();
                completion_key = Some(key);
            }

            if state.compact_after_model_switch && task.is_none() && local_task.is_none() && resume_task.is_none() && history_replay.is_none() {
                state.compact_after_model_switch = false;
                refresh_context_usage(&mut state, &history, &tools);

                if needs_model_switch_compaction(&state) {
                    let target_context = state.context_tokens;
                    let started = start_agent_run("/compact".into(), Vec::new(), true,
                        &mut state, &mut session, &history, &model, &tools,
                        target_context,
                        auto_compact, max_turns, max_tool_calls, &compaction).await;

                    match started {
                        Ok(started) => { receiver=Some(started.receiver); cancellation=Some(started.cancellation); task=Some(started.task); }
                        Err(error) if error.is::<crate::session::SessionRefreshRequired>() => {
                            refresh_before_retry = true;
                            state.compact_after_model_switch = true;
                            continue;
                        }
                        Err(error) => state.transcript.push(TranscriptItem::CommandError(error.to_string())),
                    }

                    dirty = true;
                    frames.request(time::Instant::now());
                }
            }

            if std::mem::take(&mut state.new_tab_request) {
                if let Some(task) = resume_task.take() { task.abort(); }
                if history_replay.is_none() { state.resume_loading = None; }
                park_run!();
                if let Err(error) = new_tab(
                    &mut tabs, &mut session, &mut state, &mut model,
                    &mut history, CommandContext {
                        tools: &tools,
                        instructions: &instructions,
                        skills: &skills,
                        mcp_statuses: &mcp_statuses,
                        model_options: &model_options,
                    },
                ).await {
                    state.transcript.push(TranscriptItem::CommandError(error.to_string()));
                }

                dirty = true;
                frames.request(time::Instant::now());
                continue;
            }

            if let Some(id) = state.close_tab_request.take() {
                if id != session.id() {
                    tabs.close_parked(&id);
                } else {
                    if let Some(task) = resume_task.take() { task.abort(); }
                    if history_replay.is_none() { state.resume_loading = None; }
                    park_run!();
                    if !tabs.close_active(&mut session, &mut state, &mut model, &mut history) {
                        match new_tab(&mut tabs, &mut session, &mut state, &mut model, &mut history,
                            CommandContext { tools: &tools, instructions: &instructions, skills: &skills,
                                mcp_statuses: &mcp_statuses, model_options: &model_options }).await {
                            Ok(()) => { tabs.close_parked(&id); }
                            Err(error) => state.transcript.push(TranscriptItem::CommandError(error.to_string())),
                        }
                    }
                }
                for id in tabs.prune_closed(&background_runs) { background_runs.remove(&id); }

                state.tab_hover = None;
                dirty = true;
                frames.request(time::Instant::now());
                continue;
            }

            if let Some(id) = state.delete_request.take() {
                // Refuse to delete the currently active session.
                if id == session.id() {
                    if let Some(picker) = &mut state.picker {
                        picker.error = Some("无法删除当前正在使用的会话。".into());
                    }
                } else {
                    match session.store.delete_session(&id).await {
                        Ok(_) => {
                            // Remove the deleted session from the picker list so the
                            // UI reflects the change immediately without a reload.
                            if let Some(picker) = &mut state.picker {
                                picker.remove_session(&id);
                            }
                        }
                        Err(error) => {
                            if let Some(picker) = &mut state.picker {
                                picker.error = Some(format!("删除失败：{error}"));
                            }
                        }
                    }
                }
                dirty = true;
                frames.request(time::Instant::now());
            }

            if let Some(id) = state.resume_request.take() {
                if let Some(task) = resume_task.take() { task.abort(); }
                if history_replay.is_none() { state.resume_loading = None; }
                if id != session.id() && tabs.contains(&id) { park_run!(); }
                if id == session.id() {
                    state.picker = None;
                } else if tabs.restore(&id, &mut session, &mut state, &mut model, &mut history) {
                    state.picker = None;
                    session_sync.reset(session.id());
                } else {
                    state.resume_loading = Some("正在读取会话…  Esc 取消".into());

                    if let Some(picker) = &mut state.picker { picker.error = None; }

                    let store = session.store.clone();
                    let project = session.snapshot.session.project_id.clone();
                    let instructions = instructions.clone();
                    let skills = skills.clone();
                    resume_task = Some(tokio::spawn(load_resume(store, project, id, instructions, skills)));
                }

                dirty = true;
                frames.request(time::Instant::now());
                continue;
            }

            select! {
                event = async {
                    if retry_submit {
                        Some(Ok(Event::Key(crossterm::event::KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))))
                    } else {
                        events.next().await
                    }
                } => {
                    retry_submit = false;
                    let Some(event) = event else { break };
                    let mut event = event?;
                    if !wheel.accept(&event, std::time::Instant::now()) { continue; }

                    if state.restore_picker.is_some() && restore::handle(&mut state, &event) {
                        dirty = true;
                        frames.request(time::Instant::now());
                        continue;
                    }

                    if update_tab_hover(&event, &mut state, &tabs) {
                        dirty = true;
                        frames.request(time::Instant::now());
                    }

                    if let Some(action) = tab_target(&event, &state, &tabs)
                    {
                        match action {
                            TabAction::Select(id) => state.resume_request = Some(id),
                            TabAction::New => state.new_tab_request = true,
                            TabAction::Close(id) => state.close_tab_request = Some(id),
                        }

                        dirty = true;
                        frames.request(time::Instant::now());
                        continue;
                    }
                    if resume_task.is_some() || history_replay.is_some() {
                        match &event {
                            Event::Key(key) if key.kind != KeyEventKind::Release && key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) => break,
                            Event::Key(key) if key.kind != KeyEventKind::Release && key.code == KeyCode::Esc && resume_task.is_some() => {
                                if let Some(task) = resume_task.take() { task.abort(); }

                                state.resume_loading = None;
                            }
                            Event::Key(key) if key.code == KeyCode::PageUp => state.scroll_up(10),
                            Event::Key(key) if key.code == KeyCode::PageDown => state.scroll_down(10),
                            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollUp => state.scroll_up(3),
                            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollDown => state.scroll_down(3),
                            Event::Resize(_, _) => {},
                            _ => continue,
                        }

                        dirty = true;
                        frames.request(time::Instant::now());
                        continue;
                    }

                    // Handle the overlay before any obscured transcript hit targets.
                    if state.picker.is_none() && state.model_picker.is_none()
                        && state.marketplace_dialog.is_none() && !state.show_help
                        && state.approval.is_none()
                        && let Event::Mouse(mouse) = &event
                        && state.menu_area.contains((mouse.column, mouse.row).into())
                        && !state.menu().is_empty()
                    {
                        let previous = state.menu_index;
                        completion::select_with_mouse(&mut state, mouse);
                        let clicked = mouse.kind == MouseEventKind::Down(MouseButton::Left)
                            && state.menu_rows.iter().any(|(area, _)| area.contains((mouse.column, mouse.row).into()));
                        if state.menu_index != previous || mouse.kind != MouseEventKind::Moved {
                            dirty = true;
                            frames.request(time::Instant::now());
                        }
                        if clicked {
                            let candidate = &state.menu()[state.menu_index];
                            let code = if candidate.name.starts_with("/cd ") { KeyCode::Tab } else { KeyCode::Enter };
                            event = Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE));
                        } else {
                            continue;
                        }
                    }

                    if handle_list_hover(&mut state, &event) {
                        dirty = true;
                        frames.request(time::Instant::now());
                        continue;
                    }
                    let context_hover_changed = !state.selection.dragging()
                        && matches!(render::handle_context_hover(&mut state, &event), Some(true));
                    let link_hover_changed = !state.selection.dragging()
                        && !approval::file_modal(&state)
                        && !state.show_help
                        && state.picker.is_none()
                        && state.model_picker.is_none()
                        && state.marketplace_dialog.is_none()
                        && matches!(render::handle_link_hover(&mut state, &event), Some(true));
                    let transcript_hover_changed = !state.selection.dragging()
                        && !approval::file_modal(&state)
                        && !state.show_help
                        && state.picker.is_none()
                        && state.model_picker.is_none()
                        && state.marketplace_dialog.is_none()
                        && matches!(render::handle_transcript_hover(&mut state, &event), Some(true));

                    if !state.selection.dragging() && !approval::file_modal(&state) && !state.show_help && state.picker.is_none() && state.model_picker.is_none() && state.marketplace_dialog.is_none()
                        && let Some(changed) = navigation::handle(&mut state, &event) {
                        if changed || context_hover_changed || link_hover_changed || transcript_hover_changed { dirty = true; frames.request(time::Instant::now()); }

                        continue;
                    }
                    if context_hover_changed || link_hover_changed || transcript_hover_changed { dirty = true; frames.request(time::Instant::now()); continue; }
                    // Unused pointer motion must not trigger full-frame redraws.
                    if matches!(&event, Event::Mouse(mouse) if mouse.kind == MouseEventKind::Moved) { continue; }

                    dirty = true;
                    frames.request(time::Instant::now());

                    if state.show_help || state.picker.is_some() || state.model_picker.is_some() || state.marketplace_dialog.is_some() {
                        if let Some(id) = dialogs::handle(&mut state, event) {
                            state.resume_request = Some(id);
                        }

                        if let Some(name) = state.chosen_model.take()
                            && let Err(error) = select_model(
                                &name,
                                state.chosen_effort.take(),
                                &mut state,
                                &mut model,
                                &model_options,
                                &mut session,
                            ).await
                        {
                            state.transcript.push(TranscriptItem::Error(error.to_string()));
                        }

                        if let Some(source) = state.pending_marketplace_source.take() {
                            state.transcript.push(TranscriptItem::Notice("正在下载并检查插件市场…".into()));
                            marketplace_task = Some(tokio::spawn(crate::marketplaces::add_current(source)));
                        }

                        if let Some(change) = state.pending_project_plugin_change.take() {
                            match change {
                                dialogs::ProjectPluginChange::Enable(plugin) => {
                                    let name = plugin.plugin_name.clone();

                                    match session.store.enable_project_plugin(plugin).await {
                                        Ok(_) => state.transcript.push(TranscriptItem::CommandResult(format!(
                                            "已为当前项目启用 {name}。插件包保存在 Orbit 数据目录；通过校验的 Skill 会在下次启动时按需可用，命令、Hook 与 MCP 不会自动执行。"
                                        ))),
                                        Err(error) => state.transcript.push(TranscriptItem::CommandError(format!(
                                            "无法启用插件：{error}"
                                        ))),
                                    }
                                }
                                dialogs::ProjectPluginChange::Disable {
                                    project_id,
                                    marketplace_source,
                                    plugin_name,
                                } => match session
                                    .store
                                    .disable_project_plugin(
                                        &project_id,
                                        &marketplace_source,
                                        &plugin_name,
                                    )
                                    .await
                                {
                                    Ok(true) => state.transcript.push(TranscriptItem::CommandResult(format!(
                                        "已在当前项目停用 {plugin_name}。市场缓存仍保留在 Orbit 数据目录。"
                                    ))),
                                    Ok(false) => state.transcript.push(TranscriptItem::CommandError(format!(
                                        "插件 {plugin_name} 未处于启用状态。"
                                    ))),
                                    Err(error) => state.transcript.push(TranscriptItem::CommandError(format!(
                                        "无法停用插件：{error}"
                                    ))),
                                },
                            }
                        }

                        continue;
                    }


                    if let Event::Mouse(mouse) = &event
                        && mouse.kind == crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
                        && mouse.modifiers.contains(KeyModifiers::CONTROL) {
                        let offset = if state.follow_bottom {
                            state.max_scroll
                        } else {
                            state.max_scroll.saturating_sub(state.scroll)
                        };

                        if let Some(url) = state
                            .transcript_layout
                            .link_at(state.selection.viewport, offset, (mouse.column, mouse.row))
                            .map(str::to_owned)
                        {
                            state.selection.clear();

                            if let Err(error) = open_link_in_browser(&url) {
                                state.transcript.push(TranscriptItem::Error(format!(
                                    "无法打开链接：{error}"
                                )));
                            }

                            continue;
                        }
                    }

                    if let Event::Mouse(mouse) = &event
                        && mouse.kind == crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
                        && focus_attachment_at(&mut state, mouse.column, mouse.row) {
                        state.selection.clear();
                        continue;
                    }
                    if let Some((x, y)) = state.selection.click(&event)
                        && toggle_transcript_at(&mut state, (x, y)) {
                        continue;
                    }

                    if let Some((x, y)) = state.selection.click(&event)
                        && state.input_area.contains((x, y).into()) {
                        state.attachment_cursor = None;
                        state.input.move_to((x - state.input_area.x) as usize,
                            (y - state.input_area.y) as usize + state.input_scroll,
                            state.input_width);
                    }

                    if pending.is_some() {
                        match approval::handle(&mut state, event.clone()) {
                            approval::Action::Approve => {
                                if let Some(request) = pending.take() {
                                    let _ = request.decision.send(true);
                                }

                                state.approval = None;
                                state.resume_clock();
                                continue;
                            }
                            approval::Action::AlwaysAllow => {
                                if let Some(request) = pending.take() {
                                    match &request.preview {
                                        orbit_tools::approval::ApprovalPreview::Write(p) => {
                                            if let Some(key) = orbit_tools::approval::file_grant_key(&p.path, std::path::Path::new(&session.snapshot.session.workspace)) {
                                                session.store.grant_tui_file(session.id(), &key).await?;
                                                state.session_allowed_files.insert(key);
                                            }
                                        }
                                        orbit_tools::approval::ApprovalPreview::Delete(p) => {
                                            if let Some(key) = orbit_tools::approval::file_grant_key(&p.path, std::path::Path::new(&session.snapshot.session.workspace)) {
                                                session.store.grant_tui_file(session.id(), &key).await?;
                                                state.session_allowed_files.insert(key);
                                            }
                                        }
                                        orbit_tools::approval::ApprovalPreview::Command(cmd) => {
                                            if let Some(git) = &cmd.git {
                                                match git.kind {
                                                    orbit_runtime::git::GitOperationKind::LocalWrite { .. }
                                                    | orbit_runtime::git::GitOperationKind::RemoteWrite { .. }
                                                    | orbit_runtime::git::GitOperationKind::ReadOnlyQuery { .. } => {
                                                        if let Some(key) = cmd.git_grant_key(std::path::Path::new(&session.snapshot.session.workspace)) { state.session_git_commands.insert(key); }
                                                    }
                                                    _ => {}
                                                }
                                            } else {
                                                state.session_commands.insert(cmd.request_grant_key(std::path::Path::new(&session.snapshot.session.workspace)));
                                            }
                                        }
                                        _ => {}
                                    }
                                    let _ = request.decision.send(true);
                                }

                                state.approval = None;
                                state.resume_clock();
                                continue;
                            }
                            approval::Action::Deny => {
                                if let Some(request) = pending.take() {
                                    let _ = request.decision.send(false);
                                }

                                state.approval = None;
                                state.resume_clock();
                                continue;
                            }
                            approval::Action::Cancel => {
                                if let Some(request) = pending.take() {
                                    let _ = request.decision.send(false);
                                }

                                state.approval = None;
                                state.resume_clock();
                                state.cancel();

                                if let Some(token) = &cancellation {
                                    token.cancel();
                                }

                                if let Some(token) = &local_cancellation {
                                    token.cancel();
                                }
                                continue;
                            }
                            approval::Action::None => {}
                        }
                    }

                    if state.selection.handle(&event)? {
                        if state.selection.transcript_drag() {
                            state.follow_bottom = false;
                        }

                        terminal::draw_synchronized(&mut terminal, |frame| {
                            if !state.selection.draw_frozen(frame) {
                                render::draw(frame, &mut state);
                                state.selection.paint(frame);
                            }
                        })?;
                        terminal::anchor_cursor(&mut terminal, state.ime_cursor)?;
                        dirty = false;
                        frames.drawn(time::Instant::now());
                        continue;
                    }
                    if matches!(&event, Event::Mouse(mouse) if matches!(mouse.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown)) {
                        if state.selection.dragging() {
                            keyboard_action(
                                &mut state,
                                event,
                                task.is_some() || local_task.is_some(),
                            );
                        } else if pending.is_some() {
                            approval::handle(&mut state, event);
                        } else {
                            keyboard_action(
                                &mut state,
                                event,
                                task.is_some() || local_task.is_some(),
                            );
                        }

                        continue;
                    }
                    if matches!(&event, Event::Key(key) if key.code == KeyCode::BackTab && key.kind != KeyEventKind::Release) {
                        session_sync::sync_mode(&session.store, session.id(), &mut state).await?;
                        let mode = state.permission_mode.next();
                        session.store.set_tui_mode(session.id(), mode.label()).await?;
                        state.permission_mode = mode;
                        continue;
                    }

                    match keyboard_action(&mut state, event, task.is_some() || local_task.is_some()) {
                        Action::Quit => break,
                        Action::Redraw => terminal.clear()?,

                        Action::Cancel => {
                            state.cancel();

                            if let Some(token) = &cancellation { token.cancel(); }

                            if let Some(token) = &local_cancellation { token.cancel(); }
                        }

                        Action::Steer(guidance) => {
                            if local_task.is_some() {
                                state.transcript.push(TranscriptItem::CommandError(
                                    "正在等待回退操作完成或取消。".into(),
                                ));
                                continue;
                            }

                            pending_guidance.push(guidance);
                            state.cancel();

                            if let Some(token) = &cancellation { token.cancel(); }
                        }

                        Action::AttachPaths(paths) => {
                            if let Err(error) = attach_paths(&mut state, &session, paths) {
                                state.transcript.push(TranscriptItem::Error(error.to_string()));
                            }
                        }
                        Action::PasteClipboard => {
                            if let Err(error) = paste_clipboard(&mut state, &session).await {
                                state.transcript.push(TranscriptItem::Error(error.to_string()));
                            }
                        }

                        Action::Submit(prompt) => {
                            if prompt.trim() == "/new" {
                                state.new_tab_request = true;
                                continue;
                            }
                            if prompt.trim() == "/close" {
                                state.close_tab_request = Some(session.id().to_owned());
                                continue;
                            }
                            if prompt.trim() == "/restore" {
                                match restore::RestorePicker::load(&session.store, tabs.restore_snapshot()).await {
                                    Ok(picker) => { state.restore_picker = Some(picker); state.input.set("/restore ".into()); }
                                    Err(error) => state.transcript.push(TranscriptItem::CommandError(error.to_string())),
                                }
                                continue;
                            }
                            if local_task.is_some() {
                                state.transcript.push(TranscriptItem::CommandError(
                                    "正在等待回退操作完成或取消。".into(),
                                ));
                                continue;
                            }

                            let compacting = prompt.trim() == "/compact";

                            if prompt.starts_with('/') {
                                let command = prompt
                                    .split_once(char::is_whitespace)
                                    .map_or(prompt.as_str(), |(command, _)| command);

                                if command == "/cd" {
                                    if task.is_some() || marketplace_task.is_some() || tabs.has_drafts() || !state.attachments.is_empty() {
                                        state.transcript.push(TranscriptItem::CommandError("请先结束运行，并处理各标签中未发送的草稿和附件。".into()));
                                        continue;
                                    }

                                    let argument = prompt.split_once(char::is_whitespace).map_or("", |(_, text)| text.trim());
                                    match directory::resolve(argument, std::path::Path::new(&state.workspace), options.home.as_deref()) {
                                        Ok(path) if path == std::path::Path::new(&state.workspace) => {}
                                        Ok(path) => match crate::prepare_tui_resources(options, &path, &session).await {
                                            Ok(prepared) => {
                                                match session.move_to(prepared.workspace.clone(), prepared.command_limits.clone()).await {
                                                    Ok(()) => {
                                                        state.workspace = session.snapshot.session.workspace.clone();
                                                        workspace::bind(&prepared, &mut session, &mut state, &mut model, &mut history).await?;
                                                        tools = prepared.tools;
                                                        auto_compact = prepared.budget.auto_compact;
                                                        max_turns = prepared.budget.max_turns;
                                                        max_tool_calls = prepared.budget.max_tool_calls;
                                                        compaction = prepared.budget.compaction;
                                                        instructions = prepared.budget.instructions;
                                                        skills = prepared.budget.skills;
                                                        undo_writer = prepared.undo_writer;
                                                        approvals = prepared.approvals;
                                                        mcp_statuses = prepared.budget.mcp_statuses;
                                                        model_options = prepared.budget.model_options;
                                                        memory_scope = prepared.budget.memory_scope;
                                                        refresh_context_usage(&mut state, &history, &tools);
                                                        state.transcript.push(TranscriptItem::CommandResult(format!("已切换至工作区 {}", state.workspace)));
                                                        dirty = true;
                                                        frames.request(time::Instant::now());
                                                    }
                                                    Err(error) => state.transcript.push(TranscriptItem::CommandError(format!("无法移动会话：{error:#}"))),
                                                }
                                            }
                                            Err(error) => state.transcript.push(TranscriptItem::CommandError(format!("无法切换目录：{error:#}"))),
                                        },
                                        Err(error) => state.transcript.push(TranscriptItem::CommandError(format!("{error:#}"))),
                                    }
                                    continue;
                                }
                                if prompt.trim() == "/model refresh" {
                                    if model_refresh.is_none() {
                                        let config_path = options.config.clone();
                                        let auth_directory = options.auth_directory.clone();
                                        model_refresh = Some(tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
                                            let mut config = crate::config::FileConfig::load(&config_path, &auth_directory)?;
                                            let diagnostics = crate::account_models::refresh(&mut config, true).await;
                                            Ok((config.model_options()?, diagnostics))
                                        })));
                                    }
                                    continue;
                                }
                                if command == "/connect" {
                                    if task.is_some() || marketplace_task.is_some() {
                                        state.transcript.push(TranscriptItem::CommandError("请等待当前操作结束后再连接账号。".into()));
                                        continue;
                                    }

                                    match connect::run(&mut terminal, &mut events, options).await {
                                        Ok(Some(models)) => {
                                            model_options = models;
                                            state.model_completions = model_options.iter().map(ModelOption::qualified_name).collect();
                                            let current = model_options.iter().find(|choice| choice.qualified_name() == state.model)
                                                .or_else(|| model_options.first()).map(ModelOption::qualified_name);
                                            if let Some(current) = current {
                                                select_model(&current, None, &mut state, &mut model, &model_options, &mut session).await?;
                                            }
                                            state.model_picker = Some(dialogs::ModelPicker::new(model_options.clone(), state.model.clone(), state.reasoning_effort));
                                        }
                                        Ok(None) => {}
                                        Err(error) => state.transcript.push(TranscriptItem::CommandError(format!("连接账号失败：{error:#}"))),
                                    }
                                    state.menu_dismissed = true;
                                    state.selection.clear();
                                    continue;
                                }
                                if command == "/editor" {
                                    if task.is_some() {
                                        state.transcript.push(TranscriptItem::CommandError("请等待当前响应结束后再打开编辑器。".into()));
                                        continue;
                                    }

                                    let draft = prompt.split_once(char::is_whitespace).map_or("", |(_, text)| text).to_owned();
                                    state.input.set(draft.clone());
                                    drop(events);
                                    terminal::suspend();
                                    let edited = editor.edit(&draft, std::path::Path::new(&state.workspace)).await;
                                    terminal::resume(&mut terminal)?;
                                    events = EventStream::new();

                                    match edited {
                                        Ok(text) => state.restore_editor_draft(text),
                                        Err(error) => state.transcript.push(TranscriptItem::CommandError(format!("{error:#}"))),
                                    }
                                    state.selection.clear();
                                    state.menu_dismissed = true;
                                    continue;
                                }
                                if command == "/undo" {
                                    if task.is_some() {
                                        state.transcript.push(TranscriptItem::CommandError(
                                            "Agent 运行中不能回退文件；请等待当前响应结束。".into(),
                                        ));
                                        continue;
                                    }

                                    let argument = prompt
                                        .split_once(char::is_whitespace)
                                        .map_or("", |(_, argument)| argument.trim());
                                    let changes = session.store.workspace_changes(session.id()).await?;
                                    let change = changes
                                        .iter()
                                        .rev()
                                        .find(|record| {
                                            !record.undone
                                                && (argument.is_empty()
                                                    || record.change.path == argument)
                                        })
                                        .cloned();
                                    let Some(change) = change else {
                                        state.transcript.push(TranscriptItem::CommandError(if argument.is_empty() {
                                            "当前会话没有可回退的 Orbit 文件写入。".into()
                                        } else {
                                            format!("当前会话没有 {argument} 的 Orbit 写入检查点。")
                                        }));
                                        continue;
                                    };

                                    let token = CancellationToken::new();
                                    let writer = undo_writer.clone();
                                    let task_token = token.clone();
                                    let store = session.store.clone();
                                    let session_id = session.id().to_owned();
                                    local_task = Some(tokio::spawn(async move {
                                        let result = writer.restore(change.change.clone(), task_token).await?;
                                        store
                                            .record_workspace_undo(&session_id, &change)
                                            .await
                                            .map_err(|error| orbit_agent::tool::ToolError::ExecutionFailed(format!(
                                                "undo completed but its audit record could not be saved: {error}"
                                            )))?;

                                        Ok(result)
                                    }));
                                    local_cancellation = Some(token);
                                    continue;
                                }
                                if compacting && task.is_some() {
                                    state.transcript.push(TranscriptItem::CommandError(
                                        "运行中不能压缩上下文；请先按 Ctrl+C 取消当前响应。".into(),
                                    ));
                                    continue;
                                }
                                if !compacting {
                                    if task.is_some()
                                        && !commands::available_while_running(command)
                                    {
                                        state.transcript.push(TranscriptItem::CommandError(
                                            "此命令需要当前响应结束；可先切换标签，或按 Ctrl+C 取消响应。".into(),
                                        ));
                                        continue;
                                    }

                                    match run_command(&prompt, &mut state, &mut model, &mut history, &mut session, CommandContext { tools: &tools, instructions: &instructions, skills: &skills, mcp_statuses: &mcp_statuses, model_options: &model_options }).await {
                                        Ok(true) => break,
                                        Ok(false) => refresh_context_usage(&mut state, &history, &tools),
                                        Err(error) => state.transcript.push(TranscriptItem::Error(error.to_string())),
                                    }

                                    continue;
                                }
                            }

                            let attachments = if compacting {
                                Vec::new()
                            } else {
                                state.attachment_cursor = None;
                                std::mem::take(&mut state.attachments)
                            };

                            active_context = (!compacting).then(|| {
                                ChatMessage::user_with_attachments(prompt.clone(), attachments.clone())
                            });
                            let active_context_tokens = state.context_tokens;
                            let retry_prompt = prompt.clone();
                            let retry_attachments = attachments.clone();

                            let started = start_agent_run(
                                prompt,
                                attachments,
                                compacting,
                                &mut state,
                                &mut session,
                                &history,
                                &model,
                                &tools,
                                active_context_tokens,
                                auto_compact,
                                max_turns,
                                max_tool_calls,
                                &compaction,
                            )
                            .await;
                            let started = match started {
                                Ok(started) => started,
                                Err(error) => {
                                    state.input.set(retry_prompt);
                                    state.attachments = retry_attachments;
                                    active_context = None;
                                    if error.is::<crate::session::SessionRefreshRequired>() {
                                        retry_submit = true;
                                    } else {
                                        state.transcript.push(TranscriptItem::CommandError(error.to_string()));
                                    }
                                    continue;
                                }
                            };
                            receiver = Some(started.receiver);
                            cancellation = Some(started.cancellation);
                            task = Some(started.task);
                        }

                        Action::None => {}
                    }
                }
                Some(request) = approvals.recv() => {
                    if request.decision.is_closed() { continue; }
                    session_sync::sync_mode(&session.store, session.id(), &mut state).await?;
                    queue_approval(request, &mut state, &session, &mut pending);
                    dirty = true;
                }
                event = async { receiver.as_mut().unwrap().recv().await }, if receiver.is_some() => {
                    dirty = true;

                    match event {
                        Some(event) => { apply_live_event(event, &mut state); },
                        None => receiver = None,
                    }
                }
                _ = command_ticks.tick(), if state.status == app::RunStatus::Running => {
                    if let Some(TranscriptItem::Tool(row)) = state.transcript.last_mut()
                        && row.name == "run_command"
                        && row.status == app::ToolStatus::Running
                        && let Ok(dir) = session.command_output.read()
                        && let Some(stdout) = read_active_command_stdout(&dir)
                        && !stdout.is_empty()
                    {
                        row.output = serde_json::json!({
                            "stdout": stdout,
                            "running": true,
                        }).to_string();
                        dirty = true;
                        frames.request(time::Instant::now());
                    }
                }
                result = async { task.as_mut().unwrap().await }, if task.is_some() => {
                    dirty = true;
                    task = None;
                    pending = None;

                    if state.approval.take().is_some() { state.selection = Default::default(); }

                    // Drain the completed run before accepting another prompt or clearing history.
                    if let Some(rx) = receiver.as_mut() {
                        while let Ok(event) = rx.try_recv() { apply_live_event(event, &mut state); }
                    }

                    let (status, final_event, completed_history) = background::outcome(result, cancellation.as_ref());

                    let finished_run = session.active_run_id();
                    session.finish(status, completed_history.clone()).await?;
                    session_sync.reset(session.id());

                    if completed_history.is_some() || session.snapshot.recovery.is_some() {
                        history = session.snapshot.history.clone();
                        active_context = None;
                        crate::instructions::inject(&mut history, &instructions);
                        skills.inject(&mut history);
                        crate::project_state::inject(
                            &mut history,
                            &session.store,
                            &session.snapshot.session.project_id,
                            session.id(),
                        )
                        .await?;
                    }

                    state.apply(final_event);
                    refresh_context_usage(&mut state, &history, &tools);
                    session_sync::publish(&session, &state, finished_run).await?;
                    task = None;
                    receiver = None;
                    cancellation = None;

                    if !pending_guidance.is_empty() {
                        let guidance = pending_guidance.join("\n\n");
                        pending_guidance.clear();
                        let steering_history = steering_history(&history, active_context.take());
                        active_context = Some(ChatMessage::user(guidance.clone()));

                        let context_tokens = state.context_tokens;
                        let started = start_agent_run(
                            guidance.clone(),
                            Vec::new(),
                            false,
                            &mut state,
                            &mut session,
                            &steering_history,
                            &model,
                            &tools,
                            context_tokens,
                            auto_compact,
                            max_turns,
                            max_tool_calls,
                            &compaction,
                        )
                        .await;
                        let started = match started {
                            Ok(started) => started,
                            Err(error) => {
                                state.input.set(guidance);
                                active_context = None;
                                if error.is::<crate::session::SessionRefreshRequired>() {
                                    retry_submit = true;
                                } else {
                                    state.transcript.push(TranscriptItem::CommandError(error.to_string()));
                                }
                                continue;
                            }
                        };
                        receiver = Some(started.receiver);
                        cancellation = Some(started.cancellation);
                        task = Some(started.task);
                    } else {
                        active_context = None;
                    }
                }
                result = async { resume_task.as_mut().unwrap().await }, if resume_task.is_some() => {
                    resume_task = None;
                    state.resume_loading = None;

                    let result = match result {
                        Ok(Ok(next)) => {
                            park_run!();
                            let empty_tab = tabs.empty_tab(&session, &state);
                            activate_resume(options, &mut tabs, &mut session, &mut state, &mut model, &mut history, next)
                                .await
                                .inspect(|_| {
                                    if let Some(id) = empty_tab { tabs.close_parked(&id); }
                                })
                        }
                        Ok(Err(error)) => Err(error),
                        Err(error) => Err(error.into()),
                    };

                    match result {
                        Ok(prep) => {
                            bound_session = session.id().to_owned();
                            tools = prep.tools;
                            instructions = prep.budget.instructions;
                            skills = prep.budget.skills;
                            undo_writer = prep.undo_writer;
                            approvals = prep.approvals;
                            mcp_statuses = prep.budget.mcp_statuses;
                            model_options = prep.budget.model_options;
                            memory_scope = prep.budget.memory_scope;
                            auto_compact = prep.budget.auto_compact;
                            max_turns = prep.budget.max_turns;
                            max_tool_calls = prep.budget.max_tool_calls;
                            compaction = prep.budget.compaction;
                            restore_model_profile(&mut state, &mut model, &model_options, &session);
                            history_replay = Some(crate::session::HistoryReplay::default());
                        }
                        Err(error) => {
                            let message = format!("恢复会话失败：{error:#}");

                            if let Some(picker) = &mut state.picker {
                                picker.error = Some(message);
                            } else {
                                restore_errors.push(message);
                            }
                        }
                    }

                    dirty = true;
                    frames.request(time::Instant::now());
                }
                result = async { local_task.as_mut().unwrap().await }, if local_task.is_some() => {
                    dirty = true;
                    local_task = None;
                    local_cancellation = None;

                    state.transcript.push(background::local_result(result));
                }
                result = async { marketplace_task.as_mut().unwrap().await }, if marketplace_task.is_some() => {
                    dirty = true;
                    marketplace_task = None;

                    state.transcript.push(background::marketplace_result(result));
                }
                _ = time::sleep_until(frames.deadline()), if frames.pending() => {
                    tabs.sync_ui(&mut state, &session);
                    terminal::draw_synchronized(&mut terminal, |frame| {
                        render::draw(frame, &mut state);
                        state.selection.paint(frame);
                    })?;
                    terminal::anchor_cursor(&mut terminal, state.ime_cursor)?;
                    frames.drawn(time::Instant::now());
                    dirty = false;
                }
                _ = ticks.tick() => {
                    dirty |= background::poll(&mut background_runs, &mut tabs, &mut session_sync).await?;
                    if state.thinking_hover_started.is_some_and(|started| started.elapsed() < Duration::from_millis(160)) { dirty = true; }
                    if branch_task.as_ref().is_some_and(|task| task.is_finished())
                        && let Ok((workspace, branch)) = branch_task.take().unwrap().await
                        && workspace == state.workspace
                    {
                        dirty |= state.branch_workspace != workspace || state.branch != branch;
                        state.branch_workspace = workspace;
                        state.branch = branch;
                    }
                    if branch_task.is_none() && (branch_workspace != state.workspace || branch_checked.elapsed() >= Duration::from_secs(2)) {
                        branch_workspace = state.workspace.clone();
                        branch_checked = std::time::Instant::now();
                        let workspace = branch_workspace.clone();
                        branch_task = Some(tokio::spawn(async move {
                            let branch = project_label::branch(&workspace).await;
                            (workspace, branch)
                        }));
                    }
                    if sync_checked.elapsed() >= Duration::from_millis(200) {
                        sync_checked = std::time::Instant::now();
                        tabs.persist(&session).await?;
                        dirty |= session_sync::sync_mode(&session.store, session.id(), &mut state).await?;
                        if task.is_some() {
                            session.flush_events().await?;
                            session_sync::publish(&session, &state, session.active_run_id()).await?;
                            session_sync.reset(session.id());
                        } else if local_task.is_none() && resume_task.is_none() && history_replay.is_none()
                            && session_sync.poll(&session.store, &mut session.snapshot, &mut state, &mut history).await? {
                            let previous_profile = (state.model.clone(), state.reasoning_effort);
                            restore_model_profile(&mut state, &mut model, &model_options, &session);
                            if previous_profile != (state.model.clone(), state.reasoning_effort) {
                                refresh_context_usage(&mut state, &history, &tools);
                            }
                            dirty = true;
                        }
                        if task.is_none() && local_task.is_none() && history_replay.is_none() {
                            dirty |= session_sync::sync_runtime(&session.store, session.id(), &mut state).await?;
                        }
                        dirty |= tabs.synchronize_parked(&mut session_sync, &background_runs).await?;
                    }
                    dirty |= session.poll_title().await.unwrap_or(false);
                    dirty |= tabs.poll_titles().await;
                    dirty |= state.navigation.advance_preview();
                    if let Some(picker) = &mut state.picker {
                        dirty |= picker.advance_title_scroll();
                    }

                    if let Some((action, since)) = &state.tab_hover {
                        let elapsed = since.elapsed().as_millis();
                        let tick = match action {
                            TabAction::Select(_) => elapsed / 120,
                            _ => elapsed.min(240) / 40,
                        };

                        if tick != state.tab_hover_tick {
                            state.tab_hover_tick = tick;
                            dirty = true;
                        }
                    }

                    if let Some(replay) = &mut history_replay {
                        let done = replay.advance(&session.snapshot, &mut state, 2048, Duration::from_millis(4));

                        if done {
                            history_replay = None;
                            state.resume_loading = None;
                            session_sync::sync_mode(&session.store, session.id(), &mut state).await?;
                            if let Some(notice) = move_notice.take() {
                                state.transcript.push(TranscriptItem::CommandResult(notice));
                            }
                            refresh_context_usage(&mut state, &history, &tools);
                            session_sync::sync_runtime(&session.store, session.id(), &mut state).await?;
                        } else {
                            state.resume_loading = Some(format!("正在显示会话历史…  {}/{}", replay.completed_runs(), session.snapshot.runs.len()));
                        }

                        dirty = true;
                    }

                    let direction = state.selection.auto_scroll(std::time::Instant::now());

                    if direction != 0 {
                        if direction < 0 { state.scroll_up(2); } else { state.scroll_down(2); }

                        dirty = true;
                    }

                    if dirty || state.running() { frames.request(time::Instant::now()); }
                }
            }
        }

        Ok(())
    }.await;

    if let Some(task) = resume_task {
        task.abort();
    }

    drop(pending);

    if let Some(token) = cancellation {
        token.cancel();
    }

    if let Some(token) = local_cancellation {
        token.cancel();
    }

    if let Some(mut task) = task
        && time::timeout(Duration::from_millis(500), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
    }

    if let Some(mut task) = local_task
        && time::timeout(Duration::from_millis(500), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
    }

    if let Some(mut task) = marketplace_task
        && time::timeout(Duration::from_millis(500), &mut task)
            .await
            .is_err()
    {
        task.abort();
        let _ = task.await;
    }

    if let Some(rx) = receiver.as_mut() {
        while rx.try_recv().is_ok() {}
    }

    background::shutdown(&mut background_runs, &mut tabs).await?;
    tabs.close(&session).await?;
    let saved = session.close().await;

    result.and(saved)?;

    Ok(next_workspace.map(|resources| crate::PreparedTui { session, resources }))
}

fn queue_approval(
    request: orbit_tools::approval::ApprovalRequest,
    state: &mut UiState,
    session: &SessionState,
    pending: &mut Option<orbit_tools::approval::ApprovalRequest>,
) {
    if request.decision.is_closed() {
        return;
    }
    let auto_allowed = match &request.preview {
        orbit_tools::approval::ApprovalPreview::Write(p) => orbit_tools::approval::file_grant_key(
            &p.path,
            std::path::Path::new(&session.snapshot.session.workspace),
        )
        .is_some_and(|key| state.session_allowed_files.contains(&key)),
        orbit_tools::approval::ApprovalPreview::Delete(p) => orbit_tools::approval::file_grant_key(
            &p.path,
            std::path::Path::new(&session.snapshot.session.workspace),
        )
        .is_some_and(|key| state.session_allowed_files.contains(&key)),
        orbit_tools::approval::ApprovalPreview::Command(cmd) => {
            if let Some(git) = &cmd.git {
                match git.kind {
                    orbit_runtime::git::GitOperationKind::LocalWrite { .. }
                    | orbit_runtime::git::GitOperationKind::RemoteWrite { .. }
                    | orbit_runtime::git::GitOperationKind::ReadOnlyQuery { .. } => cmd
                        .git_grant_key(std::path::Path::new(&session.snapshot.session.workspace))
                        .is_some_and(|key| state.session_git_commands.contains(&key)),
                    _ => false,
                }
            } else {
                state.session_commands.contains(
                    &cmd.request_grant_key(std::path::Path::new(
                        &session.snapshot.session.workspace,
                    )),
                )
            }
        }
        _ => false,
    };
    if auto_allowed {
        let _ = request.decision.send(true);
        return;
    }
    if state.permission_mode.allows(
        &request.preview,
        std::path::Path::new(&session.snapshot.session.workspace),
    ) {
        let _ = request.decision.send(true);
        return;
    }

    state.selection = Default::default();
    state.approval = Some(request.preview.clone());
    state.approval_scroll = 0;
    state.approval_expanded = false;
    state.approval_shown = false;
    state.approval_choice = 0;
    state.approval_diff = None;
    *pending = Some(request);
    state.pause_clock();
}

fn apply_live_event(event: AgentEvent, state: &mut UiState) {
    // The joined task is authoritative: persist terminal status with its context atomically.
    if matches!(
        event,
        AgentEvent::Completed { .. } | AgentEvent::Failed { .. } | AgentEvent::Cancelled
    ) {
        return;
    }

    state.apply(event);
}

fn refresh_context_usage(state: &mut UiState, history: &[ChatMessage], tools: &ToolRegistry) {
    // While a request is live, ContextUsage/RequestMetrics describe the actual
    // model input. The completed-history buffer can still describe an older turn.
    if matches!(
        state.status,
        app::RunStatus::Running | app::RunStatus::Cancelling
    ) {
        return;
    }
    state.context_used_tokens =
        state
            .calibration()
            .estimate(orbit_agent::context::estimate_tokens(
                history,
                &tools.definitions(),
            ));
}

fn tool_context_tokens(history: &[ChatMessage], name: &str) -> usize {
    let ids: std::collections::HashSet<_> = history
        .iter()
        .flat_map(|message| match message {
            ChatMessage::Assistant { tool_calls, .. } => tool_calls.as_slice(),
            _ => &[],
        })
        .filter(|call| call.function.name == name)
        .map(|call| call.id.as_str())
        .collect();
    let messages: Vec<_> = history.iter().filter(|message| {
        matches!(message, ChatMessage::Tool { tool_call_id, .. } if ids.contains(tool_call_id.as_str()))
    }).cloned().collect();

    if messages.is_empty() {
        0
    } else {
        orbit_agent::context::estimate_tokens(&messages, &[])
    }
}

fn format_undo_result(result: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(result) else {
        return result.into();
    };

    let path = value["path"].as_str().unwrap_or("文件");

    if value["created_file_removed"].as_bool() == Some(true) {
        format!("已回退 {path}：已删除由 Orbit 创建的文件。")
    } else if value["restored"].as_bool() == Some(true) {
        format!("已回退 {path}：已恢复到 Orbit 写入前的内容。")
    } else {
        result.into()
    }
}

fn steering_history(
    history: &[ChatMessage],
    interrupted_prompt: Option<ChatMessage>,
) -> Vec<ChatMessage> {
    let mut history = history.to_vec();

    if let Some(prompt) = interrupted_prompt {
        history.push(prompt);
    }

    history
}

#[allow(clippy::too_many_arguments)]
async fn start_agent_run(
    prompt: String,
    attachments: Vec<Attachment>,
    compacting: bool,
    state: &mut UiState,
    session: &mut SessionState,
    history: &[ChatMessage],
    model: &ModelClient,
    tools: &ToolRegistry,
    context_tokens: usize,
    auto_compact: bool,
    max_turns: usize,
    max_tool_calls: usize,
    compaction: &orbit_agent::compaction::CompactionRuntime,
) -> Result<StartedRun> {
    let max_output_tokens = state.max_output_tokens;
    let mut messages = if compacting {
        compaction_source_history(session, history).await?
    } else {
        history.to_vec()
    };

    session
        .begin_with_title(prompt.clone(), state.model.clone(), model)
        .await?;
    let display_prompt = if attachments.is_empty() {
        prompt.clone()
    } else {
        let labels = attachments
            .iter()
            .map(|attachment| format!("[图片：{}]", attachment.name()))
            .collect::<Vec<_>>()
            .join(" ");

        if prompt.is_empty() {
            labels
        } else {
            format!("{prompt}\n{labels}")
        }
    };

    if compacting {
        state.begin_run();
    } else {
        state.submit(display_prompt);
    }

    if !compacting {
        messages.push(ChatMessage::user_with_attachments(prompt, attachments));
    }

    state.context_used_tokens =
        state
            .calibration()
            .estimate(orbit_agent::context::estimate_tokens(
                &messages,
                &tools.definitions(),
            ));
    let mut request = ChatRequest::new(messages);
    request.max_tokens = Some(max_output_tokens);
    request.reasoning_effort = model.config().reasoning_effort;
    let (sender, receiver) = mpsc::channel(256);
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    let tools = tools.clone();
    let backend = ModelClientBackend::new(model.clone());
    let files = orbit_agent::memory::FileActivity::from_events(
        session.snapshot.runs.iter().flat_map(|run| &run.events),
    );
    let config = AgentConfig {
        context_tokens,
        max_turns,
        max_tool_calls,
        auto_compact,
    };
    let archive = std::sync::Arc::new(crate::history::SessionArchive::new(
        session.store.clone(),
        session.id().into(),
    ));
    let local_limit = state
        .calibration()
        .local_limit(context_tokens, max_output_tokens as usize);
    let mut agent = Agent::new(backend.clone(), tools.clone(), config)
        .with_calibration(state.model.clone(), state.calibration())
        .with_compaction(compaction.clone())
        .with_file_activity(files.clone())
        .with_context_archive(archive.clone());

    if !compacting {
        agent = agent.with_execution_journal(session.execution_journal()?);
    }

    let summary_runtime =
        compaction.for_input_window(local_limit.saturating_sub(max_output_tokens as usize));
    let task = tokio::spawn(async move {
        if compacting {
            let compacted = orbit_agent::context::compact_configured(
                &backend,
                request.messages,
                &tools.definitions(),
                local_limit,
                max_output_tokens as usize,
                token,
                &files,
                Some(archive.as_ref()),
                &summary_runtime,
            )
            .await?;
            orbit_agent::context::check_budget(
                &compacted.history,
                &tools.definitions(),
                max_output_tokens as usize,
                local_limit,
            )?;
            let fallback = compacted.record.fallback;
            let fallback_reason = compacted.record.fallback_reason.clone();
            let before_tokens = compacted.before_tokens;
            let after_tokens = compacted.after_tokens;
            let strategy = compacted.record.strategy.clone();
            sender
                .send(AgentEvent::CompactionRecorded {
                    record: compacted.record,
                })
                .await
                .map_err(|_| AgentError::EventChannelClosed)?;
            sender
                .send(AgentEvent::ContextCompacted {
                    strategy,
                    before_tokens,
                    after_tokens,
                    fallback,
                    fallback_reason,
                })
                .await
                .map_err(|_| AgentError::EventChannelClosed)?;
            return Ok(AgentResult {
                // The compacted history is already durable. The nonempty
                // sentinel keeps the common run-finalization path from
                // mistaking this maintenance command for an empty model reply.
                final_message: ChatMessage::assistant("context compacted"),
                history: compacted.history,
                turns: 1,
                tool_calls: 0,
            });
        }

        agent.run_with_agent_events(request, token, sender).await
    });

    Ok(StartedRun {
        receiver,
        cancellation,
        task,
    })
}

// A failed model summary may have committed a retrieval-backed projection. If
// the user retries immediately, summarize the immutable source snapshot rather
// than trying to compress the already small projection a second time.
fn retry_archive_id(runs: &[Run], history: &[ChatMessage]) -> Option<i64> {
    let last_completed = runs.iter().rev().find(|run| run.status == "completed")?;

    if last_completed.prompt != "/compact" {
        return None;
    }

    let reference = last_completed.events.iter().find_map(|event| match event {
        AgentEvent::CompactionRecorded { record }
            if record.strategy == CompactionStrategy::ArchiveProjection =>
        {
            record.archive.as_deref()
        }
        _ => None,
    })?;
    let id = reference
        .strip_prefix("read_history archive_id=")?
        .split(';')
        .next()?
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)?;
    let marker = format!("read_history archive_id={id};");

    history
        .iter()
        .any(|message| {
            matches!(message, ChatMessage::User { content, .. }
            if content.starts_with("BEGIN_UNTRUSTED_COMPACTION_RECORD\n")
                && content.contains(&marker))
        })
        .then_some(id)
}

async fn compaction_source_history(
    session: &SessionState,
    history: &[ChatMessage],
) -> Result<Vec<ChatMessage>> {
    let Some(archive_id) = retry_archive_id(&session.snapshot.runs, history) else {
        return Ok(history.to_vec());
    };

    let source = session
        .store
        .load_context_archive(session.id(), archive_id)
        .await?;
    // Current injected instructions can change across restarts. Keep them,
    // while restoring the original conversation messages from the archive.
    let mut restored = history
        .iter()
        .take_while(|message| matches!(message, ChatMessage::System { .. }))
        .cloned()
        .collect::<Vec<_>>();
    restored.extend(
        source
            .into_iter()
            .skip_while(|message| matches!(message, ChatMessage::System { .. })),
    );

    Ok(restored)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TabAction {
    Select(String),
    Close(String),
    New,
}

fn tab_target(event: &Event, state: &UiState, tabs: &tabs::Tabs) -> Option<TabAction> {
    if state.show_help
        || state.picker.is_some()
        || state.model_picker.is_some()
        || state.marketplace_dialog.is_some()
    {
        return None;
    }

    match event {
        Event::Key(key)
            if key.kind != KeyEventKind::Release && key.modifiers.contains(KeyModifiers::ALT) =>
        {
            let KeyCode::Char(digit @ '1'..='9') = key.code else {
                return None;
            };

            tabs.target(digit.to_digit(10)? as usize - 1)
                .map(|id| TabAction::Select(id.to_owned()))
        }

        Event::Key(key)
            if key.kind != KeyEventKind::Release
                && key.code == KeyCode::Tab
                && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            tabs.adjacent(
                state.tabs.iter().find(|tab| tab.active)?.id.as_str(),
                key.modifiers.contains(KeyModifiers::SHIFT),
            )
            .map(|id| TabAction::Select(id.to_owned()))
        }

        Event::Mouse(mouse)
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && state
                    .new_tab_area
                    .contains((mouse.column, mouse.row).into()) =>
        {
            Some(TabAction::New)
        }

        Event::Mouse(mouse)
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && state
                    .tab_close_areas
                    .iter()
                    .any(|(area, _)| area.contains((mouse.column, mouse.row).into())) =>
        {
            state
                .tab_close_areas
                .iter()
                .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
                .map(|(_, id)| TabAction::Close(id.clone()))
        }

        Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => state
            .tab_areas
            .iter()
            .find(|(area, _)| {
                mouse.column >= area.x
                    && mouse.column < area.right()
                    && mouse.row >= area.y
                    && mouse.row < area.bottom()
            })
            .map(|(_, id)| TabAction::Select(id.clone())),

        _ => None,
    }
}

// Route list motion before the main loop discards unused pointer movement.
fn handle_list_hover(state: &mut UiState, event: &Event) -> bool {
    let Event::Mouse(mouse) = event else {
        return false;
    };
    if mouse.kind != MouseEventKind::Moved {
        return false;
    }
    let point = (mouse.column, mouse.row).into();

    if state.picker.is_some() || state.model_picker.is_some() || state.marketplace_dialog.is_some()
    {
        if state
            .dialog_rows
            .iter()
            .any(|(area, _)| area.contains(point))
        {
            dialogs::handle(state, event.clone());
            return true;
        }
    } else if state.approval.is_some()
        && state
            .approval_options
            .iter()
            .any(|(area, _)| area.contains(point))
    {
        approval::handle(state, event.clone());
        return true;
    }

    false
}

fn update_tab_hover(event: &Event, state: &mut UiState, tabs: &tabs::Tabs) -> bool {
    let next = match event {
        Event::Mouse(mouse)
            if mouse.kind == MouseEventKind::Moved && !state.selection.dragging() =>
        {
            let mut probe = *mouse;
            probe.kind = MouseEventKind::Down(MouseButton::Left);
            tab_target(&Event::Mouse(probe), state, tabs)
        }
        Event::Resize(_, _) | Event::Key(_) | Event::Mouse(_) => None,
        _ => return false,
    };

    if state.tab_hover.as_ref().map(|(action, _)| action) == next.as_ref() {
        return false;
    }

    state.tab_hover = next.map(|action| (action, std::time::Instant::now()));
    state.tab_hover_tick = 0;

    true
}

async fn new_tab(
    tabs: &mut tabs::Tabs,
    session: &mut SessionState,
    state: &mut UiState,
    model: &mut ModelClient,
    history: &mut Vec<ChatMessage>,
    context: CommandContext<'_>,
) -> Result<()> {
    let prepared = session.prepare_new(state.model.clone()).await?;
    let mut next_history = prepared.snapshot.history.clone();
    crate::instructions::inject(&mut next_history, context.instructions);
    context.skills.inject(&mut next_history);
    crate::project_state::inject(
        &mut next_history,
        &session.store,
        &prepared.snapshot.session.project_id,
        &prepared.snapshot.session.id,
    )
    .await?;

    let mut next_state = resume_ui_shell(state);
    next_state.workspace = prepared.snapshot.session.workspace.clone();
    next_state.permission_mode = permissions::Mode::default();
    next_state.reasoning_effort = state.reasoning_effort;
    next_state.reasoning_efforts = state.reasoning_efforts.clone();
    let next_model = ModelClient::new(model.config().clone())?;
    tabs.activate(
        session,
        state,
        model,
        history,
        (prepared, next_state, next_model, next_history),
    );
    refresh_context_usage(state, history, context.tools);
    session
        .set_model_profile(state.model.clone(), state.reasoning_effort)
        .await?;

    Ok(())
}

async fn run_command(
    prompt: &str,
    state: &mut UiState,
    model: &mut ModelClient,
    history: &mut Vec<ChatMessage>,
    session: &mut SessionState,
    context: CommandContext<'_>,
) -> Result<bool> {
    let CommandContext {
        tools,
        instructions,
        skills,
        mcp_statuses,
        model_options,
    } = context;
    let (command, argument) = prompt
        .split_once(char::is_whitespace)
        .unwrap_or((prompt, ""));
    let argument = argument.trim();

    match command {
        "/exit" | "/quit" if argument.is_empty() => return Ok(true),

        "/rename" => {
            if argument.is_empty() {
                state.transcript.push(TranscriptItem::CommandError(
                    "用法：/rename <会话标题>".into(),
                ));
            } else if argument.chars().count() > 120 {
                state.transcript.push(TranscriptItem::CommandError(
                    "会话标题不能超过 120 个字符。".into(),
                ));
            } else {
                session.rename(argument.into()).await?;
                state.transcript.push(TranscriptItem::CommandResult(format!(
                    "会话标题已改为：{argument}"
                )));
            }
        }

        "/mode" | "/permissions" => {
            if !argument.is_empty() {
                let Some(mode) = permissions::Mode::parse(argument) else {
                    state.transcript.push(TranscriptItem::CommandResult(
                        "用法：/mode ask | auto | YOLO".into(),
                    ));
                    return Ok(false);
                };

                session
                    .store
                    .set_tui_mode(session.id(), mode.label())
                    .await?;
                state.permission_mode = mode;
            }

            state.transcript.push(TranscriptItem::CommandResult(format!(
                "{}\nShift+Tab 切换 · /mode ask|auto|YOLO\n本会话各窗口同步；新会话默认为 auto。",
                state.permission_mode.description()
            )));
        }

        "/context" if argument.is_empty() => {
            let estimated = orbit_agent::context::estimate_tokens(history, &tools.definitions());
            let overview_tokens = tool_context_tokens(history, "project_overview");
            let system = history
                .iter()
                .filter(|message| matches!(message, ChatMessage::System { content } if !content.starts_with("ORBIT_PROJECT_STATE_V1\n")) && !crate::instructions::is_orbit_instruction(message))
                .cloned()
                .collect::<Vec<_>>();
            let project_rules = history
                .iter()
                .filter(|message| crate::instructions::is_orbit_instruction(message))
                .cloned()
                .collect::<Vec<_>>();
            let conversation = history
                .iter()
                .filter(|message| !matches!(message, ChatMessage::System { .. }))
                .cloned()
                .collect::<Vec<_>>();

            let system_tokens = orbit_agent::context::estimate_tokens(&system, &[]);
            let project_instruction_tokens =
                orbit_agent::context::estimate_tokens(&project_rules, &[]);
            let conversation_tokens = orbit_agent::context::estimate_tokens(&conversation, &[]);
            let tool_tokens = orbit_agent::context::estimate_tokens(&[], &tools.definitions());

            let sources = if instructions.sources.is_empty() {
                "未找到".to_owned()
            } else {
                format!("{} 条", instructions.sources.len())
            };

            let truncation = if instructions.truncated {
                "（达到 32 KiB 上限，后续文件未加载）"
            } else {
                ""
            };

            let project_state_tokens = history
                .iter()
                .filter_map(|message| match message {
                    ChatMessage::System { content }
                        if content.starts_with("ORBIT_PROJECT_STATE_V1\n") =>
                    {
                        Some(orbit_agent::context::estimate_tokens(
                            std::slice::from_ref(message),
                            &[],
                        ))
                    }
                    _ => None,
                })
                .sum::<usize>();
            state.transcript.push(TranscriptItem::CommandResult(format!("原始上下文估算：约 {estimated} tokens（含协议开销）\n系统提示：约 {system_tokens} tokens\n项目指令：{sources} · 约 {project_instruction_tokens} tokens {truncation}\n{}\n历史消息：约 {conversation_tokens} tokens\n工具定义：约 {tool_tokens} tokens\n代码概览工具结果：约 {overview_tokens} tokens\n任务与会话记忆：约 {project_state_tokens} tokens\n输入窗口：{} tokens · 输出上限：{} tokens · 请求预算：{} tokens\n自动整理：{}（按校准后的输入窗口触发，目标按近期工作预算确定）\n这是通用估算，不等于服务端实际用量；分类值含各自协议开销，不能相加为总计。\n工具原文先归档再精简；需要摘要时调用模型，摘要失败保留原上下文。\n/compact 按预算整理旧对话，保留完整工具配对和近期工作；归档内容可通过 read_history 回读。", instructions.source_summary(), state.input_context_tokens, state.max_output_tokens,state.context_tokens,if state.auto_compact { "开启" } else { "关闭" })));
            let calibration = state.calibration();
            state.transcript.push(TranscriptItem::CommandResult(format!(
                "校准后总计：约 {} tokens · {} 个服务端样本 · 系数 {:.3}",
                calibration.estimate(estimated),
                calibration.samples(),
                calibration.factor()
            )));
        }

        "/skills" if argument.is_empty() => state
            .transcript
            .push(TranscriptItem::CommandResult(skills.render())),

        "/mcp" if argument.is_empty() => {
            state
                .transcript
                .push(TranscriptItem::CommandResult(render_mcp_status(
                    mcp_statuses,
                )))
        }

        "/plugins" if argument.is_empty() => {
            let project_id = session.snapshot.session.project_id.clone();
            let enabled = session.store.project_plugins(&project_id).await?;
            state.marketplace_dialog = Some(dialogs::MarketplaceDialog::new(
                crate::marketplaces::list_current()?,
                project_id,
                enabled,
            ));
        }

        "/skill" => {
            if argument.is_empty() {
                state.transcript.push(TranscriptItem::CommandError(
                    "用法：/skill <名称>。输入 /skills 查看已发现的 Skills。".into(),
                ));
            } else if let Some(skill) = skills.get(argument) {
                match std::fs::read_to_string(&skill.path) {
                    Ok(content) => {
                        history.push(ChatMessage::user(format!(
                            "请在后续工作中使用已载入的 Skill /{}：\n{}",
                            skill.command, content
                        )));
                        state.transcript.push(TranscriptItem::CommandResult(format!(
                            "已载入 /{}。",
                            skill.command
                        )));
                    }
                    Err(error) => state.transcript.push(TranscriptItem::CommandError(format!(
                        "无法读取 Skill /{}：{error}",
                        skill.command
                    ))),
                }
            } else {
                state.transcript.push(TranscriptItem::CommandError(format!(
                    "未找到 Skill {argument:?}。输入 /skills 查看已发现的 Skills。"
                )));
            }
        }

        "/tasks" => run_tasks_command(argument, state, history, session).await?,
        "/memory" => run_memory_command(argument, state, history, session).await?,

        "/changes" if argument.is_empty() => {
            let listing = changes::list(&session.store.workspace_changes(session.id()).await?);
            let comparison = crate::verification::changes_text(session).await;
            state.transcript.push(TranscriptItem::CommandResult(format!(
                "{listing}{comparison}"
            )));
        }

        "/diff" if argument.trim().is_empty() => {
            state
                .transcript
                .push(TranscriptItem::CommandResult(changes::list(
                    &session.store.workspace_changes(session.id()).await?,
                )))
        }

        "/diff" => state
            .transcript
            .push(TranscriptItem::CommandResult(changes::diff(
                &session.store.workspace_changes(session.id()).await?,
                argument.trim(),
            ))),
        "/diagnostics" if argument.is_empty() => {
            state.transcript.push(TranscriptItem::CommandResult(
                orbit_store::diagnostics::text(&session.store.diagnostics(session.id()).await?),
            ))
        }
        "/resume" | "/sessions" if argument.is_empty() => {
            state.picker = Some(dialogs::SessionPicker::new(session.resumable().await?));
            state.selection = Default::default();
        }
        "/resume" => {
            state.resume_request = Some(argument.to_owned());
        }

        "/status" if argument.is_empty() => {
            let verifications = session.store.verification_records(session.id()).await?;
            let passed = verifications
                .iter()
                .filter(|record| {
                    record.exit_code == Some(0) && !record.timed_out && !record.cancelled
                })
                .count();
            let failed = verifications.len().saturating_sub(passed);
            let verification = if let Some(last) = verifications.last() {
                format!(
                    "命令执行  {} 次 · 成功退出 {} · 异常结束 {} · 最近 exit {} · {} ms",
                    verifications.len(),
                    passed,
                    failed,
                    last.exit_code
                        .map_or_else(|| "无".to_owned(), |code| code.to_string()),
                    last.elapsed_ms,
                )
            } else {
                "命令执行  尚未执行沙箱命令".to_owned()
            };

            let verification = format!(
                "{}\n{}",
                verification,
                crate::verification::summary(session).await
            );
            let baseline = session.git_baseline.as_ref().map_or_else(
                || "Git 基线  非 Git 工作区或无法读取".to_owned(),
                |value| {
                    format!(
                        "Git 基线  {} · HEAD {} · 初始改动 {} 项{}",
                        value["branch"]["head"].as_str().unwrap_or("detached"),
                        value["branch"]["oid"].as_str().unwrap_or("unknown"),
                        value["entries"].as_array().map_or(0, Vec::len),
                        if value["has_more"].as_bool() == Some(true) {
                            "以上限截断"
                        } else {
                            ""
                        },
                    )
                },
            );
            let tool_names = tools
                .definitions()
                .iter()
                .map(|definition| definition.function.name.as_str())
                .collect::<Vec<_>>()
                .join(" / ");
            state.transcript.push(TranscriptItem::CommandResult(format!(
                "会话  {}\n数据库  {}\n会话工作目录  {}（沙箱内 /scratch）\n命令输出目录  {}（按会话保留）\n{}\n{}\n模型  {}\n上下文  {} 条消息 · 已持久化\n项目指令  {} 条\nSkills  {} 个\nMCP  {} 个 server，输入 /mcp 查看状态\n超时  {}s（流空闲）\n工具  {} · 审批：{}",
                session.id(), session.store.path().display(), session.scratch.read().expect("scratch lock poisoned").display(), session.command_output.read().expect("command output lock poisoned").display(), baseline, verification, state.model, history.len(), instructions.sources.len(), skills.len(), mcp_statuses.len(), model.config().timeout.as_secs(), tool_names, state.permission_mode.label()
            )));
        }

        "/reasoning" => {
            let efforts = state.reasoning_efforts.clone();
            if efforts.is_empty() {
                state.transcript.push(TranscriptItem::CommandResult(
                    "当前模型未配置可调整的思考强度。".into(),
                ));
            } else if argument.is_empty() {
                state.input.set("/reasoning ".into());
            } else if let Some(effort) = efforts.iter().find(|effort| effort.as_str() == argument) {
                let name = state.model.clone();
                select_model(&name, Some(*effort), state, model, model_options, session).await?;
            } else {
                state.transcript.push(TranscriptItem::CommandError(format!(
                    "当前模型支持的思考强度：{}",
                    efforts
                        .iter()
                        .map(|effort| effort.as_str())
                        .collect::<Vec<_>>()
                        .join(" / ")
                )));
            }
        }

        "/model" => {
            if argument.is_empty() {
                state.model_picker = Some(dialogs::ModelPicker::new(
                    model_options.to_vec(),
                    state.model.clone(),
                    model.config().reasoning_effort,
                ));
            } else {
                let current_provider = model_options
                    .iter()
                    .find(|choice| choice.qualified_name() == state.model)
                    .map(|choice| choice.provider.as_str());
                let choice = if let Some((provider, name)) = argument.split_once("::") {
                    model_options
                        .iter()
                        .find(|choice| choice.provider == provider && choice.model == name)
                } else {
                    model_options.iter().find(|choice| {
                        Some(choice.provider.as_str()) == current_provider
                            && choice.model == argument
                    })
                };

                if let Some(choice) = choice {
                    select_model(
                        &choice.qualified_name(),
                        None,
                        state,
                        model,
                        model_options,
                        session,
                    )
                    .await?;
                } else {
                    state.transcript.push(TranscriptItem::CommandResult(format!(
                        "模型 {argument:?} 未找到。"
                    )));
                }
            }
        }

        _ => {
            let result = if let Some(suggestion) = commands::suggestion(command) {
                format!("未知命令：{command}。是否想输入 {}？", suggestion.name)
            } else {
                format!("未知命令：{command}。输入 / 查看可用命令。")
            };

            state.transcript.push(TranscriptItem::CommandError(result));
        }
    }

    state.follow_bottom = true;
    state.menu_dismissed = false;

    Ok(false)
}

async fn run_tasks_command(
    argument: &str,
    state: &mut UiState,
    history: &mut Vec<ChatMessage>,
    session: &SessionState,
) -> Result<()> {
    let (action, rest) = argument
        .split_once(char::is_whitespace)
        .unwrap_or((argument, ""));
    let project_id = &session.snapshot.session.project_id;

    match action {
        "" | "list" | "all" => {
            let tasks = session.store.tasks(project_id, action == "all").await?;
            state
                .transcript
                .push(TranscriptItem::CommandResult(render_tasks(&tasks)));
        }

        "add" => {
            if rest.trim().is_empty() {
                state.transcript.push(TranscriptItem::CommandError(
                    "用法：/tasks add <任务标题>".into(),
                ));
            } else {
                let task = session
                    .store
                    .create_task(project_id, None, rest.trim().into(), String::new(), 0)
                    .await?;
                crate::project_state::inject(history, &session.store, project_id, session.id())
                    .await?;
                state.transcript.push(TranscriptItem::CommandResult(format!(
                    "已添加任务 #{}：{}",
                    task.id, task.title
                )));
            }
        }

        "start" | "done" | "block" | "cancel" => {
            let Ok(id) = rest.trim().parse::<i64>() else {
                state.transcript.push(TranscriptItem::CommandError(
                    "用法：/tasks start|done|block|cancel <任务编号>".into(),
                ));
                return Ok(());
            };

            let status = match action {
                "start" => orbit_store::types::TaskStatus::InProgress,
                "done" => orbit_store::types::TaskStatus::Completed,
                "block" => orbit_store::types::TaskStatus::Blocked,
                "cancel" => orbit_store::types::TaskStatus::Cancelled,
                _ => unreachable!(),
            };

            let task = session
                .store
                .update_task(project_id, id, None, None, Some(status), None)
                .await?;
            crate::project_state::inject(history, &session.store, project_id, session.id()).await?;
            state.transcript.push(TranscriptItem::CommandResult(format!(
                "任务 #{} 已标记为{}。",
                task.id,
                task_status_label(task.status)
            )));
        }

        _ => state.transcript.push(TranscriptItem::CommandError(
            "用法：/tasks [list|all] · /tasks add <标题> · /tasks start|done|block|cancel <编号>"
                .into(),
        )),
    }

    Ok(())
}

fn render_mcp_status(statuses: &[orbit_tools::mcp::McpServerStatus]) -> String {
    if statuses.is_empty() {
        return "没有配置 MCP server。可在 config.toml 中添加 [mcp.servers.<名称>]，指定 command、args 和 enabled = true。当前仅支持本地 stdio server。".into();
    }

    let mut output = String::from("MCP servers\n");

    for status in statuses {
        let marker = if status.connected { "✓" } else { "×" };

        output.push_str(&format!(
            "{marker} {}  {} · {} 个工具\n",
            status.name, status.detail, status.tool_count
        ));
    }

    output
}

async fn run_memory_command(
    argument: &str,
    state: &mut UiState,
    history: &mut Vec<ChatMessage>,
    session: &SessionState,
) -> Result<()> {
    let (action, rest) = argument
        .split_once(char::is_whitespace)
        .unwrap_or((argument, ""));
    let session_id = session.id();

    match action {
        "" | "list" | "all" => {
            let memories = session
                .store
                .session_memories(session_id, action == "all")
                .await?;
            state
                .transcript
                .push(TranscriptItem::CommandResult(render_memories(&memories)));
        }

        "add" => {
            let (category, content) = rest.split_once(char::is_whitespace).unwrap_or(("", ""));
            let Some(category) = orbit_store::types::MemoryCategory::parse(category) else {
                state.transcript.push(TranscriptItem::CommandError(
                    "用法：/memory add constraint|decision|fact|preference <内容>".into(),
                ));
                return Ok(());
            };
            if content.trim().is_empty() {
                state
                    .transcript
                    .push(TranscriptItem::CommandError("记忆内容不能为空。".into()));
                return Ok(());
            }

            let title = concise_title(content.trim());
            let memory = session
                .store
                .remember_in_session(
                    session_id,
                    category,
                    title,
                    content.trim().into(),
                )
                .await?;
            crate::project_state::inject(
                history,
                &session.store,
                &session.snapshot.session.project_id,
                session_id,
            )
            .await?;
            state.transcript.push(TranscriptItem::CommandResult(format!(
                "已保存会话记忆 #{}。",
                memory.id
            )));
        }

        "search" => {
            if rest.trim().is_empty() {
                state.transcript.push(TranscriptItem::CommandError(
                    "用法：/memory search <关键词>".into(),
                ));
                return Ok(());
            }

            let memories = session
                .store
                .search_session_memories(session_id, rest.trim(), 8)
                .await?;
            state.transcript.push(TranscriptItem::CommandResult(format!(
                "记忆搜索：{}\n{}",
                rest.trim(),
                render_memories(&memories)
            )));
        }

        "forget" => {
            let Ok(id) = rest.trim().parse::<i64>() else {
                state.transcript.push(TranscriptItem::CommandError(
                    "用法：/memory forget <记忆编号>".into(),
                ));
                return Ok(());
            };

            let memory = session
                .store
                .set_session_memory_active(session_id, id, false)
                .await?;
            crate::project_state::inject(
                history,
                &session.store,
                &session.snapshot.session.project_id,
                session_id,
            )
            .await?;
            state.transcript.push(TranscriptItem::CommandResult(format!(
                "会话记忆 #{} 已停用。",
                memory.id
            )));
        }

        _ => state.transcript.push(TranscriptItem::CommandError(
            "用法：/memory [list|all] · /memory search <关键词> · /memory add <类型> <内容> · /memory forget <编号>".into(),
        )),
    }

    Ok(())
}

fn render_tasks(tasks: &[orbit_store::types::ProjectTask]) -> String {
    if tasks.is_empty() {
        return "当前项目没有待跟踪的任务。\n\n/tasks add <标题> 可创建任务。".into();
    }

    let mut output = String::from("项目任务\n");

    for task in tasks {
        use std::fmt::Write;

        let _ = writeln!(
            output,
            "{}  #{}  {}{}",
            task_status_label(task.status),
            task.id,
            task.title,
            if !task.details.is_empty() {
                format!("\n        {}", task.details)
            } else {
                String::new()
            },
        );
    }

    output
}

fn task_status_label(status: orbit_store::types::TaskStatus) -> &'static str {
    match status {
        orbit_store::types::TaskStatus::Pending => "○ 待办",
        orbit_store::types::TaskStatus::InProgress => "▶ 进行中",
        orbit_store::types::TaskStatus::Blocked => "◆ 已阻塞",
        orbit_store::types::TaskStatus::Completed => "✓ 完成",
        orbit_store::types::TaskStatus::Cancelled => "— 已取消",
    }
}

fn render_memories(memories: &[orbit_store::types::SessionMemory]) -> String {
    if memories.is_empty() {
        return "当前会话没有已保存的长期记忆。\n\n/memory add <类型> <内容> 可保存约束、决策、事实或偏好。".into();
    }

    let mut output = String::from("会话记忆\n");

    for memory in memories {
        use std::fmt::Write;

        let marker = if memory.active { "●" } else { "○" };

        let _ = writeln!(
            output,
            "{marker} #{} [{}] {}\n  {}",
            memory.id,
            memory.category.as_str(),
            memory.title,
            memory.content,
        );
    }

    output
}

fn concise_title(content: &str) -> String {
    const MAX_CHARS: usize = 72;

    let mut title: String = content.chars().take(MAX_CHARS).collect();

    if content.chars().nth(MAX_CHARS).is_some() {
        title.push('…');
    }

    title
}

fn needs_model_switch_compaction(state: &UiState) -> bool {
    state.auto_compact
        && state.context_used_tokens >= state.input_context_tokens.saturating_mul(85) / 100
}

async fn select_model(
    qualified_name: &str,
    selected_effort: Option<orbit_model::types::ReasoningEffort>,
    state: &mut UiState,
    model: &mut ModelClient,
    model_options: &[ModelOption],
    session: &mut SessionState,
) -> Result<()> {
    let Some(choice) = model_options
        .iter()
        .find(|choice| choice.qualified_name() == qualified_name)
    else {
        state
            .transcript
            .push(TranscriptItem::Error("所选模型已不在当前配置中。".into()));
        return Ok(());
    };

    let mut config = choice.config.clone();

    if let Some(effort) = selected_effort {
        config.reasoning_effort = choice.reasoning_efforts.contains(&effort).then_some(effort);
    }

    match ModelClient::new(config) {
        Ok(client) => {
            let previous_model = state.model.clone();
            let model_name = choice.qualified_name();
            let reasoning_effort = client.config().reasoning_effort;
            session
                .set_model_profile(model_name.clone(), reasoning_effort)
                .await?;
            *model = client;
            state.model = model_name;
            state.compact_after_model_switch |=
                state.auto_compact && choice.input_context_tokens < state.input_context_tokens;
            state.max_output_tokens = choice.max_output_tokens;
            state.context_tokens = choice.context_tokens;
            state.input_context_tokens = choice.input_context_tokens;
            state.reasoning_effort = reasoning_effort;
            state.reasoning_efforts = choice.reasoning_efforts.clone();
            state.supports_vision = choice.supports_vision;

            if previous_model != state.model {
                state.transcript.push(TranscriptItem::CommandResult(format!(
                    "{previous_model} → {}",
                    state.model
                )));
            }

            if !choice.supports_vision && !state.attachments.is_empty() {
                state.attachments.clear();
                state.transcript.push(TranscriptItem::Notice(
                    "所选模型不支持图片输入，已清除待发送的图片附件。".into(),
                ));
            }
        }
        Err(error) => state
            .transcript
            .push(TranscriptItem::Error(error.to_string())),
    }

    Ok(())
}

fn attachment_path(argument: &str) -> std::path::PathBuf {
    let trimmed = argument.trim();
    let trimmed = if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };

    if let Ok(url) = url::Url::parse(trimmed)
        && url.scheme() == "file"
        && let Ok(path) = url.to_file_path()
    {
        return path;
    }

    #[cfg(unix)]
    let trimmed = {
        let mut value = String::with_capacity(trimmed.len());
        let mut chars = trimmed.chars();

        while let Some(character) = chars.next() {
            if character == '\\' {
                value.push(chars.next().unwrap_or(character));
            } else {
                value.push(character);
            }
        }

        value
    };
    #[cfg(not(unix))]
    let trimmed = trimmed.to_owned();

    trimmed.into()
}

fn resolved_image_path(value: &str, directory: &std::path::Path) -> Option<PathBuf> {
    let path = attachment_path(value);
    let path = if path.is_absolute() {
        path
    } else {
        directory.join(path)
    };

    let extension = path.extension()?.to_str()?.to_ascii_lowercase();

    matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp")
        .then_some(path)
        .filter(|path| path.is_file())
}

fn pasted_image_paths(text: &str, directory: &std::path::Path) -> Option<Vec<PathBuf>> {
    if let Some(path) = resolved_image_path(text, directory) {
        return Some(vec![path]);
    }

    #[cfg(unix)]
    {
        let paths = shlex::split(text)?
            .into_iter()
            .map(|part| resolved_image_path(&part, directory))
            .collect::<Option<Vec<_>>>()?;
        (!paths.is_empty()).then_some(paths)
    }

    #[cfg(not(unix))]
    None
}

fn queue_attachment(state: &mut UiState, attachment: orbit_model::types::Attachment) -> bool {
    if !state.supports_vision {
        return false;
    }
    if state
        .attachments
        .iter()
        .any(|existing| existing == &attachment)
    {
        return false;
    }

    state.attachments.push(attachment);
    state.attachment_cursor = None;

    true
}

fn focus_attachment_at(state: &mut UiState, x: u16, y: u16) -> bool {
    let Some(index) = state
        .attachment_areas
        .iter()
        .position(|area| area.contains((x, y).into()))
    else {
        return false;
    };

    state.attachment_cursor = Some(index);

    true
}

fn browser_url(value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value)?;

    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        anyhow::bail!("仅能在浏览器中打开不含凭据的 HTTP(S) 链接");
    }

    Ok(url)
}

fn open_link_in_browser(value: &str) -> Result<()> {
    let url = browser_url(value)?;
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    anyhow::bail!("当前系统不支持打开默认浏览器");
    // `xdg-open` may hand off to an already-running Chromium process. Keep
    // every descriptor detached from the alternate screen; otherwise browser
    // diagnostics can be printed through the active TUI and corrupt its frame.
    command
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    Ok(())
}

fn remove_focused_attachment(state: &mut UiState, backwards: bool) -> bool {
    let Some(index) = state.attachment_cursor.take() else {
        return false;
    };
    if index >= state.attachments.len() {
        return false;
    }

    state.attachments.remove(index);

    if backwards && index > 0 {
        state.attachment_cursor = Some(index - 1);
    } else if !backwards && index < state.attachments.len() {
        state.attachment_cursor = Some(index);
    }

    true
}

fn attach_paths(state: &mut UiState, session: &SessionState, paths: Vec<PathBuf>) -> Result<()> {
    if !state.supports_vision {
        state.transcript.push(TranscriptItem::Notice(
            "当前模型未启用图片输入，未添加图片附件。可切换支持图片的模型，或在 /connect 中调整自定义模型的图片输入设置。".into(),
        ));
        return Ok(());
    }

    for path in paths {
        let attachment = session.import_image(&path)?;
        queue_attachment(state, attachment);
    }

    Ok(())
}

enum ClipboardPayload {
    Image {
        width: usize,
        height: usize,
        bytes: Vec<u8>,
    },
    Files(Vec<PathBuf>),
    Text(String),
}

fn read_clipboard() -> Result<ClipboardPayload> {
    let mut clipboard = arboard::Clipboard::new()?;

    if let Ok(image) = clipboard.get_image() {
        return Ok(ClipboardPayload::Image {
            width: image.width,
            height: image.height,
            bytes: image.bytes.into_owned(),
        });
    }
    if let Ok(files) = clipboard.get().file_list()
        && !files.is_empty()
    {
        return Ok(ClipboardPayload::Files(files));
    }
    if let Ok(text) = clipboard.get_text() {
        return Ok(ClipboardPayload::Text(text));
    }

    anyhow::bail!("剪贴板中没有可用的图片、文件或文本")
}

async fn paste_clipboard(state: &mut UiState, session: &SessionState) -> Result<()> {
    let payload = tokio::task::spawn_blocking(read_clipboard).await??;

    match payload {
        ClipboardPayload::Image {
            width,
            height,
            bytes,
        } => {
            if !state.supports_vision {
                state.transcript.push(TranscriptItem::Notice(
                    "当前模型未启用图片输入，未添加图片附件。可切换支持图片的模型，或在 /connect 中调整自定义模型的图片输入设置。".into(),
                ));
                state.menu_dismissed = false;
                return Ok(());
            }

            let attachment = session.import_clipboard_image(width, height, bytes)?;
            queue_attachment(state, attachment);
        }
        ClipboardPayload::Files(files) => {
            let images = files
                .iter()
                .filter_map(|path| {
                    resolved_image_path(
                        &path.to_string_lossy(),
                        std::path::Path::new(&state.workspace),
                    )
                })
                .collect::<Vec<_>>();

            if images.len() == files.len() {
                attach_paths(state, session, images)?;
            } else {
                state.input.insert(
                    &files
                        .iter()
                        .map(|path| path.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
        }
        ClipboardPayload::Text(text) => {
            if let Some(paths) = pasted_image_paths(&text, std::path::Path::new(&state.workspace)) {
                attach_paths(state, session, paths)?;
            } else {
                state.input.insert(&text);
            }
        }
    }

    state.menu_dismissed = false;

    Ok(())
}

async fn activate_resume(
    options: &crate::TuiOptions,
    tabs: &mut tabs::Tabs,
    session: &mut SessionState,
    state: &mut UiState,
    model: &mut ModelClient,
    history: &mut Vec<ChatMessage>,
    next: (crate::session::PreparedResume, Vec<ChatMessage>),
) -> Result<crate::TuiResources> {
    let (prepared, loaded_history) = next;
    let previous_id = session.id().to_owned();
    let restored_id = prepared.snapshot.session.id.clone();
    let directory = PathBuf::from(&prepared.snapshot.session.workspace);
    let mut restored = resume_ui_shell(state);
    restored.workspace = prepared.snapshot.session.workspace.clone();
    restored.resume_loading = Some("正在显示会话历史…".into());
    let next_model = ModelClient::new(model.config().clone())?;
    tabs.activate(
        session,
        state,
        model,
        history,
        (prepared, restored, next_model, loaded_history),
    );

    let result: Result<_> = async {
        let resources = crate::prepare_tui_resources(options, &directory, session).await?;
        workspace::bind(&resources, session, state, model, history).await?;
        Ok::<_, anyhow::Error>(resources)
    }
    .await
    .with_context(|| format!("会话 {restored_id}，工作区 {}", directory.display()));

    if result.is_err() {
        tabs.restore(&previous_id, session, state, model, history);
        tabs.close_parked(&restored_id);
    }

    result
}

async fn load_resume(
    store: orbit_store::store::Store,
    project: String,
    id: String,
    instructions: crate::instructions::Instructions,
    skills: crate::skills::Skills,
) -> Result<(crate::session::PreparedResume, Vec<ChatMessage>)> {
    let prepared = SessionState::prepare_shared(store.clone(), project, id).await?;
    let mut history = prepared.snapshot.history.clone();
    crate::instructions::inject(&mut history, &instructions);
    skills.inject(&mut history);
    crate::project_state::inject(
        &mut history,
        &store,
        &prepared.snapshot.session.project_id,
        &prepared.snapshot.session.id,
    )
    .await?;

    Ok((prepared, history))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn resume_session(
    id: &str,
    state: &mut UiState,
    model: &mut ModelClient,
    history: &mut Vec<ChatMessage>,
    session: &mut SessionState,
    instructions: &crate::instructions::Instructions,
    skills: &crate::skills::Skills,
    model_options: &[ModelOption],
    memory_scope: &crate::project_state::MemoryScope,
    tools: &ToolRegistry,
) -> Result<()> {
    if id == session.id() {
        return Ok(());
    }

    let (prepared, loaded_history) = load_resume(
        session.store.clone(),
        session.snapshot.session.project_id.clone(),
        id.into(),
        instructions.clone(),
        skills.clone(),
    )
    .await?;
    session.install_resume(prepared);
    memory_scope.set_session(session.id().into());
    *history = loaded_history;
    let mut restored = resume_ui_shell(state);
    session.restore_ui(&mut restored);
    *state = restored;
    restore_model_profile(state, model, model_options, session);
    refresh_context_usage(state, history, tools);

    Ok(())
}

fn resume_ui_shell(state: &UiState) -> UiState {
    let mut restored = UiState::new(state.model.clone());
    restored.context_tokens = state.context_tokens;
    restored.input_context_tokens = state.input_context_tokens;
    restored.auto_compact = state.auto_compact;
    restored.max_output_tokens = state.max_output_tokens;
    restored.model_completions = state.model_completions.clone();
    restored.completion_home = state.completion_home.clone();
    restored.skill_completions = state.skill_completions.clone();
    restored.permission_mode = state.permission_mode;
    restored.supports_vision = state.supports_vision;

    restored
}

fn read_active_command_stdout(dir: &std::path::Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut latest = None;
    let mut latest_time = std::time::SystemTime::UNIX_EPOCH;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "stdout")
            && let Ok(metadata) = entry.metadata()
            && let Ok(modified) = metadata.modified()
            && modified >= latest_time
        {
            latest_time = modified;
            latest = Some(path);
        }
    }

    let path = latest?;
    let content = std::fs::read(&path).ok()?;
    let slice = if content.len() > 16 * 1024 {
        &content[content.len() - 16 * 1024..]
    } else {
        &content[..]
    };
    Some(String::from_utf8_lossy(slice).to_string())
}

fn restore_model_profile(
    state: &mut UiState,
    model: &mut ModelClient,
    model_options: &[ModelOption],
    session: &SessionState,
) {
    let (saved_model, saved_effort) = session.saved_model_profile();
    let Some(choice) = model_options
        .iter()
        .find(|choice| choice.qualified_name() == saved_model)
    else {
        return;
    };

    let mut config = choice.config.clone();

    if let Some(effort) = saved_effort.filter(|effort| choice.reasoning_efforts.contains(effort)) {
        config.reasoning_effort = Some(effort);
    }

    let Ok(client) = ModelClient::new(config) else {
        return;
    };

    *model = client;
    state.model = choice.qualified_name();
    state.max_output_tokens = choice.max_output_tokens;
    state.context_tokens = choice.context_tokens;
    state.input_context_tokens = choice.input_context_tokens;
    state.reasoning_effort = model.config().reasoning_effort;
    state.reasoning_efforts = choice.reasoning_efforts.clone();
    state.supports_vision = choice.supports_vision;
}

#[derive(Debug, PartialEq)]
enum Action {
    None,
    Quit,
    Cancel,
    Steer(String),
    Redraw,
    AttachPaths(Vec<PathBuf>),
    PasteClipboard,
    Submit(String),
}

fn toggle_transcript_at(state: &mut UiState, point: (u16, u16)) -> bool {
    let offset = state.max_scroll.saturating_sub(state.scroll);
    let Some(index) = state.transcript_layout.disclosure_at(
        &state.transcript,
        state.selection.viewport,
        offset,
        point,
    ) else {
        return false;
    };

    toggle_transcript_item(state, index);

    true
}

fn toggle_transcript_item(state: &mut UiState, index: usize) {
    let offset = state.max_scroll.saturating_sub(state.scroll);
    let anchor = state.transcript_layout.anchor_at(offset);
    let collapsing = state.expanded.contains(&index);
    let start = state.transcript_layout.item_offset(index).unwrap_or(offset);
    let end = state
        .transcript_layout
        .item_offset(index + 1)
        .unwrap_or_else(|| state.transcript_layout.height());
    let viewport_in_collapsing_item = collapsing && (start..end).contains(&offset);
    if collapsing {
        state.expanded.remove(&index);
    } else {
        state.expanded.insert(index);
    }
    state.follow_bottom = false;
    state.selection.clear();
    state.transcript_layout.update_expanded(
        &state.transcript,
        &state.expanded,
        state.selection.viewport.width,
    );
    state.max_scroll = state
        .transcript_layout
        .height()
        .saturating_sub(usize::from(state.selection.viewport.height));
    let target = if viewport_in_collapsing_item {
        state.transcript_layout.item_offset(index).unwrap_or(offset)
    } else {
        anchor
            .and_then(|anchor| state.transcript_layout.offset_for_anchor(anchor))
            .unwrap_or(offset)
    };

    state.scroll = state.max_scroll.saturating_sub(target);
}

fn keyboard_action(state: &mut UiState, event: Event, busy: bool) -> Action {
    if let Event::Mouse(mouse) = event {
        match mouse.kind {
            MouseEventKind::ScrollUp => state.scroll_up(wheel::LINES),
            MouseEventKind::ScrollDown => state.scroll_down(wheel::LINES),
            _ => {}
        }

        return Action::None;
    }
    if let Event::Paste(text) = event {
        state.attachment_cursor = None;

        if text.is_empty() {
            return Action::PasteClipboard;
        }
        if let Some(paths) = pasted_image_paths(&text, std::path::Path::new(&state.workspace)) {
            return Action::AttachPaths(paths);
        }

        state.input.insert(&text);
        state.menu_dismissed = false;
        return Action::None;
    }
    let Event::Key(key) = event else {
        return Action::None;
    };
    if key.kind == KeyEventKind::Release {
        return Action::None;
    }

    let control = key.modifiers.contains(KeyModifiers::CONTROL);

    if control {
        match key.code {
            KeyCode::Char('c') if busy => return Action::Cancel,
            KeyCode::Char('c') if !state.input.text.is_empty() || !state.attachments.is_empty() => {
                state.input.clear();
                state.attachments.clear();
                state.attachment_cursor = None;
            }
            KeyCode::Char('c') => return Action::Quit,
            KeyCode::Char('d') if state.input.text.is_empty() && state.attachments.is_empty() => {
                return Action::Quit;
            }
            KeyCode::Char('d') => {
                if !remove_focused_attachment(state, false) {
                    state.input.delete();
                }
            }

            KeyCode::Char('j') => {
                if state.attachment_cursor.take().is_some() {
                    state.input.cursor = 0;
                }

                state.input.insert("\n");
            }

            KeyCode::Char('a') if !state.attachments.is_empty() => {
                state.input.home();
                state.attachment_cursor = Some(0);
            }
            KeyCode::Char('a') => state.input.home(),
            KeyCode::Char('e') => {
                state.attachment_cursor = None;
                state.input.end();
            }
            KeyCode::Char('u') => {
                let end = state.input.cursor;
                state.input.home();
                state.input.text.drain(state.input.cursor..end);
            }
            KeyCode::Char('k') => {
                let start = state.input.cursor;
                state.input.end();
                state.input.text.drain(start..state.input.cursor);
                state.input.cursor = start;
            }
            KeyCode::Char('l') => return Action::Redraw,
            KeyCode::Char('v') => return Action::PasteClipboard,

            KeyCode::Char('o') => {
                let offset = state.max_scroll.saturating_sub(state.scroll);

                if let Some(index) = state.expanded.iter().next_back().copied().or_else(|| {
                    state.transcript_layout.visible_collapsible(
                        &state.transcript,
                        offset,
                        state.selection.viewport.height as usize,
                    )
                }) {
                    toggle_transcript_item(state, index);
                }
            }

            _ => {}
        }

        return Action::None;
    }
    if key.code == KeyCode::Esc {
        state.attachment_cursor = None;
        state.show_help = false;
        state.menu_dismissed = true;
        return Action::None;
    }
    if key.code == KeyCode::Char('v') && key.modifiers.contains(KeyModifiers::ALT) {
        return Action::PasteClipboard;
    }

    state.menu_manual_scroll = false;
    let menu = state.menu();

    if !menu.is_empty() {
        state.menu_index = state.menu_index.min(menu.len() - 1);

        match key.code {
            KeyCode::Up => {
                state.menu_index = (state.menu_index + menu.len() - 1) % menu.len();
                return Action::None;
            }
            KeyCode::Down => {
                state.menu_index = (state.menu_index + 1) % menu.len();
                return Action::None;
            }
            KeyCode::Tab => {
                let candidate = &menu[state.menu_index];
                let suffix = if (candidate.continue_input && !candidate.name.starts_with("/cd "))
                    || matches!(
                        candidate.name.as_str(),
                        "/cd"
                            | "/mode"
                            | "/model"
                            | "/reasoning"
                            | "/skill"
                            | "/tasks"
                            | "/memory"
                            | "/diff"
                            | "/undo"
                    ) {
                    " "
                } else {
                    ""
                };
                state.input.set(format!("{}{suffix}", candidate.name));
                state.menu_index = 0;
                return Action::None;
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let candidate = &menu[state.menu_index];
                if candidate.continue_input && !candidate.name.starts_with("/cd ") {
                    state.input.set(format!("{} ", candidate.name));
                    state.menu_index = 0;
                    return Action::None;
                }
                // An exact directory can be submitted even while its children are suggested.
                let exact_directory =
                    state
                        .input
                        .text
                        .strip_prefix("/cd ")
                        .is_some_and(|argument| {
                            directory::resolve(
                                argument,
                                std::path::Path::new(&state.workspace),
                                state.completion_home.as_deref(),
                            )
                            .is_ok()
                        });
                if !exact_directory || state.menu_index != 0 {
                    state.input.set(candidate.name.clone());
                }
            }
            _ => {}
        }
    }

    match key.code {
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            state.input.insert("\n")
        }

        KeyCode::Enter if busy => {
            if let Some(guidance) = state.input.take() {
                return if guidance.starts_with('/') {
                    Action::Submit(guidance)
                } else {
                    Action::Steer(guidance)
                };
            }
        }

        KeyCode::Enter if state.input.text.ends_with('\\') => {
            state.input.backspace();
            state.input.insert("\n");
        }

        KeyCode::Enter => {
            state.menu_index = 0;
            state.menu_dismissed = false;
            state.attachment_cursor = None;
            return if let Some(prompt) = state.input.take() {
                Action::Submit(prompt)
            } else if !state.attachments.is_empty() {
                Action::Submit(String::new())
            } else {
                Action::None
            };
        }

        KeyCode::Char('?') if state.input.text.is_empty() && state.attachments.is_empty() => {
            state.selection = Default::default();
            state.show_help = true;
            state.help_scroll = 0;
        }

        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
            if state.attachment_cursor.take().is_some() {
                state.input.cursor = 0;
            }

            state.input.insert(&c.to_string());
            state.menu_index = 0;
            state.menu_dismissed = false;
        }

        KeyCode::Backspace => {
            if !remove_focused_attachment(state, true) {
                if state.input.cursor == 0 && !state.attachments.is_empty() {
                    state.attachments.pop();
                } else {
                    state.input.backspace();
                }
            }

            state.menu_index = 0;
            state.menu_dismissed = false;
        }

        KeyCode::Delete => {
            if !remove_focused_attachment(state, false) {
                state.input.delete();
            }
        }

        KeyCode::Left => {
            if let Some(index) = &mut state.attachment_cursor {
                *index = index.saturating_sub(1);
            } else if state.input.cursor == 0 && !state.attachments.is_empty() {
                state.attachment_cursor = Some(state.attachments.len() - 1);
            } else {
                state.input.move_left();
            }
        }

        KeyCode::Right => {
            if let Some(index) = state.attachment_cursor {
                if index + 1 < state.attachments.len() {
                    state.attachment_cursor = Some(index + 1);
                } else {
                    state.attachment_cursor = None;
                    state.input.cursor = 0;
                }
            } else {
                state.input.move_right();
            }
        }

        KeyCode::Home => {
            state.input.home();

            if !state.attachments.is_empty() {
                state.attachment_cursor = Some(0);
            }
        }

        KeyCode::End => {
            state.attachment_cursor = None;
            state.input.end();
        }
        KeyCode::Up => {
            if state.attachment_cursor.is_none() {
                state.input.move_vertical(false, state.input_width);
            }
        }

        KeyCode::Down => {
            if state.attachment_cursor.take().is_some() {
                state.input.cursor = 0;
            } else {
                state.input.move_vertical(true, state.input_width);
            }
        }

        KeyCode::PageUp => state.scroll_up(8),
        KeyCode::PageDown => state.scroll_down(8),
        _ => {}
    }

    Action::None
}

/// First-run connection setup, before a model or session is constructed.
pub(crate) async fn setup_connection(options: &crate::TuiOptions) -> Result<bool> {
    let (_guard, mut terminal) = terminal::TerminalGuard::enter()?;
    let mut events = EventStream::new();
    Ok(connect::run(&mut terminal, &mut events, options)
        .await?
        .is_some())
}

#[cfg(test)]
mod tests {
    #[test]
    fn running_context_is_not_overwritten_by_completed_history() {
        let mut state = UiState::new("mock");
        state.submit("new prompt".into());
        state.apply(AgentEvent::ContextUsage {
            estimated_input: 45000,
        });
        let live = state.context_used_tokens;
        refresh_context_usage(&mut state, &[], &ToolRegistry::new());
        assert_eq!(state.context_used_tokens, live);
        state.apply(AgentEvent::Completed {
            turns: 1,
            tool_calls: 0,
        });
        refresh_context_usage(&mut state, &[], &ToolRegistry::new());
        assert!(state.context_used_tokens < live);
    }

    use super::*;
    use crossterm::event::KeyEvent;

    #[tokio::test]
    async fn compact_retry_restores_the_original_history_after_a_failed_retry() {
        use orbit_agent::memory::{CompactionMode, CompactionRecord, FileActivity, TaskSummary};

        let dir = tempfile::tempdir().unwrap();
        let mut session = SessionState::open(dir.path().join("orbit.db"), None, "mock".into())
            .await
            .unwrap();
        let original = vec![
            ChatMessage::system("old instructions"),
            ChatMessage::user("original work"),
            ChatMessage::assistant("original answer"),
        ];
        let id = session
            .store
            .archive_context(session.id(), &original)
            .await
            .unwrap();

        let record = CompactionRecord {
            version: 2,
            mode: CompactionMode::Manual,
            source_messages: 3,
            retained_from: 2,
            pinned_user: None,
            before_tokens: 200_000,
            after_tokens: 40_000,
            strategy: CompactionStrategy::ArchiveProjection,
            summary_requests: 1,
            summary_input_tokens: 4_096,
            archive: Some(format!("read_history archive_id={id}; search with query")),
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
        };
        let archive_run = Run {
            prompt: "/compact".into(),
            status: "completed".into(),
            elapsed_ms: None,
            events: vec![AgentEvent::CompactionRecorded { record }],
        };
        let failed_retry = Run {
            prompt: "/compact".into(),
            status: "failed".into(),
            elapsed_ms: None,
            events: vec![],
        };
        let projected = vec![
            ChatMessage::system("current instructions"),
            ChatMessage::user(format!(
                "BEGIN_UNTRUSTED_COMPACTION_RECORD\nread_history archive_id={id}; search with query"
            )),
        ];

        assert_eq!(
            retry_archive_id(&[archive_run.clone(), failed_retry], &projected),
            Some(id)
        );

        session.snapshot.runs = vec![archive_run.clone()];
        let restored = compaction_source_history(&session, &projected)
            .await
            .unwrap();

        assert_eq!(
            restored,
            vec![
                ChatMessage::system("current instructions"),
                ChatMessage::user("original work"),
                ChatMessage::assistant("original answer"),
            ]
        );
        assert_eq!(
            retry_archive_id(std::slice::from_ref(&archive_run), &[]),
            None
        );

        let later_reply = Run {
            prompt: "new work".into(),
            status: "completed".into(),
            elapsed_ms: None,
            events: vec![],
        };

        assert_eq!(
            retry_archive_id(&[archive_run, later_reply], &projected),
            None
        );
    }

    #[test]
    fn resume_ui_shell_preserves_permission_mode() {
        let mut state = UiState::new("test::model");
        state.permission_mode = crate::tui::permissions::Mode::Yolo;
        let restored = resume_ui_shell(&state);

        assert_eq!(
            restored.permission_mode,
            crate::tui::permissions::Mode::Yolo
        );

        state.permission_mode = crate::tui::permissions::Mode::Ask;
        let restored = resume_ui_shell(&state);

        assert_eq!(restored.permission_mode, crate::tui::permissions::Mode::Ask);
    }

    #[tokio::test]
    async fn failed_restore_preserves_a_running_tab_and_allows_the_next_restore() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("orbit-original");
        let missing = directory.path().join("orbit-missing");
        let available = directory.path().join("orbit-available");
        for path in [&original, &missing, &available] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::write(available.join("marker.txt"), "Orbit restored workspace").unwrap();
        let config = directory.path().join("config.toml");
        std::fs::write(
            &config,
            r#"default_provider = "test"
[providers.test]
base_url = "http://127.0.0.1:1/v1"
api_key = "test-only"
[[providers.test.models]]
id = "orbit-model"
"#,
        )
        .unwrap();
        let options = crate::TuiOptions {
            config,
            database: directory.path().join("orbit.db"),
            auth_directory: directory.path().join("auth"),
            allow_mcp_start: false,
            home: None,
            config_home: None,
            instruction_dir: None,
            editor_command: "vi".into(),
        };
        let setup = crate::prepare_tui(&options, &original, None).await.unwrap();
        let mut session = setup.session;
        session.enable_shared();
        let original_id = session.id().to_owned();
        session
            .begin("Orbit original request".into(), "test::orbit-model".into())
            .await
            .unwrap();
        let original_run = session.active_run_id();
        let scratch = session.scratch.clone();
        let command_output = session.command_output.clone();
        let mut model = setup.resources.model;
        let mut state = UiState::new("test::orbit-model");
        state.submit("Orbit original request".into());
        state.input.set("Orbit draft".into());
        let mut history = vec![ChatMessage::user("Orbit original request")];
        let original_history = history.clone();
        let mut tabs = tabs::Tabs::new(&session);
        let mut saved_ids = Vec::new();
        for path in [&missing, &available] {
            let mut saved = SessionState::open_shared_in(
                options.database.clone(),
                None,
                "test::orbit-model".into(),
                path,
            )
            .await
            .unwrap();
            saved
                .begin("Orbit saved request".into(), "test::orbit-model".into())
                .await
                .unwrap();
            saved
                .finish(
                    "completed",
                    Some(vec![ChatMessage::user("Orbit saved request")]),
                )
                .await
                .unwrap();
            saved_ids.push(saved.id().to_owned());
        }
        std::fs::remove_dir(&missing).unwrap();

        let mut resources = None;
        for (index, id) in saved_ids.iter().enumerate() {
            let next = load_resume(
                session.store.clone(),
                session.snapshot.session.project_id.clone(),
                id.clone(),
                crate::instructions::Instructions::default(),
                crate::skills::Skills::default(),
            )
            .await
            .unwrap();
            let result = activate_resume(
                &options,
                &mut tabs,
                &mut session,
                &mut state,
                &mut model,
                &mut history,
                next,
            )
            .await;
            if index == 0 {
                let error = result.err().unwrap();
                assert!(format!("{error:#}").contains(missing.to_str().unwrap()));
                assert_eq!(session.id(), original_id);
                assert_eq!(session.active_run_id(), original_run);
                assert_eq!(session.audit_workspace.as_ref().unwrap().root(), original);
                assert!(std::sync::Arc::ptr_eq(&scratch, &session.scratch));
                assert!(std::sync::Arc::ptr_eq(
                    &command_output,
                    &session.command_output
                ));
                assert_eq!(state.input.text, "Orbit draft");
                assert!(state.running());
                assert_eq!(history, original_history);
                assert_eq!(model.config().model, "orbit-model");
                assert_eq!(
                    tabs.prune_closed(&background::Runs::new()),
                    vec![id.clone()]
                );
                assert!(!tabs.contains(id));
            } else {
                resources = Some(result.unwrap());
            }
        }

        assert_eq!(session.id(), saved_ids[1]);
        assert_eq!(session.audit_workspace.as_ref().unwrap().root(), available);
        let output = resources
            .unwrap()
            .tools
            .execute(
                orbit_model::types::ToolCall::function(
                    "orbit-read",
                    "read_file",
                    serde_json::json!({"path": "marker.txt"}),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(output.content.contains("Orbit restored workspace"));
        assert!(tabs.restore(
            &original_id,
            &mut session,
            &mut state,
            &mut model,
            &mut history
        ));
        assert_eq!(session.active_run_id(), original_run);
        assert_eq!(state.input.text, "Orbit draft");
        session.finish("completed", Some(history)).await.unwrap();
    }

    #[test]
    fn overview_budget_counts_only_matching_tool_results_in_effective_history() {
        use orbit_model::types::ToolCall;

        let call = ToolCall::function("map", "project_overview", serde_json::json!({}));
        let output = ChatMessage::tool("map", "repository map");
        let messages = vec![
            ChatMessage::user("project_overview"),
            ChatMessage::assistant_tool_calls(vec![call]),
            output.clone(),
            ChatMessage::tool("other", "unrelated output"),
        ];

        assert_eq!(
            tool_context_tokens(&messages, "project_overview"),
            orbit_agent::context::estimate_tokens(&[output], &[])
        );
        assert_eq!(tool_context_tokens(&[], "project_overview"), 0);
        assert_eq!(tool_context_tokens(&messages, "missing"), 0);
    }

    #[tokio::test]
    async fn resuming_recomputes_usage_from_restored_history_and_current_tools() {
        let tmp = tempfile::tempdir().unwrap();
        let mut session =
            SessionState::open(tmp.path().join("sessions.db"), None, "test::model".into())
                .await
                .unwrap();
        let id = session.id().to_owned();
        session
            .begin("question".into(), "test::model".into())
            .await
            .unwrap();
        session
            .finish(
                "completed",
                Some(vec![
                    ChatMessage::user("question"),
                    ChatMessage::assistant("answer ".repeat(5_000)),
                ]),
            )
            .await
            .unwrap();
        session.new_session("other".into()).await.unwrap();
        let scope = crate::project_state::MemoryScope::new(session.id().into());
        let mut model = ModelClient::new(Default::default()).unwrap();
        let options = vec![ModelOption {
            max_output_tokens: 32_000,
            provider: "test".into(),
            model: "model".into(),
            context_tokens: 232_000,
            input_context_tokens: 224_000,
            default_for_provider: true,
            reasoning_efforts: vec![],
            supports_vision: true,
            config: Default::default(),
        }];
        let mut tools = ToolRegistry::new();
        tools.register(orbit_agent::tool::ClosureTool::new(
            "test_tool",
            "test tool description",
            serde_json::json!({ "type": "object" }),
            |_, _| async { Ok("unused".into()) },
        ));
        let instructions = crate::instructions::Instructions::default();
        let skills = crate::skills::Skills::default();
        let mut history = Vec::new();
        let mut state = UiState::new("other");
        // Both the picker and `/resume <id>` call this entry point. It must
        // produce a valid count without a later command or model request.
        resume_session(
            &id,
            &mut state,
            &mut model,
            &mut history,
            &mut session,
            &instructions,
            &skills,
            &options,
            &scope,
            &tools,
        )
        .await
        .unwrap();

        assert_eq!(state.input_context_tokens, 224_000);
        assert_eq!(state.model, "test::model");
        assert!(state.context_used_tokens > 10_000);
        assert_eq!(
            state.context_used_tokens,
            orbit_agent::context::estimate_tokens(&history, &tools.definitions())
        );
        assert!(state.context_used_tokens > orbit_agent::context::estimate_tokens(&history, &[]));

        // Reopening the same saved session must not accumulate injected prompts.
        let before = state.context_used_tokens;
        resume_session(
            &id,
            &mut state,
            &mut model,
            &mut history,
            &mut session,
            &instructions,
            &skills,
            &options,
            &scope,
            &tools,
        )
        .await
        .unwrap();

        assert_eq!(state.context_used_tokens, before);
    }

    fn press(state: &mut UiState, code: KeyCode, modifiers: KeyModifiers, busy: bool) -> Action {
        keyboard_action(state, Event::Key(KeyEvent::new(code, modifiers)), busy)
    }

    #[test]
    fn browser_links_require_safe_http_urls() {
        assert_eq!(
            browser_url("https://example.com/docs?from=orbit")
                .unwrap()
                .as_str(),
            "https://example.com/docs?from=orbit"
        );

        for value in [
            "file:///tmp/report.html",
            "mailto:person@example.com",
            "https://user:secret@example.com/",
        ] {
            assert!(browser_url(value).is_err(), "{value}");
        }
    }

    #[test]
    fn argument_menu_completes_without_executing_incomplete_commands() {
        let mut state = UiState::new("one::fast");
        state.model_completions = vec!["one::fast".into(), "two::fast".into()];
        state.input.set("/model".into());

        assert_eq!(
            press(&mut state, KeyCode::Tab, KeyModifiers::NONE, false),
            Action::None
        );
        assert_eq!(state.input.text, "/model ");
        press(&mut state, KeyCode::Down, KeyModifiers::NONE, false);
        assert_eq!(
            press(&mut state, KeyCode::Enter, KeyModifiers::NONE, false),
            Action::Submit("/model two::fast".into())
        );

        state.input.set("/memory add f".into());
        assert_eq!(
            press(&mut state, KeyCode::Enter, KeyModifiers::NONE, false),
            Action::None
        );
        assert_eq!(state.input.text, "/memory add fact ");

        state.input.set("/mode au".into());
        assert_eq!(
            press(&mut state, KeyCode::Enter, KeyModifiers::NONE, true),
            Action::Submit("/mode auto".into())
        );
    }

    #[test]
    fn menu_selects_commands_and_busy_enter_routes_commands_locally() {
        let mut state = UiState::new("mock");
        state.input.insert("/");
        press(&mut state, KeyCode::Down, KeyModifiers::NONE, false);
        press(&mut state, KeyCode::Down, KeyModifiers::NONE, false);

        assert_eq!(
            press(&mut state, KeyCode::Enter, KeyModifiers::NONE, false),
            Action::Submit("/rename".into())
        );

        state.input.insert("next question");

        assert_eq!(
            press(&mut state, KeyCode::Enter, KeyModifiers::NONE, true),
            Action::Steer("next question".into())
        );
        assert!(state.input.text.is_empty());

        state.input.insert("/model");

        assert_eq!(
            press(&mut state, KeyCode::Enter, KeyModifiers::NONE, true),
            Action::Submit("/model".into())
        );
        assert_eq!(
            press(&mut state, KeyCode::Esc, KeyModifiers::NONE, true),
            Action::None
        );
        assert_eq!(
            press(&mut state, KeyCode::Char('c'), KeyModifiers::CONTROL, true),
            Action::Cancel
        );
    }

    #[test]
    fn steering_keeps_the_interrupted_user_task_in_the_next_request() {
        let history = vec![ChatMessage::system("rules")];
        let history = steering_history(&history, Some(ChatMessage::user("build the parser")));

        assert_eq!(history.len(), 2);
        assert!(matches!(
            &history[1],
            ChatMessage::User { content, .. } if content == "build the parser"
        ));
    }

    #[test]
    fn mouse_scrolls_transcript_without_touching_prompt_history() {
        let mut state = UiState::new("mock");
        state.max_scroll = 30;
        state.input.insert("previous prompt");
        state.input.take();
        state.input.insert("draft");

        for kind in [MouseEventKind::ScrollUp, MouseEventKind::ScrollDown] {
            keyboard_action(
                &mut state,
                Event::Mouse(crossterm::event::MouseEvent {
                    kind,
                    column: 5,
                    row: 5,
                    modifiers: KeyModifiers::NONE,
                }),
                false,
            );

            assert_eq!(state.input.text, "draft");

            if kind == MouseEventKind::ScrollUp {
                assert_eq!(state.scroll, wheel::LINES);
                assert!(!state.follow_bottom);
            }
        }

        assert_eq!(state.scroll, 0);
        assert!(state.follow_bottom);

        press(&mut state, KeyCode::Up, KeyModifiers::NONE, false);

        assert_eq!(state.input.text, "previous prompt");
    }

    #[test]
    fn plain_q_is_text_and_paste_does_not_submit() {
        let mut state = UiState::new("mock");

        assert_eq!(
            press(&mut state, KeyCode::Char('q'), KeyModifiers::NONE, false),
            Action::None
        );

        keyboard_action(&mut state, Event::Paste("\nhello\nworld".into()), false);

        assert_eq!(state.input.text, "q\nhello\nworld");
    }

    #[test]
    fn pasted_or_dropped_image_paths_become_attachments() {
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("screen shot.png");
        std::fs::write(&image, b"not decoded until it is imported").unwrap();
        let mut state = UiState::new("mock");

        assert_eq!(
            keyboard_action(
                &mut state,
                Event::Paste(image.to_string_lossy().into_owned()),
                false,
            ),
            Action::AttachPaths(vec![image.clone()])
        );
        assert!(state.input.text.is_empty());

        let file_url = url::Url::from_file_path(&image).unwrap().to_string();

        assert_eq!(
            pasted_image_paths(&file_url, temp.path()),
            Some(vec![image])
        );
    }

    #[test]
    fn clipboard_shortcuts_request_the_system_clipboard() {
        let mut state = UiState::new("mock");

        for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            assert_eq!(
                press(&mut state, KeyCode::Char('v'), modifiers, false),
                Action::PasteClipboard
            );
        }
    }

    fn image(name: &str) -> orbit_model::types::Attachment {
        orbit_model::types::Attachment::Image {
            name: name.into(),
            mime_type: "image/png".into(),
            path: PathBuf::from(name),
        }
    }

    #[test]
    fn attachments_are_atomic_editable_input_items() {
        let mut state = UiState::new("mock");
        state.attachments = vec![image("first.png"), image("second.png")];
        state.input.set("caption".into());
        state.input.home();

        press(&mut state, KeyCode::Left, KeyModifiers::NONE, false);

        assert_eq!(state.attachment_cursor, Some(1));

        press(&mut state, KeyCode::Left, KeyModifiers::NONE, false);

        assert_eq!(state.attachment_cursor, Some(0));

        press(&mut state, KeyCode::Delete, KeyModifiers::NONE, false);

        assert_eq!(state.attachments, vec![image("second.png")]);
        assert_eq!(state.attachment_cursor, Some(0));

        press(&mut state, KeyCode::Right, KeyModifiers::NONE, false);

        assert_eq!(state.attachment_cursor, None);
        assert_eq!(state.input.cursor, 0);

        press(&mut state, KeyCode::Backspace, KeyModifiers::NONE, false);

        assert!(state.attachments.is_empty());

        press(&mut state, KeyCode::Right, KeyModifiers::NONE, false);

        assert_eq!(state.input.cursor, 1);
    }

    #[test]
    fn attachment_click_focuses_it_and_image_only_messages_can_submit() {
        let mut state = UiState::new("mock");
        state.attachments.push(image("only.png"));
        state
            .attachment_areas
            .push(ratatui::layout::Rect::new(4, 5, 20, 1));

        assert!(focus_attachment_at(&mut state, 8, 5));
        assert_eq!(state.attachment_cursor, Some(0));
        assert_eq!(
            press(&mut state, KeyCode::Enter, KeyModifiers::NONE, false),
            Action::Submit(String::new())
        );
        assert_eq!(state.attachment_cursor, None);
    }

    #[tokio::test]
    async fn text_only_model_blocks_queuing_and_attaching_images() {
        let mut state = UiState::new("mock");
        state.supports_vision = false;

        assert!(!queue_attachment(&mut state, image("test.png")));
        assert!(state.attachments.is_empty());

        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("orbit.db");
        let session = SessionState::open(db_path, None, "test::model".into())
            .await
            .unwrap();

        attach_paths(&mut state, &session, vec![PathBuf::from("test.png")]).unwrap();

        assert!(state.attachments.is_empty());
        assert!(state.transcript.iter().any(|item| matches!(
            item,
            TranscriptItem::Notice(msg) if msg.contains("当前模型未启用图片输入，未添加图片附件")
        )));

        // With vision enabled, queue_attachment works
        state.supports_vision = true;

        assert!(queue_attachment(&mut state, image("test.png")));
        assert_eq!(state.attachments.len(), 1);
    }
}
