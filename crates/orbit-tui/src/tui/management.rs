//! Session-bound native management surfaces. Transport receipts retain the
//! surface identity so a late reply cannot replace a newly opened editor.
mod files;
mod history;
mod keys;
mod queue;
mod resources;
mod review;
mod settings;
mod tasks;

use crate::workspace::protocol::{Request, Response};
use crossterm::event::Event;
use ratatui::{Frame, layout::Rect};

pub(super) enum Action {
    None,
    Close,
    Request(Request),
    Draft(String),
    Copy(String),
}

pub(super) trait Panel: std::fmt::Debug + Send {
    fn handle(&mut self, event: &Event) -> Action;
    fn receive(&mut self, response: Response) -> Action;
    fn error(&mut self, error: String);
    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect);
    fn invalidate_geometry(&mut self) {}
}

#[derive(Debug)]
pub(super) struct State {
    pub id: String,
    key: String,
    pub session: String,
    pub workspace: String,
    pub panel: Box<dyn Panel>,
}

impl State {
    pub fn new(session: String, workspace: String, panel: Box<dyn Panel>) -> Self {
        Self {
            id: crate::workspace::request_id(),
            key: String::new(),
            session,
            workspace,
            panel,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        let bounds = frame.area();
        let area = super::components::centered(bounds, 120, bounds.height.saturating_sub(2));
        let inner = super::components::modal(frame, area);
        self.panel.draw(frame, inner);
    }

    pub fn with_key(mut self, key: &str) -> Self {
        self.key = key.into();
        self
    }
}

/// Hiding an editor never discards it. Receipts can still settle hidden panels,
/// and reopening the same command restores its exact draft and cursor.
#[derive(Debug, Default)]
pub(super) struct Store {
    pub active: Option<State>,
    hidden: Vec<State>,
    approval_suspended: Option<String>,
}

impl Store {
    pub fn hide(&mut self) {
        if let Some(state) = self.active.take() {
            self.hidden.push(state);
        }
    }

    pub fn activate(&mut self, state: State) -> bool {
        self.approval_suspended = None;
        self.hide();
        if let Some(index) = self.hidden.iter().position(|old| {
            old.key == state.key && old.session == state.session && old.workspace == state.workspace
        }) {
            self.active = Some(self.hidden.remove(index));
            false
        } else {
            self.active = Some(state);
            true
        }
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut State> {
        if self
            .active
            .as_ref()
            .is_some_and(|state| state.id == id)
        {
            self.active.as_mut()
        } else {
            self.hidden
                .iter_mut()
                .find(|state| state.id == id)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.active.is_none() && self.hidden.is_empty()
    }

    pub fn approval(&mut self, pending: bool) {
        if pending {
            if let Some(state) = &self.active {
                self.approval_suspended = Some(state.id.clone());
                self.hide();
            }
        } else if let Some(id) = self.approval_suspended.take()
            && self.active.is_none()
            && let Some(index) = self
                .hidden
                .iter()
                .position(|state| state.id == id)
        {
            self.active = Some(self.hidden.remove(index));
        }
    }
}

pub(super) fn open(command: &str, session: &str, workspace: &str) -> Option<(State, Request)> {
    let (name, argument) = command
        .split_once(char::is_whitespace)
        .unwrap_or((command, ""));
    let argument = argument.trim();
    let session = session.to_owned();
    let workspace = workspace.to_owned();
    let (panel, request): (Box<dyn Panel>, Request) = match name {
        "/queue" if argument.is_empty() => {
            let mut panel = queue::State::new(session.clone(), workspace.clone());
            let request = panel.initial();
            (Box::new(panel), request)
        }
        "/skills" if argument.is_empty() => {
            let mut panel = resources::State::new(session.clone(), workspace.clone());
            let request = panel.initial();
            (Box::new(panel), request)
        }
        "/tasks" if argument.is_empty() => {
            let mut panel = tasks::State::new(session.clone(), workspace.clone());
            let request = panel.initial();
            (Box::new(panel), request)
        }
        "/file" => {
            let mut panel = files::State::new(session.clone(), workspace.clone())
                .at((!argument.is_empty()).then(|| argument.to_owned()));
            let request = panel.initial();
            (Box::new(panel), request)
        }
        "/review" if argument == "history" || argument.starts_with("history ") => {
            let revision = argument
                .strip_prefix("history")
                .unwrap()
                .trim();
            let mut panel = history::State::new(
                session.clone(),
                workspace.clone(),
                if revision.is_empty() {
                    "HEAD"
                } else {
                    revision
                },
            );
            let request = panel.initial();
            (Box::new(panel), request)
        }
        "/changes" | "/diff" | "/review" | "/undo" if name != "/undo" || argument.is_empty() => {
            let kind = match (name, argument) {
                ("/review", "staged") => orbit_protocol::review::Kind::Staged,
                ("/review", "untracked") => orbit_protocol::review::Kind::Untracked,
                ("/review", "checkpoint") => orbit_protocol::review::Kind::Checkpoint,
                ("/review", _) => orbit_protocol::review::Kind::Unstaged,
                _ => orbit_protocol::review::Kind::Checkpoint,
            };
            let path = (name == "/diff" && !argument.is_empty()).then(|| argument.to_owned());
            let mut panel = review::State::new(session.clone(), workspace.clone())
                .with_kind(kind)
                .at(path);
            let request = panel.initial();
            (Box::new(panel), request)
        }
        _ => return None,
    };
    let mut state = State::new(session, workspace, panel);
    state.key = command.trim().into();
    Some((state, request))
}

pub(super) fn local_keys(
    session: String,
    workspace: String,
    keymap: super::keymap::Shared,
) -> State {
    State::new(session, workspace, Box::new(keys::State::new(keymap))).with_key("/keys")
}

pub(super) fn local_settings(
    session: String,
    workspace: String,
    config: std::path::PathBuf,
    auth: std::path::PathBuf,
    mcp: bool,
) -> State {
    State::new(
        session,
        workspace,
        Box::new(settings::State::new(config, auth, mcp)),
    )
    .with_key(if mcp { "/mcp" } else { "/settings" })
}
