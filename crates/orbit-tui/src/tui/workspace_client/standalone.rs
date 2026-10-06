use crossterm::event::EventStream;
use tokio::time;

use super::*;
use crate::tui::{self, TabAction, frames::Frames, input_events, render, tabs::Tabs, terminal};

pub(super) async fn run(opened: Opened) -> Result<()> {
    let (_guard, mut terminal) = terminal::TerminalGuard::enter()?;
    let first = Tab::ready(opened, media::Context::detect(Arc::default()));
    let mut tabs = Tabs::default();
    tabs.add_remote(first);
    let mut events = EventStream::new();
    let mut pending_event = None;
    let mut frames = Frames::default();
    frames.request(time::Instant::now());
    let mut ticks = time::interval(Duration::from_millis(33));
    loop {
        tokio::select! {
            event = input_events::next(&mut events, &mut pending_event) => {
                let Some(event) = event else { break };
                let event = event?;
                tabs.sync_remote_ui();
                let id = tabs.active_remote.clone().unwrap();
                let tab = tabs.remote.get_mut(&id).unwrap();
                tui::update_tab_hover(&event, &mut tab.state);
                let action = match tui::tab_target(&event, &tab.state) {
                    Some(TabAction::Select(next)) => { tabs.active_remote = Some(next); Action::None }
                    Some(TabAction::New) => Action::SameTarget(None),
                    Some(TabAction::Close(closing)) => {
                        close(&mut tabs, &closing);
                        if tabs.active_remote.is_none() { break; }
                        Action::None
                    }
                    None => tab.handle(event)?,
                };
                match action {
                    Action::Quit => break,
                    Action::Close => { close(&mut tabs, &id); if tabs.active_remote.is_none() { break; } }
                    Action::Open(spec) => {
                        let media = tabs.remote[&id].state.media.clone();
                        tabs.add_remote(Tab::connecting(*spec, media));
                    }
                    Action::SameTarget(resume) => { let next = tabs.remote[&id].sibling(resume); tabs.add_remote(next); }
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
                frames.request(time::Instant::now());
            }
            _ = ticks.tick() => {
                if tabs.poll_remote().await { frames.request(time::Instant::now()); }
            }
            _ = time::sleep_until(frames.deadline()), if frames.pending() => {
                tabs.sync_remote_ui();
                let state = &mut tabs.remote.get_mut(tabs.active_remote.as_ref().unwrap()).unwrap().state;
                terminal::draw_synchronized(&mut terminal, |frame| {
                    render::draw(frame, state);
                    state.selection.paint(frame);
                    state.cursor_motion.paint(frame.buffer_mut(), std::time::Instant::now());
                })?;
                frames.drawn(time::Instant::now());
                if state.cursor_motion.active(std::time::Instant::now()) { frames.request(time::Instant::now() + Duration::from_millis(16)); }
            }
        }
    }
    Ok(())
}

fn close(tabs: &mut Tabs, id: &str) {
    let next = tabs.close_target(id).map(str::to_owned);
    tabs.remove_remote(id);
    tabs.active_remote = next;
}
