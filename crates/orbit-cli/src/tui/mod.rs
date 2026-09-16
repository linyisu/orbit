pub mod app;
mod approval;
pub mod commands;
mod demo;
pub mod input;
pub mod render;
mod selection;
pub mod terminal;

use std::time::Duration;

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

pub async fn run(
    mut model: ModelClient,
    model_name: String,
    demo: bool,
    tools: ToolRegistry,
    mut session: SessionState,
    mut approvals: orbit_tools::approval::ApprovalReceiver,
    context_tokens: usize,
) -> Result<()> {
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    let mut state = UiState::new(model_name);
    state.demo = demo;
    state.context_tokens = context_tokens;
    session.restore_ui(&mut state);
    let mut history = session.snapshot.history.clone();
    let mut events = EventStream::new();
    let mut ticks = time::interval(Duration::from_millis(80));
    let mut receiver: Option<mpsc::Receiver<AgentEvent>> = None;
    let mut cancellation: Option<CancellationToken> = None;
    let mut task: Option<RunTask> = None;
    let mut pending: Option<orbit_tools::approval::ApprovalRequest> = None;

    let result: Result<()> = async {
        let mut dirty = true;
        loop {
            select! {
                event = events.next() => {
                    let Some(event) = event else { break };
                    dirty = true;
                    let event = event?;
                    if state.selection.handle(&event)? { continue; }
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
                        Action::Submit(prompt) => {
                            let compacting = prompt.trim() == "/compact";
                            if prompt.starts_with('/') && !compacting {
                                match run_command(&prompt, &mut state, &mut model, &mut history, &mut session, &tools).await {
                                    Ok(true) => break,
                                    Ok(false) => {},
                                    Err(error) => state.transcript.push(TranscriptItem::Error(error.to_string())),
                                }
                                continue;
                            }
                            session.begin(prompt.clone(), state.model.clone()).await?;
                            state.submit(prompt.clone());
                            let mut messages = history.clone();
                            if !compacting { messages.push(ChatMessage::user(prompt)); }
                            let request = ChatRequest::new(messages);
                            let (sender, rx) = mpsc::channel(256);
                            receiver = Some(rx);
                            let token = CancellationToken::new();
                            cancellation = Some(token.clone());
                            let tools = tools.clone();
                            let backend = ModelClientBackend::new(model.clone());
                            let config = AgentConfig { context_tokens, ..AgentConfig::default() };
                            let agent = Agent::new(backend.clone(), tools.clone(), config.clone());
                            task = Some(tokio::spawn(async move {
                                if compacting {
                                    let compacted = if demo {
                                        orbit_agent::context::compact(&self::demo::DemoModel, request.messages, &tools.definitions(), context_tokens, token).await?
                                    } else {
                                        orbit_agent::context::compact(&backend, request.messages, &tools.definitions(), context_tokens, token).await?
                                    };
                                    let report = format!("上下文已压缩：估算 {} → {} tokens。保留最近两轮完整对话，原始记录仍在。\n\n{}", compacted.before_tokens, compacted.after_tokens, compacted.summary);
                                    sender.send(AgentEvent::Model(orbit_model::types::StreamEvent::TextDelta(report.clone()))).await.map_err(|_| AgentError::EventChannelClosed)?;
                                    return Ok(AgentResult { final_message: ChatMessage::assistant(report), history: compacted.history, turns: 1, tool_calls: 0 });
                                }
                                if demo {
                                    Agent::new(self::demo::DemoModel, tools, config)
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
                    state.selection = Default::default();
                    state.approval = Some(request.preview.clone());
                    state.approval_scroll = 0;
                    state.approval_shown = false;
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
                            ("failed", AgentEvent::Failed { error: "API 返回了空回答，请重试或使用 /model 切换模型。".into() }, None)
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
                    if let Some(saved) = completed_history { history = saved; }
                    state.apply(final_event);
                    task = None;
                    receiver = None;
                    cancellation = None;
                }
                _ = ticks.tick() => {
                    session.flush_due().await?;
                    if dirty || state.running() {
                        terminal.draw(|frame| {
                            render::draw(frame, &mut state);
                            state.selection.paint(frame);
                        })?;
                        dirty = false;
                    }
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
    tools: &ToolRegistry,
) -> Result<bool> {
    let (command, argument) = prompt
        .split_once(char::is_whitespace)
        .unwrap_or((prompt, ""));
    let argument = argument.trim();
    match command {
        "/exit" | "/quit" if argument.is_empty() => return Ok(true),
        "/help" if argument.is_empty() => state.show_help = true,
        "/clear" if argument.is_empty() => {
            session.clear(state.model.clone(), state.demo).await?;
            history.clear();
            let input = std::mem::take(&mut state.input);
            let demo = state.demo;
            let context_tokens = state.context_tokens;
            *state = UiState::new(state.model.clone());
            state.demo = demo;
            state.context_tokens = context_tokens;
            state.input = input;
            session.restore_ui(state);
        }
        "/context" if argument.is_empty() => {
            let estimated = orbit_agent::context::estimate_tokens(history, &tools.definitions());
            state.transcript.push(TranscriptItem::Notice(format!("上下文估算：{estimated} tokens（含工具定义）\n输出预留：4096 tokens · 总预算：{}\n这是通用估算，不等于服务端实际用量。\n/compact 总结旧对话并保留最近两轮；真实模式会调用模型，演示模式生成机械提要。",state.context_tokens)));
        }
        "/sessions" if argument.is_empty() => state.transcript.push(TranscriptItem::Notice(crate::session::list_text(&session.store).await?)),
        "/status" if argument.is_empty() => state.transcript.push(TranscriptItem::Notice(format!(
            "会话  {}\n数据库  {}\n模型  {}\n模式  {}\n上下文  {} 条消息 · 已持久化\n超时  {}s（流空闲）\n工具  read_file / write_file / run_command · 写入和沙箱命令需逐次确认",
            session.id(), session.store.path().display(), state.model, if state.demo { "离线演示" } else { "真实 API" }, history.len(), model.config().timeout.as_secs()
        ))),
        "/model" if state.demo => state.transcript.push(TranscriptItem::Notice("当前为演示模式，不连接真实模型。退出后移除 --demo 即可使用配置中的 API。".into())),
        "/model" => {
            if argument.is_empty() {
                state.transcript.push(TranscriptItem::Notice(format!("当前模型：{}\n输入 /model <名称> 切换；沿用当前 API 配置。", state.model)));
            } else {
                let mut config = model.config().clone();
                config.model = argument.to_owned();
                match ModelClient::new(config) {
                    Ok(client) => {
                        *model = client;
                        state.model = argument.to_owned();
                        state.transcript.push(TranscriptItem::Notice(format!("已选择 {argument} · 下一条消息验证可用性")));
                    }
                    Err(error) => state.transcript.push(TranscriptItem::Error(error.to_string())),
                }
            }
        }
        _ => state.transcript.push(TranscriptItem::Notice(format!("无法识别命令：{prompt}\n输入 /help 查看可用命令。"))),
    }
    state.follow_bottom = true;
    state.menu_dismissed = false;
    Ok(false)
}

#[derive(Debug, PartialEq)]
enum Action {
    None,
    Quit,
    Cancel,
    Redraw,
    Submit(String),
}

fn keyboard_action(state: &mut UiState, event: Event, busy: bool) -> Action {
    if let Event::Mouse(mouse) = event {
        match mouse.kind {
            MouseEventKind::ScrollUp => state.scroll_up(3),
            MouseEventKind::ScrollDown => state.scroll_down(3),
            _ => {}
        }
        return Action::None;
    }
    if let Event::Paste(text) = event {
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
            KeyCode::Char('c') if !state.input.text.is_empty() => state.input.clear(),
            KeyCode::Char('c') => return Action::Quit,
            KeyCode::Char('d') if state.input.text.is_empty() => return Action::Quit,
            KeyCode::Char('d') => state.input.delete(),
            KeyCode::Char('j') => state.input.insert("\n"),
            KeyCode::Char('a') => state.input.home(),
            KeyCode::Char('e') => state.input.end(),
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
            KeyCode::Char('o') => state.expanded = !state.expanded,
            _ => {}
        }
        return Action::None;
    }
    if key.code == KeyCode::Esc {
        if busy {
            return Action::Cancel;
        }
        state.show_help = false;
        state.menu_dismissed = true;
        return Action::None;
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
            return state
                .input
                .take()
                .map(Action::Submit)
                .unwrap_or(Action::None);
        }
        KeyCode::Char('?') if state.input.text.is_empty() => state.show_help = !state.show_help,
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
            state.input.insert(&c.to_string());
            state.menu_index = 0;
            state.menu_dismissed = false;
        }
        KeyCode::Backspace => {
            state.input.backspace();
            state.menu_index = 0;
            state.menu_dismissed = false;
        }
        KeyCode::Delete => state.input.delete(),
        KeyCode::Left => state.input.move_left(),
        KeyCode::Right => state.input.move_right(),
        KeyCode::Home => state.input.home(),
        KeyCode::End => state.input.end(),
        KeyCode::Up => state.input.history_up(),
        KeyCode::Down => state.input.history_down(),
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
            Action::Submit("/clear".into())
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
                assert_eq!(state.scroll, 3);
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
}
