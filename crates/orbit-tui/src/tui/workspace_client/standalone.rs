use crossterm::event::EventStream;
use tokio::time;

use super::*;
use crate::tui::{
    self, TabAction, frames::Frames, input_events, render, tabs::live::Tabs, terminal,
};

pub(super) async fn run(mut opened: Opened) -> Result<()> {
    let window =
        workspace::windows::ClientWindow::for_connection(&mut opened.client, &opened.connection)
            .await?;
    let previous = window.previous().to_vec();
    let mut remembered = None;
    let mut save_error = None;
    let mut persisted = std::time::Instant::now();
    let (_guard, mut terminal) = terminal::TerminalGuard::enter()?;
    let first = Tab::ready(opened, media::Context::detect(Arc::default()));
    let mut tabs = Tabs::default();
    let restore_active = !first.persisted;
    let initial = tabs.add_remote(first);
    if !previous.is_empty() {
        tabs.restore_tabs(&initial, &previous.iter().collect::<Vec<_>>());
        tabs.sync_remote_ui();
        if restore_active && let Some(index) = window.active() {
            tabs.active_remote = tabs.remote[&tabs.active_remote.clone().unwrap()]
                .state
                .tabs
                .get(index)
                .map(|entry| entry.id.clone())
                .or(tabs.active_remote);
        } else if !restore_active {
            tabs.active_remote = Some(initial);
        }
    }
    let mut events = EventStream::new();
    let mut pending_event = None;
    let mut frames = Frames::default();
    frames.request(time::Instant::now());
    let mut ticks = time::interval(Duration::from_millis(33));
    let mut wheel = tui::wheel::Wheel::default();
    loop {
        tokio::select! {
            event = input_events::next(&mut events, &mut pending_event) => {
                let Some(event) = event else { break };
                let event = event?;
                if !wheel.accept(&event, std::time::Instant::now()) {
                    continue;
                }
                tabs.sync_remote_ui();
                let id = tabs.active_remote.clone().unwrap();
                let tab = tabs.remote.get_mut(&id).unwrap();
                tui::update_tab_hover(&event, &mut tab.state);
                let action = match tui::tab_target(&event, &tab.state) {
                    Some(TabAction::Select(next)) => {
                        tabs.active_remote = Some(next);
                        Action::None
                    }
                    Some(TabAction::New) => Action::SameTarget(None),
                    Some(TabAction::Close(closing)) => {
                        close(&mut tabs, &closing);
                        Action::None
                    }
                    None => tab.handle(event)?,
                };
                match action {
                    Action::Quit => break,
                    Action::Close => close(&mut tabs, &id),
                    Action::Open(spec) => {
                        let media = tabs.remote[&id].state.media.clone();
                        tabs.add_remote(Tab::connecting(*spec, media));
                    }
                    Action::SameTarget(resume) => {
                        tabs.open_session(&id, resume);
                    }
                    Action::Restore => {
                        tabs.remote
                            .get_mut(&id)
                            .unwrap()
                            .state
                            .restore_picker =
                            Some(tui::restore::RestorePicker::from_tabs(&previous));
                    }
                    Action::Phone => {
                        let result = if let Some(connection) = tabs.remote[&id].connection.clone() {
                            tui::phone::run(&mut terminal, &mut events, connection).await
                        } else {
                            Err(anyhow::anyhow!("工作区尚未连接，请连接后再打开手机管理"))
                        };
                        if let Err(error) = result {
                            tabs.remote
                                .get_mut(&id)
                                .unwrap()
                                .state
                                .report_local(TranscriptItem::CommandError(error.to_string()));
                        }
                    }
                    Action::Connect => {
                        let tab = &tabs.remote[&id];
                        if let Some(local) = tab
                            .connection
                            .as_ref()
                            .and_then(|connection| connection.local_configuration.as_ref())
                        {
                            let settings = crate::ConnectionSettings {
                                config: local.config.clone(),
                                auth_directory: local.auth.clone(),
                            };
                            match tui::connect::run(&mut terminal, &mut events, &settings).await {
                                Ok(Some(_)) => {
                                    for tab in tabs.remote.values_mut().filter(|tab| {
                                        tab.connection
                                            .as_ref()
                                            .and_then(|c| c.local_configuration.as_ref())
                                            .is_some_and(|local| local.config == settings.config)
                                    }) {
                                        tab.reload_models();
                                    }
                                }
                                Ok(None) => {}
                                Err(error) => tabs
                                    .remote
                                    .get_mut(&id)
                                    .unwrap()
                                    .state
                                    .report_local(TranscriptItem::CommandError(error.to_string())),
                            }
                        }
                    }
                    Action::RefreshModels => tabs
                        .remote
                        .get_mut(&id)
                        .unwrap()
                        .reload_models(),
                    Action::Editor => {
                        let draft = tabs.remote[&id]
                            .state
                            .input
                            .text
                            .clone();
                        let command = std::env::var("VISUAL")
                            .ok()
                            .filter(|value| !value.trim().is_empty())
                            .or_else(|| std::env::var("EDITOR").ok())
                            .unwrap_or_else(|| "vi".into());
                        terminal::suspend();
                        let result = workspace::platform::editor::Editor::new(command)
                            .edit(&draft, &tabs.remote[&id].local_directory()?)
                            .await;
                        terminal::resume(&mut terminal)?;
                        let state = &mut tabs.remote.get_mut(&id).unwrap().state;
                        state.media.invalidate();
                        match result {
                            Ok(draft) => state.restore_editor_draft(draft),
                            Err(error) => {
                                state.report_local(TranscriptItem::CommandError(error.to_string()))
                            }
                        }
                    }
                    Action::Local => {
                        let media = tabs.remote[&id].state.media.clone();
                        let mut spec = Spec::local();
                        if let Some(configuration) = tabs.remote[&id]
                            .connection
                            .as_ref()
                            .and_then(|connection| connection.local_configuration.as_ref())
                        {
                            spec.inherit(&configuration.config, &configuration.auth, None);
                        }
                        tabs.add_remote(Tab::connecting(spec, media));
                    }
                    Action::None | Action::Redraw => {}
                }
                if let Some(selected) = tabs
                    .remote
                    .get_mut(&id)
                    .and_then(|tab| tab.state.restore_selection.take())
                {
                    let saved: Vec<_> = selected
                        .into_iter()
                        .filter_map(|value| value.parse::<usize>().ok())
                        .filter_map(|index| previous.get(index))
                        .collect();
                    tabs.restore_tabs(&id, &saved);
                }
                frames.request(time::Instant::now());
            }
            _ = ticks.tick() => {
                if tabs.poll_remote().await {
                    frames.request(time::Instant::now());
                }
                if persisted.elapsed() >= Duration::from_millis(500) {
                    persisted = std::time::Instant::now();
                    tabs.sync_remote_ui();
                    let saved = saved_tabs(&tabs);
                    let active = active_index(&tabs, &saved);
                    if remembered.as_ref().is_none_or(|(tabs, selected)| tabs != &saved || *selected != active) {
                        match window.update_active(saved.clone(), active).await {
                            Ok(()) => {
                                remembered = Some((saved, active));
                                save_error = None;
                            }
                            Err(error) => {
                                let message = format!("窗口和草稿尚未保存：{error:#}");
                                if save_error.as_ref() != Some(&message) {
                                    let tab = tabs.remote.get_mut(tabs.active_remote.as_ref().unwrap()).unwrap();
                                    tab.state.report_local(TranscriptItem::Error(message.clone()));
                                    frames.request(time::Instant::now());
                                    save_error = Some(message);
                                }
                            }
                        }
                    }
                }
            }
            _ = time::sleep_until(frames.deadline()), if frames.pending() => {
                tabs.sync_remote_ui();
                let state = &mut tabs
                    .remote
                    .get_mut(tabs.active_remote.as_ref().unwrap())
                    .unwrap()
                    .state;
                terminal::draw_synchronized(&mut terminal, |frame| {
                    render::draw_frame(frame, state);
                })?;
                frames.drawn(time::Instant::now());
                if state
                    .cursor_motion
                    .active(std::time::Instant::now())
                {
                    frames.request(time::Instant::now() + Duration::from_millis(16));
                }
            }
        }
    }
    tabs.sync_remote_ui();
    let saved = saved_tabs(&tabs);
    let active = active_index(&tabs, &saved);
    window
        .update_active(saved, active)
        .await?;
    window.close().await?;
    Ok(())
}

fn active_index(tabs: &Tabs, saved: &[workspace::windows::SavedTab]) -> Option<usize> {
    let id = tabs.active_remote.as_ref()?;
    let key = tabs
        .remote
        .get(id)?
        .restore_key
        .as_ref()
        .unwrap_or(id);
    saved
        .iter()
        .position(|saved| &saved.key == key)
}

pub(super) fn saved_tabs(tabs: &Tabs) -> Vec<workspace::windows::SavedTab> {
    let Some(active) = tabs
        .active_remote
        .as_ref()
        .and_then(|id| tabs.remote.get(id))
    else {
        return Vec::new();
    };
    active
        .state
        .tabs
        .iter()
        .filter_map(|entry| {
            let tab = tabs.remote.get(&entry.id)?;
            let draft = workspace::windows::Draft {
                text: tab.state.input.text.clone(),
                attachments: tab
                    .state
                    .attachments
                    .iter()
                    .map(|attachment| workspace::windows::DraftAttachment {
                        name: attachment.name().into(),
                        path: attachment.path().into(),
                    })
                    .collect(),
                leases: tab.state.attachments.clone(),
            };
            let mut saved = if let Some(connection) = tab.connection.as_ref() {
                let mut saved = workspace::windows::SavedTab::from_connection(
                    connection,
                    tab.id.clone()?,
                    tab.title.clone(),
                )?;
                if !tab.persisted {
                    saved.id = None;
                }
                saved
            } else {
                tab.spec
                    .as_ref()?
                    .saved_tab(entry.id.clone(), tab.title.clone(), draft.clone())?
            };
            saved.key = tab
                .restore_key
                .clone()
                .unwrap_or_else(|| entry.id.clone());
            saved.server = tab
                .spec
                .as_ref()
                .and_then(|spec| spec.server())
                .map(str::to_owned);
            saved.draft = draft;
            Some(saved)
        })
        .collect()
}

pub(super) fn close(tabs: &mut Tabs, id: &str) {
    let next = tabs.close_target(id).map(str::to_owned);
    if next.is_none() {
        let replacement = tabs.remote[id].sibling(None);
        tabs.add_remote(replacement);
    }
    tabs.remove_remote(id);
    if let Some(next) = next {
        tabs.active_remote = Some(next);
    }
}
