pub mod app;
pub mod commands;
mod demo;
pub mod input;
pub mod render;
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
) -> Result<()> {
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    let mut state = UiState::new(model_name);
    state.demo = demo;
    let mut history = Vec::new();
    let mut events = EventStream::new();
    let mut ticks = time::interval(Duration::from_millis(80));
    let mut receiver: Option<mpsc::Receiver<AgentEvent>> = None;
    let mut cancellation: Option<CancellationToken> = None;
    let mut task: Option<RunTask> = None;

    let result: Result<()> = async {
        let mut dirty = true;
        loop {
            select! {
                event = events.next() => {
                    let Some(event) = event else { break };
                    dirty = true;
                    match keyboard_action(&mut state, event?, task.is_some()) {
                        Action::Quit => break,
                        Action::Redraw => terminal.clear()?,
                        Action::Cancel => {
                            state.cancel();
                            if let Some(token) = &cancellation { token.cancel(); }
                        }
                        Action::Submit(prompt) => {
                            if prompt.starts_with('/') {
                                if run_command(&prompt, &mut state, &mut model, &mut history) { break; }
                                continue;
                            }
                            state.submit(prompt.clone());
                            let mut messages = history.clone();
                            messages.push(ChatMessage::user(prompt));
                            let request = ChatRequest::new(messages);
                            let (sender, rx) = mpsc::channel(256);
                            receiver = Some(rx);
                            let token = CancellationToken::new();
                            cancellation = Some(token.clone());
                            let tools = tools.clone();
                            let agent = Agent::new(ModelClientBackend::new(model.clone()), tools.clone(), AgentConfig::default());
                            task = Some(tokio::spawn(async move {
                                select! {
                                    biased;
                                    _ = token.cancelled() => Err(AgentError::Cancelled),
                                    result = async {
                                        if demo {
                                            Agent::new(self::demo::DemoModel, tools, AgentConfig::default())
                                                .run_with_agent_events(request, token.clone(), sender).await
                                        } else {
                                            agent.run_with_agent_events(request, token.clone(), sender).await
                                        }
                                    } => result,
                                }
                            }));
                        }
                        Action::None => {}
                    }
                }
                event = async { receiver.as_mut().unwrap().recv().await }, if receiver.is_some() => {
                    dirty = true;
                    match event {
                        Some(event) => state.apply(event),
                        None => receiver = None,
                    }
                }
                result = async { task.as_mut().unwrap().await }, if task.is_some() => {
                    dirty = true;
                    // Drain the completed run before accepting another prompt or clearing history.
                    if let Some(rx) = receiver.as_mut() {
                        while let Ok(event) = rx.try_recv() { state.apply(event); }
                    }
                    match result {
                        Ok(Ok(result)) if matches!(&result.final_message, ChatMessage::Assistant { content, tool_calls } if content.as_deref().unwrap_or_default().trim().is_empty() && tool_calls.is_empty()) => {
                            state.fail("API 返回了空回答，请重试或使用 /model 切换模型。");
                        }
                        Ok(Ok(result)) => {
                            history = result.history;
                            state.apply(AgentEvent::Completed { turns: result.turns, tool_calls: result.tool_calls });
                        }
                        Ok(Err(AgentError::Cancelled)) => state.apply(AgentEvent::Cancelled),
                        Ok(Err(error)) if state.running() => state.fail(error.to_string()),
                        Ok(Err(_)) => {},
                        Err(error) => state.fail(format!("后台任务中断：{error}")),
                    }
                    task = None;
                    receiver = None;
                    cancellation = None;
                }
                _ = ticks.tick() => {
                    if dirty || state.running() {
                        terminal.draw(|frame| render::draw(frame, &mut state))?;
                        dirty = false;
                    }
                }
            }
        }
        Ok(())
    }.await;

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
    result
}

fn run_command(
    prompt: &str,
    state: &mut UiState,
    model: &mut ModelClient,
    history: &mut Vec<ChatMessage>,
) -> bool {
    let (command, argument) = prompt
        .split_once(char::is_whitespace)
        .unwrap_or((prompt, ""));
    let argument = argument.trim();
    match command {
        "/exit" | "/quit" if argument.is_empty() => return true,
        "/help" if argument.is_empty() => state.show_help = true,
        "/clear" if argument.is_empty() => {
            history.clear();
            let input = std::mem::take(&mut state.input);
            let demo = state.demo;
            *state = UiState::new(state.model.clone());
            state.demo = demo;
            state.input = input;
        }
        "/status" if argument.is_empty() && !state.demo => state.transcript.push(TranscriptItem::Notice(format!(
            "模型  {}\n协议  {:?}\n上下文  {} 条消息 · 仅保存在本次进程内\n超时  {}s\n工具  read_file · 只读访问当前工作区\n连接  发送消息后验证", state.model, model.config().api_format, history.len(), model.config().timeout.as_secs()
        ))),
        "/model" if state.demo => state.transcript.push(TranscriptItem::Notice("当前为演示模式，不连接真实模型。退出后移除 --demo 即可使用配置中的 API。".into())),
        "/status" if state.demo => state.transcript.push(TranscriptItem::Notice(format!("演示模式 · 无网络请求\n工具 read_file · 真实读取工作区文件\n上下文 {} 条消息", history.len()))),
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
    false
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
