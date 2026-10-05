use std::{sync::Arc, time::Duration};

use anyhow::{Result, bail};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use tokio::sync::mpsc;

use super::{
    app::{RunStatus, TranscriptItem, UiState},
    approval,
    frames::Frames,
    input_events, media, render, selection, terminal,
};
use crate::workspace::{
    self, Call, Files,
    protocol::{Mode, Request, Response, SessionInfo, Update},
    transport::{Client, Connection},
};

const HELP: &str = "工作区连接：/sessions 列出会话，/resume <ID> 接回，/new 新建，/model <供应商::模型> 切换模型，/mode auto|ask|yolo，/quit 断开。Ctrl+C 取消任务，Ctrl+D 断开。任务在目标环境运行，断开后仍会继续。";

pub async fn run(client: Client, connection: Arc<Connection>, info: SessionInfo) -> Result<()> {
    let (_guard, mut terminal) = terminal::TerminalGuard::enter()?;
    let mut state = UiState::new(&info.model);
    state.media = media::Context::detect(Arc::default());
    let (commands, requests) = mpsc::channel(16);
    let commands = Arc::new(commands);
    install(&mut state, &info, connection.clone(), commands.clone());
    state.report_local(TranscriptItem::Notice(HELP.into()));
    let (responses, mut incoming) = mpsc::channel(16);
    let _worker = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(pump(
        client,
        info.id.clone(),
        requests,
        responses,
    )));
    let mut id = info.id;
    let mut busy = false;
    let mut approval_id = None;
    let mut selected_model = None;
    let mut mode = Mode::Auto;
    let mut events = EventStream::new();
    let mut pending_event = None;
    let mut frames = Frames::default();
    frames.request(tokio::time::Instant::now());
    let mut ticks = tokio::time::interval(Duration::from_millis(33));
    loop {
        tokio::select! {
            response = incoming.recv() => {
                let Some(response) = response else {
                    bail!("workspace connection ended");
                };
                match response? {
                    Response::Page(page) => {
                        busy = page.busy;
                        state.model = page.model;
                        for update in page.updates {
                            match update {
                                Update::Profile(info) => {
                                    state.model = info.model;
                                    state.context_tokens = info.context_tokens;
                                    state.input_context_tokens = info.input_context_tokens;
                                    state.max_output_tokens = info.max_output_tokens;
                                }
                                Update::Started { prompt } => state.submit(prompt),
                                Update::Agent(event) => state.apply(*event),
                                Update::Finished { status } => {
                                    state.pause_clock();
                                    state.status = match status.as_str() {
                                        "completed" => RunStatus::Completed,
                                        "cancelled" => RunStatus::Cancelled,
                                        _ => RunStatus::Failed,
                                    };
                                }
                            }
                        }
                        let pending = page.approvals.into_iter().next();
                        if pending.as_ref().map(|approval| approval.id) != approval_id {
                            approval_id = pending.as_ref().map(|approval| approval.id);
                            state.approval = pending.map(|approval| approval.preview);
                            state.approval_scroll = 0;
                            state.approval_expanded = false;
                            state.approval_shown = false;
                            state.approval_choice = 0;
                            state.approval_diff = None;
                            if state.approval.is_some() {
                                state.pause_clock();
                            } else if busy {
                                state.resume_clock();
                            }
                        }
                    }
                    Response::Opened(info) => {
                        id = info.id.clone();
                        let media = state.media.clone();
                        state = UiState::new(&info.model);
                        state.media = media;
                        install(&mut state, &info, connection.clone(), commands.clone());
                        busy = false;
                        approval_id = None;
                        selected_model = None;
                        mode = Mode::Auto;
                    }
                    Response::Sessions(sessions) => state.report_local(TranscriptItem::CommandResult(
                        sessions
                            .iter()
                            .map(|session| format!("{}  {}  {}", session.id, session.model, session.title))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    )),
                    Response::Error(error) => state.report_local(TranscriptItem::Error(error)),
                    _ => {}
                }
                frames.request(tokio::time::Instant::now());
            }
            event = input_events::next(&mut events, &mut pending_event) => {
                let Some(event) = event else { break; };
                let event = event?;
                if matches!(&event, Event::Key(key) if key.kind != KeyEventKind::Release && key.code == KeyCode::Char('d') && key.modifiers.contains(KeyModifiers::CONTROL) && state.input.text.is_empty())
                {
                    break;
                }
                if let Some(preview) = &mut state.media_preview {
                    let (_, close) = preview.handle(&event);
                    if close {
                        state.media_preview = None;
                    }
                    frames.request(tokio::time::Instant::now());
                    continue;
                }
                if let Some(approval) = approval_id {
                    let action = approval::handle(&mut state, event.clone());
                    let request = match action {
                        approval::Action::Approve | approval::Action::AlwaysAllow | approval::Action::Deny => {
                            Some(Request::Approve {
                                session: id.clone(),
                                approval,
                                allow: action != approval::Action::Deny,
                                remember: action == approval::Action::AlwaysAllow,
                            })
                        }
                        approval::Action::Cancel => Some(Request::Cancel {
                            session: id.clone(),
                        }),
                        approval::Action::None => None,
                    };
                    if let Some(request) = request {
                        send(&commands, request, &mut state);
                    }
                    frames.request(tokio::time::Instant::now());
                    continue;
                }
                let mut changed = false;
                if !state.selection.dragging() {
                    changed |= render::handle_link_hover(&mut state, &event) == Some(true);
                    changed |= render::handle_transcript_hover(&mut state, &event) == Some(true);
                    changed |= super::navigation::handle(&mut state, &event) == Some(true);
                }
                if matches!(&event, Event::Mouse(mouse) if mouse.kind == MouseEventKind::Moved) {
                    if changed {
                        frames.request(tokio::time::Instant::now());
                    }
                    continue;
                }
                frames.request(tokio::time::Instant::now());
                if super::handle_input_mouse(&mut state, &event) {
                    continue;
                }
                if let Event::Mouse(mouse) = &event
                    && mouse.kind == MouseEventKind::Down(MouseButton::Left)
                    && mouse.modifiers.contains(KeyModifiers::CONTROL)
                {
                    let offset = state.max_scroll.saturating_sub(state.scroll);
                    if let Some(link) = state
                        .transcript_layout
                        .link_at(state.selection.viewport, offset, (mouse.column, mouse.row))
                        .map(str::to_owned)
                    {
                        if let Err(error) = super::open_link_in_browser(&link) {
                            state.report_local(TranscriptItem::Error(error.to_string()));
                        }
                        continue;
                    }
                }
                if let Some(point) = state.selection.click(&event)
                    && (super::jump_footnote_at(&mut state, point)
                        || super::toggle_transcript_at(&mut state, point))
                {
                    continue;
                }
                if state.selection.handle(&event)? {
                    continue;
                }
                match &event {
                    Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollUp => {
                        state.scroll_up(super::wheel::LINES)
                    }
                    Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollDown => {
                        state.scroll_down(super::wheel::LINES)
                    }
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        let control = key.modifiers.contains(KeyModifiers::CONTROL);
                        match key.code {
                            KeyCode::Char('d') if control && state.input.text.is_empty() => break,
                            KeyCode::Char('c') if control => {
                                if let Some(text) = state.input.selected_text() {
                                    selection::copy_text(text)?;
                                } else if busy {
                                    send(
                                        &commands,
                                        Request::Cancel {
                                            session: id.clone(),
                                        },
                                        &mut state,
                                    );
                                } else if state.input.text.is_empty() {
                                    break;
                                } else {
                                    state.input.clear();
                                }
                            }
                            KeyCode::PageUp => state.scroll_up(10),
                            KeyCode::PageDown => state.scroll_down(10),
                            KeyCode::Enter if key.modifiers.is_empty() => {
                                if let Some(prompt) = state.input.take() {
                                    let request = match prompt.trim() {
                                        "/quit" | "/exit" => break,
                                        "/help" => {
                                            state.report_local(TranscriptItem::Notice(HELP.into()));
                                            None
                                        }
                                        "/sessions" => Some(Request::Sessions),
                                        "/new" => Some(Request::Open { resume: None }),
                                        "/model" => {
                                            state.report_local(TranscriptItem::CommandResult(
                                                state.model_completions.join("\n"),
                                            ));
                                            None
                                        }
                                        text if text.starts_with("/resume ") => Some(Request::Open {
                                            resume: Some(text[8..].trim().into()),
                                        }),
                                        text if text.starts_with("/model ") => {
                                            if busy {
                                                state.report_local(TranscriptItem::Notice(
                                                    "任务执行中，结束后再切换模型。".into(),
                                                ));
                                                continue;
                                            }
                                            let model = text[7..].trim();
                                            if state.model_completions.iter().any(|choice| choice == model) {
                                                selected_model = Some(model.to_owned());
                                                state.model = model.into();
                                            } else {
                                                state.report_local(TranscriptItem::Error(
                                                    "模型未配置；用 /model 查看目标环境的模型。".into(),
                                                ));
                                            }
                                            None
                                        }
                                        text if text.starts_with("/mode ") => {
                                            if busy {
                                                state.report_local(TranscriptItem::Notice(
                                                    "任务执行中，结束后再切换权限模式。".into(),
                                                ));
                                                continue;
                                            }
                                            match text[6..].trim() {
                                                "auto" => {
                                                    mode = Mode::Auto;
                                                    state.permission_mode = super::permissions::Mode::Auto;
                                                }
                                                "ask" => {
                                                    mode = Mode::Ask;
                                                    state.permission_mode = super::permissions::Mode::Ask;
                                                }
                                                "yolo" => {
                                                    mode = Mode::Yolo;
                                                    state.permission_mode = super::permissions::Mode::Yolo;
                                                }
                                                _ => state.report_local(TranscriptItem::Error(
                                                    "模式：auto、ask、yolo".into(),
                                                )),
                                            }
                                            None
                                        }
                                        text if text.starts_with('/') => {
                                            state.report_local(TranscriptItem::Error(
                                                "此工作区入口尚未接入该命令；用 /help 查看支持的命令。".into(),
                                            ));
                                            None
                                        }
                                        _ if busy => {
                                            state.input.set(prompt);
                                            state.report_local(TranscriptItem::Notice(
                                                "任务仍在执行；可先取消，输入已保留。".into(),
                                            ));
                                            None
                                        }
                                        _ => Some(Request::Start {
                                            session: id.clone(),
                                            request_id: workspace::request_id(),
                                            prompt,
                                            mode,
                                            model: selected_model.clone(),
                                        }),
                                    };
                                    if let Some(request) = request {
                                        send(&commands, request, &mut state);
                                    }
                                }
                            }
                            _ => {
                                state.input.edit(&event, state.input_width);
                            }
                        }
                    }
                    Event::Paste(_) => {
                        state.input.edit(&event, state.input_width);
                    }
                    _ => {}
                }
            }
            _ = ticks.tick() => {
                if state.media.changed()
                    || state
                        .media_preview
                        .as_ref()
                        .is_some_and(|preview| preview.tick())
                    || busy
                {
                    frames.request(tokio::time::Instant::now());
                }
            }
            _ = tokio::time::sleep_until(frames.deadline()), if frames.pending() => {
                state.menu_dismissed = true;
                terminal::draw_synchronized(&mut terminal, |frame| {
                    render::draw(frame, &mut state);
                })?;
                frames.drawn(tokio::time::Instant::now());
            }
        }
    }
    Ok(())
}

fn install(
    state: &mut UiState,
    info: &SessionInfo,
    connection: Arc<Connection>,
    requests: Arc<mpsc::Sender<Call>>,
) {
    state.connection_label = Some(connection.target.label());
    state.workspace = info.workspace.clone();
    state.context_tokens = info.context_tokens;
    state.input_context_tokens = info.input_context_tokens;
    state.max_output_tokens = info.max_output_tokens;
    state.model_completions = info.models.clone();
    state.media.files = Some(Files {
        requests,
        session: info.id.clone(),
    });
    state.report_local(TranscriptItem::Notice(format!("工作区会话 {}", info.id)));
}

fn send(commands: &mpsc::Sender<Call>, request: Request, state: &mut UiState) {
    if let Err(error) = commands.try_send(Call {
        request,
        reply: None,
    }) {
        let message = format!("连接请求未发送：{error}");
        if let Request::Start { prompt, .. } = error.into_inner().request {
            state.input.set(prompt);
        }
        state.report_local(TranscriptItem::Error(message));
    }
}

async fn pump(
    mut client: Client,
    mut session: String,
    mut commands: mpsc::Receiver<Call>,
    responses: mpsc::Sender<Result<Response>>,
) {
    let mut cursor = 0;
    let mut last_status = None;
    let mut interval = tokio::time::interval(Duration::from_millis(75));
    loop {
        let call = tokio::select! {
            command = commands.recv() => match command { Some(command) => command, None => break },
            _ = interval.tick() => Call { request: Request::Poll { session: session.clone(), after: cursor }, reply: None },
        };
        let response = client.request(call.request).await;
        let response = match response {
            Ok(response) => response,
            Err(error) if client.failed() => {
                if let Some(reply) = call.reply {
                    let _ = reply.send(Err(anyhow::anyhow!(error.to_string())));
                }
                let _ = responses.send(Err(error)).await;
                break;
            }
            Err(error) => Response::Error(format!("{error:#}")),
        };
        if let Some(reply) = call.reply {
            let _ = reply.send(Ok(response));
            continue;
        }
        match &response {
            Response::Page(page) => {
                cursor = page.next;
                if page.more {
                    interval.reset_immediately();
                }
                let status = (
                    page.busy,
                    page.model.clone(),
                    page.approvals
                        .iter()
                        .map(|approval| approval.id)
                        .collect::<Vec<_>>(),
                );
                if page.updates.is_empty() && last_status.as_ref() == Some(&status) {
                    continue;
                }
                last_status = Some(status);
            }
            Response::Opened(info) => {
                session = info.id.clone();
                cursor = 0;
                last_status = None;
                interval.reset_immediately();
            }
            _ => {}
        }
        if responses.send(Ok(response)).await.is_err() {
            break;
        }
    }
}
