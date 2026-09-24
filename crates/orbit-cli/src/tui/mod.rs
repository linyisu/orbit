mod app;
mod approval_diff;
mod commands;
mod input;
mod markdown;
mod render;
mod session_runtime;
mod terminal;
mod tool_display;

use std::{
    future::pending,
    path::{Path, PathBuf},
};

use anyhow::Result;
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use futures_util::StreamExt;
use orbit_agent::events::AgentEvent;
use orbit_model::types::ChatMessage;
use orbit_store::Store;
use orbit_tools::approval::ApprovalRequest;
use tokio::{sync::mpsc, task::JoinHandle, time};

use crate::{
    TuiArgs,
    approval::{self, ApprovalMode},
};
use app::{App, ModelPicker, Picker};
use session_runtime::{ActiveRun, RunOutcome, Runtime};
use terminal::TerminalGuard;

enum RestoreEvent {
    Page(Vec<ChatMessage>),
    Done(Result<()>),
}

struct RestoreRun {
    session_id: String,
    events: mpsc::Receiver<RestoreEvent>,
    task: JoinHandle<()>,
}

enum LoopEvent {
    Input(Option<std::io::Result<Event>>),
    Approval(Option<ApprovalRequest>),
    Agent(AgentEvent),
    Restore(Option<RestoreEvent>),
    Done(Result<RunOutcome, tokio::task::JoinError>),
    Tick,
}

pub async fn run(config_path: PathBuf, database: PathBuf, args: TuiArgs) -> Result<()> {
    let (approval_sender, mut approvals) = orbit_tools::approval::channel();
    let mut runtime = Runtime::open(config_path, database, &args, approval_sender).await?;
    let mut app = App::new(
        runtime.session_id().to_owned(),
        runtime.workspace.display().to_string(),
        runtime.selected_model().name.clone(),
        runtime.selected_model().context_tokens,
    );
    app.approval_mode = args.approval;
    app.reasoning = runtime.reasoning();
    app.context_used_tokens = runtime.context_usage().await?.0;
    let (_guard, mut terminal) = TerminalGuard::enter()?;
    app.begin_restore();
    let mut restore = Some(start_restore(runtime.store.clone(), app.session_id.clone()));
    let mut input_events = EventStream::new();
    let mut tick = time::interval(std::time::Duration::from_millis(50));
    tick.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
    let mut active: Option<ActiveRun> = None;
    let mut dirty = true;

    loop {
        if dirty {
            terminal.draw(|frame| render::draw(frame, &mut app))?;
            dirty = false;
        }
        let next = tokio::select! {
            event = input_events.next() => LoopEvent::Input(event),
            request = approvals.recv() => LoopEvent::Approval(request),
            page = async {
                match restore.as_mut() {
                    Some(loading) => loading.events.recv().await,
                    None => pending().await,
                }
            } => LoopEvent::Restore(page),
            activity = async {
                match active.as_mut() {
                    Some(run) => tokio::select! {
                        event = run.events.recv() => match event {
                            Some(event) => LoopEvent::Agent(event),
                            None => LoopEvent::Done((&mut run.task).await),
                        },
                        done = &mut run.task => LoopEvent::Done(done),
                    },
                    None => pending().await,
                }
            } => activity,
            _ = tick.tick(), if app.running => LoopEvent::Tick,
        };
        match next {
            LoopEvent::Input(Some(event)) => {
                let event = event?;
                if handle_event(event, &mut runtime, &mut app, &mut active).await? {
                    break;
                }
                let previous = if app.restoring
                    && restore
                        .as_ref()
                        .is_none_or(|loading| loading.session_id != app.session_id)
                {
                    restore.replace(start_restore(runtime.store.clone(), app.session_id.clone()))
                } else {
                    None
                };
                if let Some(previous) = previous {
                    previous.task.abort();
                }
                dirty = true;
            }
            LoopEvent::Input(None) => break,
            LoopEvent::Approval(Some(request)) => {
                resolve_approval(request, app.approval_mode, &runtime.workspace, &mut app);
                dirty = true;
            }
            LoopEvent::Approval(None) => {}
            LoopEvent::Agent(event) => {
                app.apply(event);
                dirty = true;
            }
            LoopEvent::Restore(Some(RestoreEvent::Page(page))) => {
                app.restore_page(&page);
                dirty = true;
            }
            LoopEvent::Restore(Some(RestoreEvent::Done(result))) => {
                app.finish_restore();
                restore = None;
                if let Err(error) = result {
                    app.error(format!("Session restore failed: {error}"));
                }
                dirty = true;
            }
            LoopEvent::Restore(None) => {
                app.finish_restore();
                restore = None;
                dirty = true;
            }
            LoopEvent::Done(done) => {
                let mut run = active.take().expect("active run exists");
                while let Ok(event) = run.events.try_recv() {
                    app.apply(event);
                }
                match done {
                    Ok(RunOutcome::Agent {
                        writer,
                        result,
                        finish,
                    }) => {
                        runtime.writer = Some(writer);
                        if let Err(error) = finish {
                            app.error(format!("Session persistence failed: {error}"));
                        }
                        if let Err(error) = result {
                            let message = error.to_string();
                            if !app.entries.last().is_some_and(|entry| {
                                entry.kind == app::EntryKind::Error && entry.text == message
                            }) {
                                app.error(message);
                            }
                        }
                    }
                    Ok(RunOutcome::Compaction(result)) => match result {
                        Ok(progress) if progress.batches == 0 => {
                            app.notice("Context is already within the compact target")
                        }
                        Ok(progress) => app.notice(format!(
                            "Context compaction saved {} checkpoint batch(es){}{}",
                            progress.batches,
                            if progress.degraded {
                                " · local fallback used"
                            } else {
                                ""
                            },
                            if progress.complete {
                                ""
                            } else {
                                " · more history remains; run /compact again"
                            }
                        )),
                        Err(error) => app.error(format!("Context compaction failed: {error}")),
                    },
                    Err(error) => {
                        app.error(format!("Agent task stopped: {error}"));
                        if runtime.writer.is_none()
                            && let Ok(writer) = runtime.store.acquire(&app.session_id).await
                        {
                            runtime.writer = Some(writer);
                        }
                    }
                }
                app.running = false;
                app.status = "Ready".to_owned();
                if let Ok((used, _, _)) = runtime.context_usage().await {
                    app.context_used_tokens = used;
                }
                dirty = true;
            }
            LoopEvent::Tick => {
                dirty = true;
            }
        }
    }
    if let Some(run) = active {
        run.cancellation.cancel();
        let _ = run.task.await;
    }
    if let Some(loading) = restore {
        loading.task.abort();
    }
    Ok(())
}

fn start_restore(store: Store, session_id: String) -> RestoreRun {
    let (sender, events) = mpsc::channel(2);
    let loading_id = session_id.clone();
    let task = tokio::spawn(async move {
        let mut after = None;
        let result = loop {
            match store.canonical_messages(&loading_id, after, 64).await {
                Ok(page) if page.is_empty() => break Ok(()),
                Ok(page) => {
                    after = page.last().map(|message| message.id);
                    let messages = page.into_iter().map(|record| record.message).collect();
                    if sender.send(RestoreEvent::Page(messages)).await.is_err() {
                        return;
                    }
                }
                Err(error) => break Err(error),
            }
        };
        let _ = sender.send(RestoreEvent::Done(result)).await;
    });
    RestoreRun {
        session_id,
        events,
        task,
    }
}

fn resolve_approval(
    request: ApprovalRequest,
    mode: ApprovalMode,
    workspace: &std::path::Path,
    app: &mut App,
) {
    match mode {
        ApprovalMode::Auto => {
            let approved = approval::auto_allows(&request.preview, workspace);
            let _ = request.decision.send(approved);
        }
        ApprovalMode::Deny => {
            let _ = request.decision.send(false);
        }
        ApprovalMode::Ask => {
            app.approval = Some(request);
            app.approval_scroll = 0;
            app.approval_diff = None;
            app.approval_full = false;
        }
    }
}

async fn handle_event(
    event: Event,
    runtime: &mut Runtime,
    app: &mut App,
    active: &mut Option<ActiveRun>,
) -> Result<bool> {
    match event {
        Event::Resize(_, _) => return Ok(false),
        Event::Paste(text) if let Some(picker) = &mut app.picker => {
            picker.insert_query(&text);
            return Ok(false);
        }
        Event::Paste(text)
            if app.approval.is_none()
                && app.picker.is_none()
                && app.model_picker.is_none()
                && active.is_none() =>
        {
            app.input.insert(&text);
            app.command_selection = 0;
            return Ok(false);
        }
        Event::Mouse(mouse) => {
            match mouse.kind {
                MouseEventKind::ScrollUp => scroll_up(app, 3),
                MouseEventKind::ScrollDown => scroll_down(app, 3),
                _ => {}
            }
            return Ok(false);
        }
        Event::Key(key) if key.kind != KeyEventKind::Release => {
            return handle_key(key, runtime, app, active).await;
        }
        _ => {}
    }
    Ok(false)
}

async fn handle_key(
    key: KeyEvent,
    runtime: &mut Runtime,
    app: &mut App,
    active: &mut Option<ActiveRun>,
) -> Result<bool> {
    if let Some(request) = app.approval.take() {
        match key.code {
            KeyCode::Char('y' | 'Y') => {
                let _ = request.decision.send(true);
                app.approval_diff = None;
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                let _ = request.decision.send(false);
                app.approval_diff = None;
            }
            KeyCode::Up => {
                app.approval_scroll = app.approval_scroll.saturating_sub(1);
                app.approval = Some(request);
            }
            KeyCode::Down => {
                app.approval_scroll = app.approval_scroll.saturating_add(1);
                app.approval = Some(request);
            }
            KeyCode::Tab
                if matches!(
                    &request.preview,
                    orbit_tools::approval::ApprovalPreview::Write(_)
                ) =>
            {
                app.approval_full = !app.approval_full;
                app.approval_scroll = 0;
                app.approval_diff = None;
                app.approval = Some(request);
            }
            _ => app.approval = Some(request),
        }
        return Ok(false);
    }
    if let Some(picker) = &mut app.picker {
        match key.code {
            KeyCode::Esc => app.picker = None,
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => {
                picker.selected =
                    (picker.selected + 1).min(picker.matching().len().saturating_sub(1))
            }
            KeyCode::Backspace => picker.backspace_query(),
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.query.clear();
                picker.selected = 0;
            }
            KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                picker.insert_query(&character.to_string());
            }
            KeyCode::Enter => {
                let id = picker
                    .matching()
                    .get(picker.selected)
                    .map(|session| session.id.clone());
                if id.is_none() {
                    return Ok(false);
                }
                app.picker = None;
                if let Some(id) = id
                    && let Err(error) = runtime.switch_session(Some(&id), app).await
                {
                    app.error(error.to_string());
                }
            }
            _ => {}
        }
        return Ok(false);
    }
    if let Some(picker) = &mut app.model_picker {
        match key.code {
            KeyCode::Esc => app.model_picker = None,
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => {
                picker.selected = (picker.selected + 1).min(picker.models.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                let name = picker.models.get(picker.selected).cloned();
                app.model_picker = None;
                if let Some(name) = name
                    && let Err(error) = runtime.select_model(&name, app).await
                {
                    app.error(error.to_string());
                }
            }
            _ => {}
        }
        return Ok(false);
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        if let Some(run) = active {
            run.cancellation.cancel();
            app.status = "Cancelling".to_owned();
            return Ok(false);
        }
        return Ok(true);
    }
    match key.code {
        KeyCode::PageUp => scroll_up(app, 10),
        KeyCode::PageDown => scroll_down(app, 10),
        KeyCode::BackTab if active.is_none() => {
            app.approval_mode = app.approval_mode.next();
            app.notice(format!("Approval mode: {}", app.approval_mode.label()));
        }
        KeyCode::Tab
            if active.is_none()
                && !app.restoring
                && !commands::matching(app.input.text()).is_empty() =>
        {
            let matches = commands::matching(app.input.text());
            let selected = matches[app.command_selection.min(matches.len() - 1)];
            app.input.replace(if selected.needs_argument {
                format!("{} ", selected.name)
            } else {
                selected.name.to_owned()
            });
            app.command_selection = 0;
        }
        KeyCode::Tab => app.select_next_tool(),
        KeyCode::Enter if app.selected_tool.is_some() && app.input.text().is_empty() => {
            app.toggle_tool()
        }
        _ if app.restoring => {}
        _ if active.is_some() => {}
        KeyCode::Esc => app.clear_tool_selection(),
        KeyCode::Left => app.input.move_left(),
        KeyCode::Right => app.input.move_right(),
        KeyCode::Home => app.input.move_home(),
        KeyCode::End => app.input.move_end(),
        KeyCode::Backspace => {
            app.input.backspace();
            app.command_selection = 0;
        }
        KeyCode::Delete => {
            app.input.delete();
            app.command_selection = 0;
        }
        KeyCode::Up if !commands::matching(app.input.text()).is_empty() => {
            app.command_selection = app.command_selection.saturating_sub(1);
        }
        KeyCode::Down if !commands::matching(app.input.text()).is_empty() => {
            let count = commands::matching(app.input.text()).len();
            app.command_selection = (app.command_selection + 1).min(count - 1);
        }
        KeyCode::Up => {
            app.input.history_up();
            app.command_selection = 0;
        }
        KeyCode::Down => {
            app.input.history_down();
            app.command_selection = 0;
        }
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => app.input.insert("\n"),
        KeyCode::Enter => {
            let matches = commands::matching(app.input.text());
            if let Some(selected) = matches
                .get(app.command_selection.min(matches.len().saturating_sub(1)))
                .copied()
            {
                if selected.needs_argument {
                    app.input.replace(format!("{} ", selected.name));
                    app.command_selection = 0;
                    return Ok(false);
                }

                if selected.name != app.input.text() {
                    app.input.replace(selected.name.to_owned());
                }
            }
            if let Some(text) = app.input.take() {
                app.command_selection = 0;
                if text.starts_with('/') {
                    return command(&text, runtime, app, active).await;
                }
                match runtime.start_run(text, app).await {
                    Ok(run) => *active = Some(run),
                    Err(error) => app.error(error.to_string()),
                }
            }
        }
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.input.insert(&character.to_string());
            app.command_selection = 0;
        }
        _ => {}
    }
    Ok(false)
}

async fn command(
    text: &str,
    runtime: &mut Runtime,
    app: &mut App,
    active: &mut Option<ActiveRun>,
) -> Result<bool> {
    let mut parts = text.split_whitespace();
    match parts.next().unwrap_or("") {
        "/exit" | "/quit" => Ok(true),
        "/new" => {
            if let Err(error) = runtime.switch_session(None, app).await {
                app.error(error.to_string());
            }
            Ok(false)
        }
        "/resume" => {
            if let Some(id) = parts.next() {
                if let Err(error) = runtime.switch_session(Some(id), app).await {
                    app.error(error.to_string());
                }
            } else {
                let sessions = runtime.store.list(&runtime.workspace).await?;
                app.picker = Some(Picker {
                    sessions,
                    selected: 0,
                    query: String::new(),
                });
            }
            Ok(false)
        }
        "/sessions" => {
            let sessions = runtime.store.list(&runtime.workspace).await?;
            app.picker = Some(Picker {
                sessions,
                selected: 0,
                query: String::new(),
            });
            Ok(false)
        }
        "/model" => {
            if let Some(name) = parts.next() {
                if let Err(error) = runtime.select_model(name, app).await {
                    app.error(error.to_string());
                }
            } else {
                let models = runtime
                    .model_options()?
                    .into_iter()
                    .map(|option| option.name)
                    .collect::<Vec<_>>();
                let selected = models
                    .iter()
                    .position(|name| name == &app.model)
                    .unwrap_or(0);
                app.model_picker = Some(ModelPicker { models, selected });
            }
            Ok(false)
        }
        "/reasoning" => {
            if let Some(value) = parts.next() {
                match value.parse() {
                    Ok(effort) => {
                        if let Err(error) = runtime.select_reasoning(effort, app).await {
                            app.error(error.to_string());
                        }
                    }
                    Err(error) => app.error(error),
                }
            } else {
                let available = runtime
                    .selected_model()
                    .reasoning_efforts
                    .iter()
                    .map(|effort| effort.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if available.is_empty() {
                    app.notice("This model has no reasoning controls");
                } else {
                    app.notice(format!("Reasoning choices: {available}"));
                }
            }
            Ok(false)
        }
        "/attach" => {
            let path = text.strip_prefix("/attach").unwrap_or("").trim();
            if path.is_empty() {
                app.error("Use /attach <image path>");
            } else if let Err(error) = runtime.attach_image(Path::new(path), app).await {
                app.error(error.to_string());
            }
            Ok(false)
        }
        "/detach" => {
            app.pending_images.clear();
            app.notice("Pending images cleared");
            Ok(false)
        }
        "/status" => {
            app.notice(format!(
                "Session {}  |  model {}  |  reasoning {}  |  approval {}  |  workspace {}",
                app.session_id,
                app.model,
                app.reasoning.map_or("default", |effort| effort.as_str()),
                app.approval_mode.label(),
                app.workspace
            ));
            Ok(false)
        }
        "/context" => {
            match runtime.context_usage().await {
                Ok((used, limit, checkpoint)) => {
                    app.context_used_tokens = used;
                    app.notice(format!(
                        "Estimated active request: {used} / {limit} tokens · checkpoint: {}",
                        if checkpoint { "saved" } else { "none" }
                    ));
                }
                Err(error) => app.error(error.to_string()),
            }
            Ok(false)
        }
        "/compact" => {
            match runtime.start_compaction(app).await {
                Ok(run) => *active = Some(run),
                Err(error) => app.error(error.to_string()),
            }
            Ok(false)
        }
        "/rename" => {
            let title = text.strip_prefix("/rename").unwrap_or("").trim();
            match runtime.store.rename_session(&app.session_id, title).await {
                Ok(()) => app.notice(format!("Session renamed: {title}")),
                Err(error) => app.error(error.to_string()),
            }
            Ok(false)
        }
        "/mode" => {
            match parts.next() {
                Some("auto") => app.approval_mode = ApprovalMode::Auto,
                Some("ask") => app.approval_mode = ApprovalMode::Ask,
                Some("deny") => app.approval_mode = ApprovalMode::Deny,
                Some(_) => app.error("Use /mode auto, /mode ask, or /mode deny"),
                None => {}
            }
            app.notice(format!("Approval mode: {}", app.approval_mode.label()));
            Ok(false)
        }
        "/help" => {
            app.notice(
                commands::COMMANDS
                    .iter()
                    .map(|command| format!("{:<12} {}", command.name, command.description))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            app.notice("Enter sends · Shift+Enter adds a line · Tab completes a command or selects a tool · Enter expands the selected tool · Shift+Tab changes approval mode · Ctrl+C cancels or exits");
            Ok(false)
        }
        other => {
            app.error(format!("Unknown command: {other}. Type /help."));
            Ok(false)
        }
    }
}

fn scroll_up(app: &mut App, amount: usize) {
    app.follow_bottom = false;
    app.scroll = app.scroll.saturating_sub(amount);
}

fn scroll_down(app: &mut App, amount: usize) {
    app.scroll = app.scroll.saturating_add(amount);
    app.follow_bottom = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restore_streams_large_sessions_in_order() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("sessions.db")).await.unwrap();
        let mut writer = store.create(root.path(), "model").await.unwrap();
        let session_id = writer.id().to_owned();
        writer.begin("start".into(), "model".into()).await.unwrap();
        for index in 0..130 {
            writer
                .append(ChatMessage::assistant(format!("answer {index}")))
                .await
                .unwrap();
        }
        writer.finish("completed").await.unwrap();

        let mut loading = start_restore(store, session_id.clone());
        let mut app = App::new(session_id, "workspace".into(), "model".into(), 128_000);
        app.begin_restore();
        let mut pages = 0;
        while let Some(event) = loading.events.recv().await {
            match event {
                RestoreEvent::Page(messages) => {
                    pages += 1;
                    app.restore_page(&messages);
                }
                RestoreEvent::Done(result) => {
                    result.unwrap();
                    app.finish_restore();
                    break;
                }
            }
        }

        assert_eq!(pages, 3);
        assert_eq!(app.entries.len(), 131);
        assert_eq!(app.entries.first().unwrap().text, "start");
        assert_eq!(app.entries.last().unwrap().text, "answer 129");
        assert!(!app.restoring);
    }
}
