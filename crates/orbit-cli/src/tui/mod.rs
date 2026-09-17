pub mod app;
mod approval;
pub mod commands;
mod demo;
mod dialogs;
mod frames;
pub mod input;
mod markdown;
mod navigation;
mod permissions;
pub mod render;
mod selection;
pub mod terminal;
mod transcript;
mod wheel;

use std::{path::PathBuf, time::Duration};

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
    types::{ChatMessage, ChatRequest},
};
use tokio::{select, sync::mpsc, task::JoinHandle, time};
use tokio_util::sync::CancellationToken;

use crate::session::SessionState;

use self::{
    app::{TranscriptItem, UiState},
    terminal::TerminalGuard,
};

type RunTask = JoinHandle<Result<AgentResult, AgentError>>;

struct CommandContext<'a> {
    tools: &'a ToolRegistry,
    instructions: &'a crate::instructions::Instructions,
    model_options: &'a [ModelOption],
}

#[derive(Clone, Debug)]
pub struct ModelOption {
    pub provider: String,
    pub model: String,
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
    pub auto_compact: bool,
    pub max_output_tokens: u32,
    pub max_turns: usize,
    pub max_tool_calls: usize,
    pub instructions: crate::instructions::Instructions,
    pub model_options: Vec<ModelOption>,
}

pub async fn run(
    mut model: ModelClient,
    model_name: String,
    demo: bool,
    tools: ToolRegistry,
    mut session: SessionState,
    mut approvals: orbit_tools::approval::ApprovalReceiver,
    budget: RequestBudget,
) -> Result<()> {
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    let mut state = UiState::new(model_name);
    state.demo = demo;
    let RequestBudget {
        context_tokens,
        auto_compact,
        max_output_tokens,
        max_turns,
        max_tool_calls,
        instructions,
        model_options,
    } = budget;
    state.context_tokens = context_tokens;
    state.auto_compact = auto_compact;
    state.max_output_tokens = max_output_tokens;
    state.reasoning_effort = model.config().reasoning_effort;
    state.model_completions = model_options
        .iter()
        .map(ModelOption::qualified_name)
        .collect();
    session.restore_ui(&mut state);
    let mut history = session.snapshot.history.clone();
    crate::instructions::inject(&mut history, &instructions);
    let mut events = EventStream::new();
    let mut ticks = time::interval(Duration::from_millis(33));
    ticks.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut receiver: Option<mpsc::Receiver<AgentEvent>> = None;
    let mut cancellation: Option<CancellationToken> = None;
    let mut task: Option<RunTask> = None;
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
                    if !state.selection.dragging() && !state.show_help && state.picker.is_none() && state.model_picker.is_none()
                        && let Some(changed) = navigation::handle(&mut state, &event) {
                        if changed { dirty = true; frames.request(time::Instant::now()); }
                        continue;
                    }
                    // Unused pointer motion must not trigger full-frame redraws.
                    if matches!(&event, Event::Mouse(mouse) if mouse.kind == MouseEventKind::Moved) { continue; }
                    dirty = true;
                    frames.request(time::Instant::now());
                    if state.show_help || state.picker.is_some() || state.model_picker.is_some() {
                        if let Some(id) = dialogs::handle(&mut state, event)
                            && let Err(error) = resume_session(&id, &mut state, &mut history, &mut session, &instructions).await
                            && let Some(picker) = &mut state.picker {
                            picker.error = Some(error.to_string());
                        }
                        if let Some(name) = state.chosen_model.take() {
                            select_model(
                                &name,
                                state.chosen_effort.take(),
                                &mut state,
                                &mut model,
                                &model_options,
                            );
                        }
                        continue;
                    }
                    if let Event::Mouse(mouse) = &event
                        && mouse.kind == crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left)
                        && focus_attachment_at(&mut state, mouse.column, mouse.row) {
                        state.selection.clear();
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
                        if state.selection.dragging() { keyboard_action(&mut state, event, task.is_some()); }
                        else if pending.is_some() { approval::handle(&mut state, event); }
                        else { keyboard_action(&mut state, event, task.is_some()); }
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
                                pending = None;
                                state.approval = None;
                                state.cancel();
                                if let Some(token) = &cancellation { token.cancel(); }
                            }
                            approval::Action::None => {}
                        }
                        continue;
                    }
                    match keyboard_action(&mut state, event, task.is_some()) {
                        Action::Quit => break,
                        Action::Redraw => terminal.clear()?,
                        Action::Cancel => {
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
                            let compacting = prompt.trim() == "/compact";
                            if prompt.starts_with('/') && !compacting {
                                match run_command(&prompt, &mut state, &mut model, &mut history, &mut session, CommandContext { tools: &tools, instructions: &instructions, model_options: &model_options }).await {
                                    Ok(true) => break,
                                    Ok(false) => {},
                                    Err(error) => state.transcript.push(TranscriptItem::Error(error.to_string())),
                                }
                                continue;
                            }
                            let attachments = if compacting {
                                Vec::new()
                            } else {
                                state.attachment_cursor = None;
                                std::mem::take(&mut state.attachments)
                            };
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
                            state.submit(display_prompt);
                            let mut messages = history.clone();
                            if !compacting {
                                messages.push(ChatMessage::user_with_attachments(prompt, attachments));
                            }
                            let mut request = ChatRequest::new(messages);
                            request.max_tokens = Some(max_output_tokens);
                            request.reasoning_effort = model.config().reasoning_effort;
                            let (sender, rx) = mpsc::channel(256);
                            receiver = Some(rx);
                            let token = CancellationToken::new();
                            cancellation = Some(token.clone());
                            let tools = tools.clone();
                            let backend = ModelClientBackend::new(model.clone());
                            let files = orbit_agent::memory::FileActivity::from_events(session.snapshot.runs.iter().flat_map(|run| &run.events));
                            let config = AgentConfig { context_tokens, max_turns, max_tool_calls, auto_compact };
                            let agent = Agent::new(backend.clone(), tools.clone(), config.clone()).with_file_activity(files.clone());
                            task = Some(tokio::spawn(async move {
                                if compacting {
                                    let compacted = if demo {
                                        orbit_agent::context::compact_with_files(&self::demo::DemoModel, request.messages, &tools.definitions(), context_tokens, token, &files).await?
                                    } else {
                                        orbit_agent::context::compact_with_files(&backend, request.messages, &tools.definitions(), context_tokens, token, &files).await?
                                    };
                                    orbit_agent::context::check_budget(&compacted.history, &tools.definitions(), max_output_tokens as usize, context_tokens)?;
                                    sender.send(AgentEvent::CompactionRecorded { record: compacted.record }).await.map_err(|_| AgentError::EventChannelClosed)?;
                                    let report = format!("上下文已压缩：估算 {} → {} tokens。保留最近两轮完整对话，原始记录仍在。\n\n{}", compacted.before_tokens, compacted.after_tokens, compacted.summary);
                                    sender.send(AgentEvent::Model(orbit_model::types::StreamEvent::TextDelta(report.clone()))).await.map_err(|_| AgentError::EventChannelClosed)?;
                                    return Ok(AgentResult { final_message: ChatMessage::assistant(report), history: compacted.history, turns: 1, tool_calls: 0 });
                                }
                                if demo {
                                    Agent::new(self::demo::DemoModel, tools, config).with_file_activity(files)
                                        .run_with_agent_events(request, token, sender).await
                                } else {
                                    agent.run_with_agent_events(request, token, sender).await
                                }
                            }));
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
                    }
                    state.apply(final_event);
                    task = None;
                    receiver = None;
                    cancellation = None;
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
    if let Some(mut task) = task
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
        model_options,
    } = context;
    let (command, argument) = prompt
        .split_once(char::is_whitespace)
        .unwrap_or((prompt, ""));
    let argument = argument.trim();
    match command {
        "/exit" | "/quit" if argument.is_empty() => return Ok(true),
        "/new" if argument.is_empty() => {
            session.new_session(state.model.clone(), state.demo).await?;
            history.clear();
            crate::instructions::inject(history, instructions);
            let input = std::mem::take(&mut state.input);
            let demo = state.demo;
            let context_tokens = state.context_tokens;
            let auto_compact = state.auto_compact;
            let max_output_tokens = state.max_output_tokens;
            *state = UiState::new(state.model.clone());
            state.demo = demo;
            state.context_tokens = context_tokens;
            state.auto_compact = auto_compact;
            state.max_output_tokens = max_output_tokens;
            state.input = input;
            session.restore_ui(state);
        }
        "/attach" if argument.is_empty() => state.transcript.push(TranscriptItem::Error(
            "用法：/attach <图片路径>；支持 PNG、JPEG、GIF、WebP".into(),
        )),
        "/attach" => {
            let path = attachment_path(argument);
            let path = if path.is_absolute() {
                path
            } else {
                std::env::current_dir()?.join(path)
            };
            let attachment = session.import_image(&path)?;
            let name = attachment.name().to_owned();
            if !queue_attachment(state, attachment) {
                state.transcript.push(TranscriptItem::Notice(format!(
                    "图片 {name} 已在待发送列表中。"
                )));
            } else {
                state.transcript.push(TranscriptItem::Notice(format!(
                    "已添加图片 {name} · 随下一条消息发送 · /detach 清空"
                )));
            }
        }
        "/detach" if argument.is_empty() => {
            let count = state.attachments.len();
            state.attachments.clear();
            state.attachment_cursor = None;
            state.transcript.push(TranscriptItem::Notice(format!(
                "已清空 {count} 个待发送图片。"
            )));
        }
        "/mode" | "/permissions" => {
            if !argument.is_empty() {
                let Some(mode) = permissions::Mode::parse(argument) else {
                    state.transcript.push(TranscriptItem::Error("用法：/mode ask | auto | yolo".into()));
                    return Ok(false);
                };
                state.permission_mode = mode;
            }
            state.transcript.push(TranscriptItem::Notice(format!("{}\nShift+Tab 切换 · /mode ask|auto|yolo\n仅本会话有效；重启、恢复或 /new 后回到 auto。",state.permission_mode.description())));
        }
        "/context" if argument.is_empty() => {
            let estimated = orbit_agent::context::estimate_tokens(history, &tools.definitions());
            let system = history
                .iter()
                .filter(|message| matches!(message, ChatMessage::System { .. }) && !crate::instructions::is_orbit_instruction(message))
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
            let project_instruction_tokens = orbit_agent::context::estimate_tokens(&project_rules, &[]);
            let conversation_tokens = orbit_agent::context::estimate_tokens(&conversation, &[]);
            let tool_tokens = orbit_agent::context::estimate_tokens(&[], &tools.definitions());
            let sources = if instructions.sources.is_empty() { "未找到".to_owned() } else { format!("{} 条", instructions.sources.len()) };
            let truncation = if instructions.truncated { "（达到 32 KiB 上限，后续文件未加载）" } else { "" };
            state.transcript.push(TranscriptItem::Notice(format!("上下文总计：约 {estimated} tokens（含协议开销）\n系统提示：约 {system_tokens} tokens\n项目指令：{sources} · 约 {project_instruction_tokens} tokens {truncation}\n{}\n历史消息：约 {conversation_tokens} tokens\n工具定义：约 {tool_tokens} tokens\n项目概览：尚未建立（0 tokens）\n输出上限及预留：{} tokens · 总预算：{}\n自动压缩：{}（可用输入预算的 85% 触发）\n这是通用估算，不等于服务端实际用量；分类值含各自协议开销，不能相加为总计。\n/compact 总结旧对话并保留最近两轮；真实模式会调用模型，演示模式生成机械提要。", instructions.source_summary(), state.max_output_tokens,state.context_tokens,if state.auto_compact { "开启" } else { "关闭" })));
        }
        "/resume" | "/sessions" if argument.is_empty() => {
            state.picker = Some(dialogs::SessionPicker::new(session.resumable().await?));
            state.selection = Default::default();
        }
        "/resume" => resume_session(argument, state, history, session, instructions).await?,
        "/status" if argument.is_empty() => state.transcript.push(TranscriptItem::Notice(format!(
            "会话  {}\n数据库  {}\n会话工作目录  {}（沙箱内 /scratch）\n模型  {}\n模式  {}\n上下文  {} 条消息 · 已持久化\n项目指令  {} 条\n超时  {}s（流空闲）\n工具  read_file / read_image / read_document / search_text / list_files / git_inspect / web_fetch / write_file / edit_file / run_command · 审批：{}",
            session.id(), session.store.path().display(), session.scratch.read().expect("scratch lock poisoned").display(), state.model, if state.demo { "离线演示" } else { "真实 API" }, history.len(), instructions.sources.len(), model.config().timeout.as_secs(), state.permission_mode.label()
        ))),
        "/model" if state.demo => state.transcript.push(TranscriptItem::Notice("当前为演示模式，不连接真实模型。退出后移除 --demo 即可使用配置中的 API。".into())),
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
                    model_options.iter().find(|choice| choice.provider == provider && choice.model == name)
                } else {
                    model_options.iter().find(|choice| {
                        Some(choice.provider.as_str()) == current_provider && choice.model == argument
                    })
                };
                if let Some(choice) = choice {
                    select_model(
                        &choice.qualified_name(),
                        None,
                        state,
                        model,
                        model_options,
                    );
                } else {
                    state.transcript.push(TranscriptItem::Error(format!("未找到模型 {argument:?}。使用 /model 打开选择器；不带供应商时只在当前供应商中查找。")));
                }
            }
        }
        _ => state.transcript.push(TranscriptItem::Notice(format!("无法识别命令：{prompt}\n输入 / 查看可用命令。"))),
    }
    state.follow_bottom = true;
    state.menu_dismissed = false;
    Ok(false)
}

fn select_model(
    qualified_name: &str,
    selected_effort: Option<orbit_model::types::ReasoningEffort>,
    state: &mut UiState,
    model: &mut ModelClient,
    model_options: &[ModelOption],
) {
    let Some(choice) = model_options
        .iter()
        .find(|choice| choice.qualified_name() == qualified_name)
    else {
        state
            .transcript
            .push(TranscriptItem::Error("所选模型已不在当前配置中。".into()));
        return;
    };
    let mut config = choice.config.clone();
    if let Some(effort) = selected_effort {
        config.reasoning_effort = choice.reasoning_efforts.contains(&effort).then_some(effort);
    }
    match ModelClient::new(config) {
        Ok(client) => {
            let previous_model = state.model.clone();
            *model = client;
            state.model = choice.qualified_name();
            state.reasoning_effort = model.config().reasoning_effort;
            if previous_model != state.model {
                state.transcript.push(TranscriptItem::ModelChanged(format!(
                    "{previous_model} → {}",
                    state.model
                )));
            }
        }
        Err(error) => state
            .transcript
            .push(TranscriptItem::Error(error.to_string())),
    }
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

async fn resume_session(
    id: &str,
    state: &mut UiState,
    history: &mut Vec<ChatMessage>,
    session: &mut SessionState,
    instructions: &crate::instructions::Instructions,
) -> Result<()> {
    session.resume(id).await?;
    *history = session.snapshot.history.clone();
    crate::instructions::inject(history, instructions);
    let mut restored = UiState::new(state.model.clone());
    restored.demo = state.demo;
    restored.context_tokens = state.context_tokens;
    restored.auto_compact = state.auto_compact;
    restored.max_output_tokens = state.max_output_tokens;
    session.restore_ui(&mut restored);
    *state = restored;
    Ok(())
}

#[derive(Debug, PartialEq)]
enum Action {
    None,
    Quit,
    Cancel,
    Redraw,
    AttachPaths(Vec<PathBuf>),
    PasteClipboard,
    Submit(String),
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
                if let Some(index) = state.transcript_layout.visible_tool(
                    &state.transcript,
                    offset,
                    state.selection.viewport.height as usize,
                ) {
                    state.expanded = if state.expanded == Some(index) {
                        None
                    } else {
                        Some(index)
                    };
                    state.selection = Default::default();
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
            KeyCode::Enter if key.modifiers.is_empty() && !busy => {
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
        KeyCode::Enter if busy => {}
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

    fn press(state: &mut UiState, code: KeyCode, modifiers: KeyModifiers, busy: bool) -> Action {
        keyboard_action(state, Event::Key(KeyEvent::new(code, modifiers)), busy)
    }

    #[test]
    fn menu_selects_commands_and_busy_enter_preserves_draft() {
        let mut state = UiState::new("mock");
        state.input.insert("/");
        press(&mut state, KeyCode::Down, KeyModifiers::NONE, false);
        assert_eq!(
            press(&mut state, KeyCode::Enter, KeyModifiers::NONE, false),
            Action::Submit("/status".into())
        );
        state.input.insert("next question");
        assert_eq!(
            press(&mut state, KeyCode::Enter, KeyModifiers::NONE, true),
            Action::None
        );
        assert_eq!(state.input.text, "next question");
        assert_eq!(
            press(&mut state, KeyCode::Esc, KeyModifiers::NONE, true),
            Action::Cancel
        );
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
