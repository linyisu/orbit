pub mod app;
mod approval;
mod approval_diff;
mod changes;
pub mod commands;
mod dialogs;
mod frames;
pub mod input;
mod markdown;
mod navigation;
mod overlay;
mod permissions;
pub mod render;
mod selection;
pub mod terminal;
mod tool_display;
mod transcript;
mod wheel;

/// Nerd Font nf-fa-code. Reserve a following blank cell when drawing it in a
/// frame so terminals can render the wide pictogram without shrinking it.
pub(crate) const COMMAND_ICON: &str = "";

use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use futures_util::StreamExt;
use orbit_agent::{
    agent::{Agent, AgentConfig, AgentResult},
    error::AgentError,
    events::AgentEvent,
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

use self::{
    app::{TranscriptItem, UiState},
    terminal::TerminalGuard,
};

type RunTask = JoinHandle<Result<AgentResult, AgentError>>;

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
    memory_scope: &'a crate::project_state::MemoryScope,
}

#[derive(Clone, Debug)]
pub struct ModelOption {
    pub provider: String,
    pub model: String,
    /// Request budget passed to the agent, including the output upper bound.
    pub context_tokens: usize,
    /// Full usable input window shown to the user and used for compaction.
    pub input_context_tokens: usize,
    pub default_for_provider: bool,
    pub reasoning_efforts: Vec<orbit_model::types::ReasoningEffort>,
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
    pub memory_scope: crate::project_state::MemoryScope,
}

pub async fn run(
    mut model: ModelClient,
    model_name: String,
    tools: ToolRegistry,
    mut session: SessionState,
    undo_writer: orbit_tools::write_file::WriteFile,
    mut approvals: orbit_tools::approval::ApprovalReceiver,
    budget: RequestBudget,
) -> Result<()> {
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    let mut state = UiState::new(model_name);
    let RequestBudget {
        context_tokens,
        input_context_tokens,
        auto_compact,
        max_output_tokens,
        max_turns,
        max_tool_calls,
        instructions,
        skills,
        plugin_skill_diagnostics,
        mcp_statuses,
        model_options,
        memory_scope,
    } = budget;
    state.context_tokens = context_tokens;
    state.input_context_tokens = input_context_tokens;
    state.auto_compact = auto_compact;
    state.max_output_tokens = max_output_tokens;
    state.reasoning_effort = model.config().reasoning_effort;
    state.model_completions = model_options
        .iter()
        .map(ModelOption::qualified_name)
        .collect();
    session.restore_ui(&mut state);
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
    let mut events = EventStream::new();
    let mut ticks = time::interval(Duration::from_millis(33));
    ticks.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut receiver: Option<mpsc::Receiver<AgentEvent>> = None;
    let mut cancellation: Option<CancellationToken> = None;
    let mut task: Option<RunTask> = None;
    let mut local_task: Option<
        tokio::task::JoinHandle<Result<String, orbit_agent::tool::ToolError>>,
    > = None;
    let mut local_cancellation: Option<CancellationToken> = None;
    let mut marketplace_task: Option<JoinHandle<Result<crate::marketplaces::Marketplace>>> = None;
    let mut pending_guidance = Vec::new();
    let mut active_context: Option<ChatMessage> = None;
    let mut pending: Option<orbit_tools::approval::ApprovalRequest> = None;
    let mut frames = frames::Frames::default();
    let mut wheel = wheel::Wheel::default();

    let result: Result<()> = async {
        let mut dirty = true;
        loop {
            select! {
                event = events.next() => {
                    let Some(event) = event else { break };
                    let event = event?;
                    if !wheel.accept(&event, std::time::Instant::now()) { continue; }
                    let context_hover_changed = !state.selection.dragging()
                        && matches!(render::handle_context_hover(&mut state, &event), Some(true));
                    let link_hover_changed = !state.selection.dragging()
                        && !approval::file_modal(&state)
                        && !state.show_help
                        && state.picker.is_none()
                        && state.model_picker.is_none()
                        && state.marketplace_dialog.is_none()
                        && matches!(render::handle_link_hover(&mut state, &event), Some(true));
                    if !state.selection.dragging() && !approval::file_modal(&state) && !state.show_help && state.picker.is_none() && state.model_picker.is_none() && state.marketplace_dialog.is_none()
                        && let Some(changed) = navigation::handle(&mut state, &event) {
                        if changed || context_hover_changed || link_hover_changed { dirty = true; frames.request(time::Instant::now()); }
                        continue;
                    }
                    if context_hover_changed || link_hover_changed { dirty = true; frames.request(time::Instant::now()); continue; }
                    // Unused pointer motion must not trigger full-frame redraws.
                    if matches!(&event, Event::Mouse(mouse) if mouse.kind == MouseEventKind::Moved) { continue; }
                    dirty = true;
                    frames.request(time::Instant::now());
                    if state.show_help || state.picker.is_some() || state.model_picker.is_some() || state.marketplace_dialog.is_some() {
                        if let Some(id) = dialogs::handle(&mut state, event)
                            && let Err(error) = resume_session(&id, &mut state, &mut model, &mut history, &mut session, &instructions, &skills, &model_options, &memory_scope, &tools).await
                            && let Some(picker) = &mut state.picker {
                            picker.error = Some(error.to_string());
                        }
                        if let Some(name) = state.chosen_model.take() {
                            if let Err(error) = select_model(
                                &name,
                                state.chosen_effort.take(),
                                &mut state,
                                &mut model,
                                &model_options,
                                &mut session,
                            ).await {
                                state.transcript.push(TranscriptItem::Error(error.to_string()));
                            }
                        }
                        if let Some(source) = state.pending_marketplace_source.take() {
                            state.transcript.push(TranscriptItem::Command(format!("/plugins add {source}")));
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
                        && toggle_tool_at(&mut state, (x, y)) {
                        continue;
                    }
                    if let Some((x, y)) = state.selection.click(&event)
                        && state.input_area.contains((x, y).into()) {
                        state.attachment_cursor = None;
                        state.input.move_to((x - state.input_area.x) as usize,
                            (y - state.input_area.y) as usize + state.input_scroll,
                            state.input_width);
                    }
                    if state.selection.handle(&event)? {
                        if state.selection.transcript_drag() { state.follow_bottom = false; }
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
                        if state.selection.dragging() { keyboard_action(&mut state, event, task.is_some() || local_task.is_some()); }
                        else if pending.is_some() { approval::handle(&mut state, event); }
                        else { keyboard_action(&mut state, event, task.is_some() || local_task.is_some()); }
                        continue;
                    }
                    if matches!(&event, Event::Key(key) if key.code == KeyCode::BackTab && key.kind != KeyEventKind::Release) {
                        state.permission_mode = state.permission_mode.next();
                        if state.approval_shown && pending.as_ref().is_some_and(|request| state.permission_mode.allows(&request.preview)) {
                            if let Some(request) = pending.take() { let _ = request.decision.send(true); }
                            state.approval = None;
                        }
                        continue;
                    }
                    if pending.is_some() {
                        match approval::handle(&mut state, event) {
                            approval::Action::Approve => {
                                if let Some(request) = pending.take() { let _ = request.decision.send(true); }
                                state.approval = None;
                            }
                            approval::Action::Deny => {
                                if let Some(request) = pending.take() { let _ = request.decision.send(false); }
                                state.approval = None;
                            }
                            approval::Action::Cancel => {
                                if let Some(request) = pending.take() { let _ = request.decision.send(false); }
                                state.approval = None;
                                state.cancel();
                                if let Some(token) = &cancellation { token.cancel(); }
                                if let Some(token) = &local_cancellation { token.cancel(); }
                            }
                            approval::Action::None => {}
                        }
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
                            if local_task.is_some() {
                                state.transcript.push(TranscriptItem::Command(prompt));
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
                                state.transcript.push(TranscriptItem::Command(prompt.clone()));
                                if command == "/undo" {
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
                                        "运行中不能压缩上下文；请先按 Esc 取消当前响应。".into(),
                                    ));
                                    continue;
                                }
                                if !compacting {
                                    if task.is_some()
                                        && !commands::available_while_running(command)
                                    {
                                        state.transcript.push(TranscriptItem::CommandError(
                                            "运行中只能使用 /status、/diagnostics、/model、/context 或 /mode；请先按 Esc 取消当前响应。".into(),
                                        ));
                                        continue;
                                    }
                                    match run_command(&prompt, &mut state, &mut model, &mut history, &mut session, CommandContext { tools: &tools, instructions: &instructions, skills: &skills, mcp_statuses: &mcp_statuses, model_options: &model_options, memory_scope: &memory_scope }).await {
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
                                max_output_tokens,
                                max_turns,
                                max_tool_calls,
                            )
                            .await?;
                            receiver = Some(started.receiver);
                            cancellation = Some(started.cancellation);
                            task = Some(started.task);
                        }
                        Action::None => {}
                    }
                }
                Some(request) = approvals.recv() => {
                    if request.decision.is_closed() { continue; }
                    if state.permission_mode.allows(&request.preview) {
                        let _ = request.decision.send(true);
                        continue;
                    }
                    state.selection = Default::default();
                    state.approval = Some(request.preview.clone());
                    state.approval_scroll = 0;
                    state.approval_expanded = false;
                    state.approval_shown = false;
                    state.approval_choice = 0;
                    state.approval_diff = None;
                    pending = Some(request);
                    dirty = true;
                }
                event = async { receiver.as_mut().unwrap().recv().await }, if receiver.is_some() => {
                    dirty = true;
                    match event {
                        Some(event) => { apply_live_event(event, &mut state, &mut session); session.flush_due().await?; },
                        None => receiver = None,
                    }
                }
                result = async { task.as_mut().unwrap().await }, if task.is_some() => {
                    dirty = true;
                    task = None;
                    pending = None;
                    if state.approval.take().is_some() { state.selection = Default::default(); }
                    // Drain the completed run before accepting another prompt or clearing history.
                    if let Some(rx) = receiver.as_mut() {
                        while let Ok(event) = rx.try_recv() { apply_live_event(event, &mut state, &mut session); }
                    }
                    let (status, final_event, completed_history) = match result {
                        Ok(Ok(result)) if matches!(&result.final_message, ChatMessage::Assistant { content, tool_calls } if content.as_deref().unwrap_or_default().trim().is_empty() && tool_calls.is_empty()) => {
                            ("failed", AgentEvent::Failed { error: "模型已结束响应，但未返回回答或工具调用；请检查服务端返回内容。".into() }, None)
                        }
                        Ok(Ok(result)) => {
                            ("completed", AgentEvent::Completed { turns: result.turns, tool_calls: result.tool_calls }, Some(result.history))
                        }
                        Ok(Err(_)) if cancellation.as_ref().is_some_and(CancellationToken::is_cancelled) => ("cancelled", AgentEvent::Cancelled, None),
                        Ok(Err(AgentError::Cancelled)) => ("cancelled", AgentEvent::Cancelled, None),
                        Ok(Err(error)) => ("failed", AgentEvent::Failed { error: error.to_string() }, None),
                        Err(error) => ("failed", AgentEvent::Failed { error: format!("后台任务中断：{error}") }, None),
                    };
                    session.record(&final_event);
                    session.finish(status, completed_history.clone()).await?;
                    if let Some(saved) = completed_history {
                        history = saved;
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
                    refresh_context_usage(&mut state, &history, &tools);
                    state.apply(final_event);
                    task = None;
                    receiver = None;
                    cancellation = None;
                    if !pending_guidance.is_empty() {
                        let guidance = pending_guidance.join("\n\n");
                        pending_guidance.clear();
                        let steering_history = steering_history(&history, active_context.take());
                        active_context = Some(ChatMessage::user(guidance.clone()));
                        let started = start_agent_run(
                            guidance,
                            Vec::new(),
                            false,
                            &mut state,
                            &mut session,
                            &steering_history,
                            &model,
                            &tools,
                            context_tokens,
                            auto_compact,
                            max_output_tokens,
                            max_turns,
                            max_tool_calls,
                        )
                        .await?;
                        receiver = Some(started.receiver);
                        cancellation = Some(started.cancellation);
                        task = Some(started.task);
                    } else {
                        active_context = None;
                    }
                }
                result = async { local_task.as_mut().unwrap().await }, if local_task.is_some() => {
                    dirty = true;
                    local_task = None;
                    local_cancellation = None;
                    match result {
                        Ok(Ok(result)) => state.transcript.push(TranscriptItem::CommandResult(format_undo_result(&result))),
                        Ok(Err(error)) => state.transcript.push(TranscriptItem::CommandError(error.to_string())),
                        Err(error) => state.transcript.push(TranscriptItem::CommandError(format!("回退操作中断：{error}"))),
                    }
                }
                result = async { marketplace_task.as_mut().unwrap().await }, if marketplace_task.is_some() => {
                    dirty = true;
                    marketplace_task = None;
                    match result {
                        Ok(Ok(market)) => state.transcript.push(TranscriptItem::CommandResult(format!(
                            "已添加市场 {}，发现 {} 个插件。\n{}",
                            market.name,
                            market.plugins.len(),
                            market.directory.display(),
                        ))),
                        Ok(Err(error)) => state.transcript.push(TranscriptItem::CommandError(format!("无法添加市场：{error}"))),
                        Err(error) => state.transcript.push(TranscriptItem::CommandError(format!("市场下载任务中断：{error}"))),
                    }
                }
                _ = time::sleep_until(frames.deadline()), if frames.pending() => {
                    terminal::draw_synchronized(&mut terminal, |frame| {
                        render::draw(frame, &mut state);
                        state.selection.paint(frame);
                    })?;
                    terminal::anchor_cursor(&mut terminal, state.ime_cursor)?;
                    frames.drawn(time::Instant::now());
                    dirty = false;
                }
                _ = ticks.tick() => {
                    let direction = state.selection.auto_scroll(std::time::Instant::now());
                    if direction != 0 {
                        if direction < 0 { state.scroll_up(2); } else { state.scroll_down(2); }
                        dirty = true;
                    }
                    if dirty || state.running() { frames.request(time::Instant::now()); }
                    session.flush_due().await?;
                }
            }
        }
        Ok(())
    }.await;

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
        while let Ok(event) = rx.try_recv() {
            session.record(&event);
        }
    }
    let saved = session.close().await;
    result.and(saved)
}

fn apply_live_event(event: AgentEvent, state: &mut UiState, session: &mut SessionState) {
    // The joined task is authoritative: persist terminal status with its context atomically.
    if matches!(
        event,
        AgentEvent::Completed { .. } | AgentEvent::Failed { .. } | AgentEvent::Cancelled
    ) {
        return;
    }
    session.record(&event);
    state.apply(event);
}

fn refresh_context_usage(state: &mut UiState, history: &[ChatMessage], tools: &ToolRegistry) {
    state.context_used_tokens =
        orbit_agent::context::estimate_tokens(history, &tools.definitions());
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
    max_output_tokens: u32,
    max_turns: usize,
    max_tool_calls: usize,
) -> Result<StartedRun> {
    session.begin(prompt.clone(), state.model.clone()).await?;
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
    let mut messages = history.to_vec();
    if !compacting {
        messages.push(ChatMessage::user_with_attachments(prompt, attachments));
    }
    state.context_used_tokens =
        orbit_agent::context::estimate_tokens(&messages, &tools.definitions());
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
    let agent = Agent::new(backend.clone(), tools.clone(), config)
        .with_file_activity(files.clone())
        .with_context_archive(archive.clone());
    let task = tokio::spawn(async move {
        if compacting {
            let compacted = orbit_agent::context::compact_with_archive(
                &backend,
                request.messages,
                &tools.definitions(),
                context_tokens,
                max_output_tokens as usize,
                token,
                &files,
                Some(archive.as_ref()),
            )
            .await?;
            orbit_agent::context::check_budget(
                &compacted.history,
                &tools.definitions(),
                max_output_tokens as usize,
                context_tokens,
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
        memory_scope,
    } = context;
    let (command, argument) = prompt
        .split_once(char::is_whitespace)
        .unwrap_or((prompt, ""));
    let argument = argument.trim();
    match command {
        "/exit" | "/quit" if argument.is_empty() => return Ok(true),
        "/new" if argument.is_empty() => {
            session.new_session(state.model.clone()).await?;
            memory_scope.set_session(session.id().into());
            session
                .set_model_profile(state.model.clone(), state.reasoning_effort)
                .await?;
            history.clear();
            crate::instructions::inject(history, instructions);
            skills.inject(history);
            crate::project_state::inject(
                history,
                &session.store,
                &session.snapshot.session.project_id,
                session.id(),
            )
            .await?;
            let input = std::mem::take(&mut state.input);
            let context_tokens = state.context_tokens;
            let input_context_tokens = state.input_context_tokens;
            let auto_compact = state.auto_compact;
            let max_output_tokens = state.max_output_tokens;
            *state = UiState::new(state.model.clone());
            state.context_tokens = context_tokens;
            state.input_context_tokens = input_context_tokens;
            state.auto_compact = auto_compact;
            state.max_output_tokens = max_output_tokens;
            state.input = input;
            session.restore_ui(state);
        }
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
                state.permission_mode = mode;
            }
            state.transcript.push(TranscriptItem::CommandResult(format!("{}\nShift+Tab 切换 · /mode ask|auto|YOLO\n仅本会话有效；重启、恢复或 /new 后回到 auto。",state.permission_mode.description())));
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
            state.transcript.push(TranscriptItem::CommandResult(format!("上下文总计：约 {estimated} tokens（含协议开销）\n系统提示：约 {system_tokens} tokens\n项目指令：{sources} · 约 {project_instruction_tokens} tokens {truncation}\n{}\n历史消息：约 {conversation_tokens} tokens\n工具定义：约 {tool_tokens} tokens\n代码概览工具结果：约 {overview_tokens} tokens\n任务与会话记忆：约 {project_state_tokens} tokens\n输入窗口：{} tokens · 输出上限：{} tokens · 请求预算：{} tokens\n自动整理：{}（输入窗口的 85% 触发，目标约 50%）\n这是通用估算，不等于服务端实际用量；分类值含各自协议开销，不能相加为总计。\n自动整理优先生成结构化摘要；摘要不可用时保留最新工作和文件活动，较早记录不再发送给模型。\n/compact 手动整理旧对话并保留最近两轮。", instructions.source_summary(), state.input_context_tokens, state.max_output_tokens,state.context_tokens,if state.auto_compact { "开启" } else { "关闭" })));
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
            state
                .transcript
                .push(TranscriptItem::CommandResult(changes::list(
                    &session.store.workspace_changes(session.id()).await?,
                )))
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
            resume_session(
                argument,
                state,
                model,
                history,
                session,
                instructions,
                skills,
                model_options,
                memory_scope,
                tools,
            )
            .await?
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
                    "验证命令  {} 次 · 通过 {} · 未通过 {} · 最近 exit {} · {} ms",
                    verifications.len(),
                    passed,
                    failed,
                    last.exit_code
                        .map_or_else(|| "无".to_owned(), |code| code.to_string()),
                    last.elapsed_ms,
                )
            } else {
                "验证命令  尚未执行沙箱命令".to_owned()
            };
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
            (!task.details.is_empty())
                .then(|| format!("\n        {}", task.details))
                .unwrap_or_default(),
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
            state.context_tokens = choice.context_tokens;
            state.input_context_tokens = choice.input_context_tokens;
            state.reasoning_effort = reasoning_effort;
            if previous_model != state.model {
                state.transcript.push(TranscriptItem::CommandResult(format!(
                    "{previous_model} → {}",
                    state.model
                )));
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

fn resolved_image_path(value: &str) -> Option<PathBuf> {
    let path = attachment_path(value);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp")
        .then_some(path)
        .filter(|path| path.is_file())
}

fn pasted_image_paths(text: &str) -> Option<Vec<PathBuf>> {
    if let Some(path) = resolved_image_path(text) {
        return Some(vec![path]);
    }
    #[cfg(unix)]
    {
        let paths = shlex::split(text)?
            .into_iter()
            .map(|part| resolved_image_path(&part))
            .collect::<Option<Vec<_>>>()?;
        (!paths.is_empty()).then_some(paths)
    }
    #[cfg(not(unix))]
    None
}

fn queue_attachment(state: &mut UiState, attachment: orbit_model::types::Attachment) -> bool {
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
            let attachment = session.import_clipboard_image(width, height, bytes)?;
            queue_attachment(state, attachment);
        }
        ClipboardPayload::Files(files) => {
            let images = files
                .iter()
                .filter_map(|path| resolved_image_path(&path.to_string_lossy()))
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
            if let Some(paths) = pasted_image_paths(&text) {
                attach_paths(state, session, paths)?;
            } else {
                state.input.insert(&text);
            }
        }
    }
    state.menu_dismissed = false;
    Ok(())
}

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
    session.resume(id).await?;
    memory_scope.set_session(session.id().into());
    *history = session.snapshot.history.clone();
    crate::instructions::inject(history, instructions);
    skills.inject(history);
    crate::project_state::inject(
        history,
        &session.store,
        &session.snapshot.session.project_id,
        session.id(),
    )
    .await?;
    let mut restored = UiState::new(state.model.clone());
    restored.context_tokens = state.context_tokens;
    restored.input_context_tokens = state.input_context_tokens;
    restored.auto_compact = state.auto_compact;
    restored.max_output_tokens = state.max_output_tokens;
    restored.model_completions = state.model_completions.clone();
    session.restore_ui(&mut restored);
    *state = restored;
    restore_model_profile(state, model, model_options, session);
    refresh_context_usage(state, history, tools);
    Ok(())
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
    state.context_tokens = choice.context_tokens;
    state.input_context_tokens = choice.input_context_tokens;
    state.reasoning_effort = model.config().reasoning_effort;
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

fn toggle_tool_at(state: &mut UiState, point: (u16, u16)) -> bool {
    let offset = state.max_scroll.saturating_sub(state.scroll);
    let Some(index) = state.transcript_layout.tool_toggle_at(
        &state.transcript,
        state.selection.viewport,
        offset,
        point,
    ) else {
        return false;
    };
    toggle_tool(state, index);
    true
}

fn toggle_tool(state: &mut UiState, index: usize) {
    let offset = state.max_scroll.saturating_sub(state.scroll);
    let anchor = state.transcript_layout.anchor_at(offset);
    let collapsing = state.expanded == Some(index);
    let viewport_in_collapsing_tool = state.expanded.is_some_and(|old| {
        let start = state.transcript_layout.item_offset(old).unwrap_or(offset);
        let end = state
            .transcript_layout
            .item_offset(old + 1)
            .unwrap_or_else(|| state.transcript_layout.height());
        (start..end).contains(&offset)
    });
    state.expanded = if collapsing { None } else { Some(index) };
    state.follow_bottom = false;
    state.selection.clear();
    state.transcript_layout.update(
        &state.transcript,
        state.expanded,
        state.selection.viewport.width,
    );
    state.max_scroll = state
        .transcript_layout
        .height()
        .saturating_sub(usize::from(state.selection.viewport.height));
    let target = if viewport_in_collapsing_tool {
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
        if let Some(paths) = pasted_image_paths(&text) {
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
                if let Some(index) = state.expanded.or_else(|| {
                    state.transcript_layout.visible_tool(
                        &state.transcript,
                        offset,
                        state.selection.viewport.height as usize,
                    )
                }) {
                    toggle_tool(state, index);
                }
            }
            _ => {}
        }
        return Action::None;
    }
    if key.code == KeyCode::Esc {
        if busy {
            return Action::Cancel;
        }
        state.attachment_cursor = None;
        state.show_help = false;
        state.menu_dismissed = true;
        return Action::None;
    }
    if key.code == KeyCode::Tab && state.input.text.starts_with("/model ") {
        let typed = state.input.text[7..].trim();
        let current_provider = state.model.split_once("::").map(|(provider, _)| provider);
        let matches = state
            .model_completions
            .iter()
            .filter(|candidate| {
                if typed.contains("::") {
                    candidate.starts_with(typed)
                } else {
                    candidate.split_once("::").is_some_and(|(provider, model)| {
                        model.starts_with(typed) && Some(provider) == current_provider
                    })
                }
            })
            .collect::<Vec<_>>();
        if !matches.is_empty() {
            let index = state.model_completion_index % matches.len();
            let completion = if typed.contains("::") {
                matches[index].clone()
            } else {
                matches[index]
                    .split_once("::")
                    .map(|(_, model)| model)
                    .unwrap_or(matches[index])
                    .to_owned()
            };
            state.input.set(format!("/model {completion}"));
            state.model_completion_index = state.model_completion_index.saturating_add(1);
        }
        return Action::None;
    }
    if key.code == KeyCode::Char('v') && key.modifiers.contains(KeyModifiers::ALT) {
        return Action::PasteClipboard;
    }
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
                state.input.set(menu[state.menu_index].name.to_owned());
                return Action::None;
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                state.input.set(menu[state.menu_index].name.to_owned());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

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
            provider: "test".into(),
            model: "model".into(),
            context_tokens: 232_000,
            input_context_tokens: 224_000,
            default_for_provider: true,
            reasoning_efforts: vec![],
            config: Default::default(),
        }];
        let mut tools = ToolRegistry::new();
        tools.register(orbit_agent::tool::ClosureTool::new(
            "test_tool",
            "test tool description",
            serde_json::json!({"type":"object"}),
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
    fn menu_selects_commands_and_busy_enter_routes_commands_locally() {
        let mut state = UiState::new("mock");
        state.input.insert("/");
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
        assert_eq!(pasted_image_paths(&file_url), Some(vec![image]));
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
}
