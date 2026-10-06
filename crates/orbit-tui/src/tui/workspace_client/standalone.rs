use crossterm::event::EventStream;
use tokio::time;

use super::*;
use crate::tui::{self, TabAction, frames::Frames, input_events, render, tabs::Tabs, terminal};

pub(super) async fn run(mut opened: Opened) -> Result<()> {
    let settings = opened
        .connection
        .local_configuration
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("本地连接配置缺失"))?;
    let directory = settings
        .auth
        .parent()
        .unwrap_or(&settings.auth)
        .join("windows");
    let window = workspace::windows::TuiWindow::open_for_connection(
        directory,
        &mut opened.client,
        &opened.connection,
    )
    .await?;
    let previous = window.previous().to_vec();
    let mut remembered = Vec::new();
    let mut persisted = std::time::Instant::now();
    let (_guard, mut terminal) = terminal::TerminalGuard::enter()?;
    let first = Tab::ready(opened, media::Context::detect(Arc::default()));
    let mut tabs = Tabs::default();
    tabs.add_remote(first);
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
                if !wheel.accept(&event, std::time::Instant::now()) { continue; }
                tabs.sync_remote_ui();
                let id = tabs.active_remote.clone().unwrap();
                let tab = tabs.remote.get_mut(&id).unwrap();
                tui::update_tab_hover(&event, &mut tab.state);
                let action = match tui::tab_target(&event, &tab.state) {
                    Some(TabAction::Select(next)) => { tabs.active_remote = Some(next); Action::None }
                    Some(TabAction::New) => Action::SameTarget(None),
                    Some(TabAction::Close(closing)) => { close(&mut tabs, &closing); Action::None }
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
                        let existing = resume.as_ref().and_then(|session| tabs.remote.iter().find(|(_, tab)| tab.id.as_ref() == Some(session) && tab.connection.as_ref().map(|c| &c.target) == tabs.remote[&id].connection.as_ref().map(|c| &c.target)).map(|(id, _)| id.clone()));
                        if let Some(existing) = existing { tabs.active_remote = Some(existing); }
                        else { let next = tabs.remote[&id].sibling(resume); tabs.add_remote(next); }
                    }
                    Action::Restore => {
                        tabs.remote.get_mut(&id).unwrap().state.restore_picker = Some(tui::restore::RestorePicker::from_tabs(&previous));
                    }
                    Action::Connect => {
                        let tab = &tabs.remote[&id];
                        if let Some(local) = tab.connection.as_ref().and_then(|connection| connection.local_configuration.as_ref()) {
                            let settings = crate::ConnectionSettings { config: local.config.clone(), auth_directory: local.auth.clone() };
                            match tui::connect::run(&mut terminal, &mut events, &settings).await {
                                Ok(Some(_)) => {
                                    for tab in tabs.remote.values_mut().filter(|tab| tab.connection.as_ref().and_then(|c| c.local_configuration.as_ref()).is_some_and(|local| local.config == settings.config)) { tab.reload_models(); }
                                }
                                Ok(None) => {},
                                Err(error) => tabs.remote.get_mut(&id).unwrap().state.report_local(TranscriptItem::CommandError(error.to_string())),
                            }
                        }
                    }
                    Action::RefreshModels => tabs.remote.get_mut(&id).unwrap().reload_models(),
                    Action::Editor => {
                        let draft = tabs.remote[&id].state.input.text.clone();
                        let command = std::env::var("VISUAL").ok().filter(|value| !value.trim().is_empty()).or_else(|| std::env::var("EDITOR").ok()).unwrap_or_else(|| "vi".into());
                        terminal::suspend();
                        let result = workspace::platform::editor::Editor::new(command).edit(&draft, &std::env::current_dir()?).await;
                        terminal::resume(&mut terminal)?;
                        let state = &mut tabs.remote.get_mut(&id).unwrap().state;
                        state.media.invalidate();
                        match result {
                            Ok(draft) => state.restore_editor_draft(draft),
                            Err(error) => state.report_local(TranscriptItem::CommandError(error.to_string())),
                        }
                    }
                    Action::Local => {
                        let media = tabs.remote[&id].state.media.clone();
                        let mut spec = Spec::local();
                        if let Some(configuration) = tabs.remote[&id].connection.as_ref().and_then(|connection| connection.local_configuration.as_ref()) {
                            spec.inherit(&configuration.config, &configuration.auth, None);
                        }
                        tabs.add_remote(Tab::connecting(spec, media));
                    }
                    Action::None | Action::Redraw => {}
                }
                if let Some(selected) = tabs.remote.get_mut(&id).and_then(|tab| tab.state.restore_selection.take()) {
                    let media = tabs.remote[&id].state.media.clone();
                    for index in selected.into_iter().filter_map(|value| value.parse::<usize>().ok()) {
                        if let Some(saved) = previous.get(index) {
                            tabs.add_remote(Tab::connecting(Spec::restored(saved), media.clone()));
                        }
                    }
                }
                frames.request(time::Instant::now());
            }
            _ = ticks.tick() => {
                if tabs.poll_remote().await { frames.request(time::Instant::now()); }
                if persisted.elapsed() >= Duration::from_millis(500) {
                    persisted = std::time::Instant::now();
                    tabs.sync_remote_ui();
                    let saved = saved_tabs(&tabs);
                    if saved != remembered { window.update(saved.clone()).await?; remembered = saved; }
                }
            }
            _ = time::sleep_until(frames.deadline()), if frames.pending() => {
                tabs.sync_remote_ui();
                let state = &mut tabs.remote.get_mut(tabs.active_remote.as_ref().unwrap()).unwrap().state;
                terminal::draw_synchronized(&mut terminal, |frame| {
                    if state.selection.draw_frozen(frame) { media::frozen_frame(); return; }
                    render::draw(frame, state);
                    state.selection.paint(frame);
                    state.cursor_motion.paint(frame.buffer_mut(), std::time::Instant::now());
                })?;
                frames.drawn(time::Instant::now());
                if state.cursor_motion.active(std::time::Instant::now()) { frames.request(time::Instant::now() + Duration::from_millis(16)); }
            }
        }
    }
    tabs.sync_remote_ui();
    window.update(saved_tabs(&tabs)).await?;
    window.close().await?;
    Ok(())
}

fn saved_tabs(tabs: &Tabs) -> Vec<workspace::windows::SavedTab> {
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
            if tab.state.is_home() && !tab.state.btw.has_records() {
                return None;
            }
            workspace::windows::SavedTab::from_connection(
                tab.connection.as_ref()?,
                tab.id.clone()?,
                tab.title.clone(),
            )
        })
        .collect()
}

fn close(tabs: &mut Tabs, id: &str) {
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
