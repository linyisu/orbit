use std::collections::HashMap;

use orbit_model::{client::ModelClient, types::ChatMessage};

use super::super::app::{SessionTab, UiState};
use crate::session::{PreparedResume, SessionState};

pub(crate) struct ParkedTab {
    pub session: SessionState,
    pub state: UiState,
    pub model: ModelClient,
    pub history: Vec<ChatMessage>,
}

#[derive(Default)]
pub(crate) struct Tabs {
    pub remote: HashMap<String, super::super::workspace_client::Tab>,
    pub active_remote: Option<String>,
    order: Vec<String>,
    discovery_revision: u64,
    parked: HashMap<String, ParkedTab>,
    remembered: Vec<String>,
    window: Option<orbit_store::tui_windows::TuiWindow>,
}

impl Tabs {
    pub fn add_remote(&mut self, tab: super::super::workspace_client::Tab) {
        let id = format!("workspace:{}", crate::workspace::request_id());
        self.order.push(id.clone());
        self.remote.insert(id.clone(), tab);
        self.active_remote = Some(id);
    }

    pub fn remove_remote(&mut self, id: &str) -> bool {
        if self.remote.remove(id).is_none() {
            return false;
        }
        self.order.retain(|item| item != id);
        if self.active_remote.as_deref() == Some(id) {
            self.active_remote = None;
        }
        true
    }

    pub fn visible<'a>(&'a mut self, local: &'a mut UiState) -> &'a mut UiState {
        match self
            .active_remote
            .as_ref()
            .and_then(|id| self.remote.get_mut(id))
        {
            Some(tab) => &mut tab.state,
            None => local,
        }
    }

    pub async fn poll_remote(&mut self) -> bool {
        let revision = crate::workspace::discovery::revision();
        let mut changed = self.discovery_revision != revision;
        self.discovery_revision = revision;
        for tab in self.remote.values_mut() {
            changed |= tab.poll().await;
        }
        changed
    }

    fn remote_descriptor(&self, id: &str, tab: &super::super::workspace_client::Tab) -> SessionTab {
        SessionTab {
            id: id.into(),
            title: tab.title.clone(),
            status: tab.state.status.clone(),
            elapsed: tab.state.active_elapsed(),
            waiting_approval: tab.state.approval.is_some(),
            active: self.active_remote.as_deref() == Some(id),
        }
    }

    pub fn sync_remote_ui(&mut self) {
        let entries: Vec<_> = self
            .order
            .iter()
            .filter_map(|id| {
                self.remote
                    .get(id)
                    .map(|tab| self.remote_descriptor(id, tab))
            })
            .collect();
        for tab in self.remote.values_mut() {
            tab.state.tabs = entries.clone();
        }
    }

    pub async fn persist(&mut self, session: &SessionState) -> anyhow::Result<()> {
        if self.window.is_none() {
            self.window = Some(session.store.open_tui_window().await?);
        }
        let ids: Vec<_> = self
            .order
            .iter()
            .filter(|id| {
                let snapshot = if id.as_str() == session.id() {
                    &session.snapshot
                } else if let Some(tab) = self.parked.get(*id) {
                    &tab.session.snapshot
                } else {
                    return false;
                };
                let running = if id.as_str() == session.id() {
                    session.active_run_id().is_some()
                } else {
                    self.parked
                        .get(*id)
                        .is_some_and(|tab| tab.session.active_run_id().is_some())
                };
                running || !snapshot.runs.is_empty() || !snapshot.session.title.is_empty()
            })
            .cloned()
            .collect();

        if ids != self.remembered {
            self.window.as_ref().unwrap().update(ids.clone()).await?;
            self.remembered = ids;
        }

        Ok(())
    }

    pub fn parked_mut(&mut self, id: &str) -> Option<&mut ParkedTab> {
        self.parked.get_mut(id)
    }

    pub fn restore_snapshot(&self) -> &[String] {
        self.window.as_ref().map_or(&[], |window| window.previous())
    }

    pub async fn close(&mut self, session: &SessionState) -> anyhow::Result<()> {
        self.persist(session).await?;
        if let Some(window) = self.window.take() {
            window.close().await?;
        }
        Ok(())
    }

    pub async fn synchronize_parked(
        &mut self,
        sync: &mut super::super::session_sync::SessionSync,
        owned: &super::super::background::Runs,
    ) -> anyhow::Result<bool> {
        let mut changed = false;
        for tab in self.parked.values_mut() {
            if owned.get(tab.session.id()).is_some_and(|run| run.busy()) {
                continue;
            }
            changed |= super::super::session_sync::sync_mode(
                &tab.session.store,
                &tab.session.snapshot.session.id,
                &mut tab.state,
            )
            .await?;
            changed |= sync
                .poll(
                    &tab.session.store,
                    &mut tab.session.snapshot,
                    &mut tab.state,
                    &mut tab.history,
                )
                .await?;
            changed |= super::super::session_sync::sync_runtime(
                &tab.session.store,
                &tab.session.snapshot.session.id,
                &mut tab.state,
            )
            .await?;
        }

        Ok(changed)
    }

    pub async fn poll_titles(&mut self) -> bool {
        let mut changed = false;
        for tab in self.parked.values_mut() {
            changed |= tab.session.poll_title().await.unwrap_or(false);
        }
        changed
    }

    pub fn poll_side_questions(&mut self) -> bool {
        self.parked
            .values_mut()
            .fold(false, |changed, tab| tab.state.btw.poll() | changed)
    }

    pub fn has_drafts(&self) -> bool {
        self.parked
            .values()
            .any(|tab| !tab.state.input.text.is_empty() || !tab.state.attachments.is_empty())
            || self
                .remote
                .values()
                .any(|tab| !tab.state.input.text.is_empty())
    }

    pub fn new(session: &SessionState) -> Self {
        Self {
            order: vec![session.id().to_owned()],
            ..Self::default()
        }
    }

    pub fn empty_tab(&self, session: &SessionState, state: &UiState) -> Option<String> {
        (session.active_run_id().is_none()
            && !state.btw.has_records()
            && session.snapshot.runs.is_empty()
            && session
                .snapshot
                .history
                .iter()
                .all(|message| matches!(message, ChatMessage::System { .. }))
            && state.input.text.is_empty()
            && state.attachments.is_empty())
        .then(|| session.id().to_owned())
    }

    pub fn target(&self, index: usize) -> Option<&str> {
        self.order.get(index).map(String::as_str)
    }

    pub fn close_target(&self, id: &str) -> Option<&str> {
        let index = self.order.iter().position(|item| item == id)?;
        // Closing the leftmost tab selects the first remaining tab, rather
        // than wrapping to the other end as keyboard navigation does.
        self.target(if index == 0 { 1 } else { index - 1 })
    }

    pub fn contains(&self, id: &str) -> bool {
        self.order.iter().any(|item| item == id) || self.parked.contains_key(id)
    }

    pub fn activate(
        &mut self,
        session: &mut SessionState,
        state: &mut UiState,
        model: &mut ModelClient,
        history: &mut Vec<ChatMessage>,
        next: (PreparedResume, UiState, ModelClient, Vec<ChatMessage>),
    ) {
        self.active_remote = None;
        let current_id = session.id().to_owned();
        let (prepared, next_state, next_model, next_history) = next;
        let parked = ParkedTab {
            session: session.replace_with(prepared),
            state: std::mem::replace(state, next_state),
            model: std::mem::replace(model, next_model),
            history: std::mem::replace(history, next_history),
        };

        self.parked.insert(current_id, parked);
        state.cursor_motion.clear();
        state.input_dragging = false;
        state.tab_hover = None;
        state.tab_hover_tick = 0;

        let next_id = session.id().to_owned();

        if !self.order.contains(&next_id) {
            self.order.push(next_id);
        }
    }

    pub fn restore(
        &mut self,
        id: &str,
        session: &mut SessionState,
        state: &mut UiState,
        model: &mut ModelClient,
        history: &mut Vec<ChatMessage>,
    ) -> bool {
        let Some(parked) = self.parked.remove(id) else {
            return false;
        };

        self.active_remote = None;
        let current_id = session.id().to_owned();
        let current = ParkedTab {
            session: std::mem::replace(session, parked.session),
            state: std::mem::replace(state, parked.state),
            model: std::mem::replace(model, parked.model),
            history: std::mem::replace(history, parked.history),
        };

        self.parked.insert(current_id, current);
        if !self.order.iter().any(|item| item == id) {
            self.order.push(id.to_owned());
        }
        state.cursor_motion.clear();
        state.input_dragging = false;
        state.tab_hover = None;
        state.tab_hover_tick = 0;

        true
    }

    pub fn close_parked(&mut self, id: &str) -> bool {
        if !self.parked.contains_key(id) {
            return false;
        }

        self.order.retain(|item| item != id);

        true
    }

    pub fn prune_closed(&mut self, runs: &super::super::background::Runs) -> Vec<String> {
        let closed: Vec<_> = self
            .parked
            .keys()
            .filter(|id| {
                !self.order.contains(id)
                    && !runs.get(*id).is_some_and(|run| run.busy())
                    && !self
                        .parked
                        .get(*id)
                        .is_some_and(|tab| tab.state.btw.running())
            })
            .cloned()
            .collect();
        for id in &closed {
            self.parked.remove(id);
        }
        closed
    }

    pub fn close_tab(
        &mut self,
        id: &str,
        session: &mut SessionState,
        state: &mut UiState,
        model: &mut ModelClient,
        history: &mut Vec<ChatMessage>,
    ) -> bool {
        let Some(next_id) = self.close_target(id).map(str::to_owned) else {
            return false;
        };
        if self.remote.contains_key(&next_id) {
            self.active_remote = Some(next_id);
            self.order.retain(|item| item != id);
            return true;
        }
        self.active_remote = None;
        if next_id != session.id() && !self.restore(&next_id, session, state, model, history) {
            return false;
        }

        self.close_parked(id)
    }

    pub fn sync_ui(&mut self, state: &mut UiState, session: &SessionState) {
        state.tabs = self
            .order
            .iter()
            .filter_map(|id| {
                if let Some(tab) = self.remote.get(id) {
                    return Some(self.remote_descriptor(id, tab));
                }
                let (title, status, elapsed) = if id == session.id() {
                    (
                        &session.snapshot.session.title,
                        state.status.clone(),
                        state.active_elapsed(),
                    )
                } else {
                    let parked = self.parked.get(id)?;
                    (
                        &parked.session.snapshot.session.title,
                        parked.state.status.clone(),
                        parked.state.active_elapsed(),
                    )
                };

                let waiting_approval = if id == session.id() {
                    state.approval.is_some()
                } else {
                    self.parked.get(id)?.state.approval.is_some()
                };
                Some(SessionTab {
                    waiting_approval,
                    elapsed,
                    id: id.clone(),
                    title: title.clone(),
                    status,
                    active: self.active_remote.is_none() && id == session.id(),
                })
            })
            .collect();
        for tab in self.remote.values_mut() {
            tab.state.tabs = state.tabs.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn startup_keeps_a_new_home_and_only_restore_snapshot_remembers_old_tabs() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sessions.db");
        let mut owner = SessionState::open_shared_in(db.clone(), None, "mock".into(), dir.path())
            .await
            .unwrap();
        owner
            .begin("saved conversation".into(), "mock".into())
            .await
            .unwrap();
        owner
            .finish(
                "completed",
                Some(vec![ChatMessage::user("saved conversation")]),
            )
            .await
            .unwrap();
        let mut first = Tabs::new(&owner);
        first.persist(&owner).await.unwrap();
        first.close(&owner).await.unwrap();
        let next = SessionState::open_shared_in(db, None, "mock".into(), dir.path())
            .await
            .unwrap();
        let mut tabs = Tabs::new(&next);
        tabs.persist(&next).await.unwrap();
        assert_eq!(tabs.restore_snapshot(), [owner.id()]);
        assert_eq!(tabs.order, [next.id()]);
        assert!(tabs.parked.is_empty());
        tabs.close(&next).await.unwrap();
        let window = next.store.open_tui_window().await.unwrap();
        assert!(window.previous().is_empty());
        assert!(next.store.session(owner.id()).await.is_ok());
    }

    #[tokio::test]
    async fn plus_click_requests_a_new_tab_and_dialogs_block_it() {
        use super::super::super::{TabAction, tab_target};

        use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

        let dir = tempfile::tempdir().unwrap();
        let session = SessionState::open(dir.path().join("sessions.db"), None, "mock".into())
            .await
            .unwrap();
        let mut tabs = Tabs::new(&session);
        let mut state = UiState::new("mock");
        tabs.sync_ui(&mut state, &session);
        state.new_tab_area = ratatui::layout::Rect::new(20, 0, 3, 1);
        let event = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 21,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(tab_target(&event, &state), Some(TabAction::New));

        state
            .tab_areas
            .push((ratatui::layout::Rect::new(2, 0, 15, 1), "first".into()));
        state
            .tab_close_areas
            .push((ratatui::layout::Rect::new(14, 0, 3, 1), "first".into()));
        let close = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 15,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(
            tab_target(&close, &state),
            Some(TabAction::Close("first".into()))
        );

        state.show_help = true;

        assert_eq!(tab_target(&event, &state), None);
        assert_eq!(tab_target(&close, &state), None);
    }

    #[tokio::test]
    async fn closing_any_tab_selects_its_left_neighbor_or_first_remaining_tab() {
        let dir = tempfile::tempdir().unwrap();
        for (case, (active, closing, expected)) in [
            (0, 0, 1),
            (1, 1, 0),
            (2, 2, 1),
            (3, 3, 2),
            (0, 3, 2),
            (3, 0, 1),
            (2, 3, 2),
        ]
        .into_iter()
        .enumerate()
        {
            let mut session =
                SessionState::open(dir.path().join(format!("{case}.db")), None, "mock".into())
                    .await
                    .unwrap();
            let mut ids = vec![session.id().to_owned()];
            let mut tabs = Tabs::new(&session);
            let mut state = UiState::new("mock");
            state.input.set("draft 0".into());
            let mut model = ModelClient::new(Default::default()).unwrap();
            let mut history = vec![ChatMessage::user("message 0")];

            // Only one tab: the caller creates a replacement home. This method
            // must leave the existing session and draft intact until it succeeds.
            assert!(tabs.close_target(&ids[0]).is_none());
            assert!(!tabs.close_tab(&ids[0], &mut session, &mut state, &mut model, &mut history));
            assert_eq!(session.id(), ids[0]);
            assert_eq!(state.input.text, "draft 0");

            for index in 1..4 {
                let prepared =
                    crate::tui::session_actions::prepare_test_session(&session, "mock".into())
                        .await
                        .unwrap();
                ids.push(prepared.snapshot.session.id.clone());
                let mut next_state = UiState::new("mock");
                next_state.input.set(format!("draft {index}"));
                tabs.activate(
                    &mut session,
                    &mut state,
                    &mut model,
                    &mut history,
                    (
                        prepared,
                        next_state,
                        ModelClient::new(Default::default()).unwrap(),
                        vec![ChatMessage::user(format!("message {index}"))],
                    ),
                );
            }

            if active != 3 {
                assert!(tabs.restore(
                    &ids[active],
                    &mut session,
                    &mut state,
                    &mut model,
                    &mut history
                ));
            }

            assert_eq!(
                tabs.close_target(&ids[closing]),
                Some(ids[expected].as_str())
            );
            assert!(tabs.close_tab(
                &ids[closing],
                &mut session,
                &mut state,
                &mut model,
                &mut history
            ));
            assert_eq!(session.id(), ids[expected]);
            assert_eq!(state.input.text, format!("draft {expected}"));
            assert_eq!(
                history,
                vec![ChatMessage::user(format!("message {expected}"))]
            );
            assert_eq!(
                tabs.order,
                ids.iter()
                    .enumerate()
                    .filter(|(index, _)| *index != closing)
                    .map(|(_, id)| id.clone())
                    .collect::<Vec<_>>()
            );
            tabs.sync_ui(&mut state, &session);
            let focused: Vec<_> = state
                .tabs
                .iter()
                .filter(|tab| tab.active)
                .map(|tab| tab.id.as_str())
                .collect();

            assert_eq!(focused, [ids[expected].as_str()]);
            assert_eq!(
                tabs.prune_closed(&super::super::super::background::Runs::new()),
                [ids[closing].clone()]
            );
        }
    }

    #[tokio::test]
    async fn switching_preserves_each_tabs_draft_and_history_and_close_releases_lease() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = SessionState::open(dir.path().join("sessions.db"), None, "mock".into())
            .await
            .unwrap();
        let first_id = session.id().to_owned();
        let mut tabs = Tabs::new(&session);
        let mut state = UiState::new("mock");

        assert_eq!(tabs.empty_tab(&session, &state), Some(first_id.clone()));

        state.input.set("first draft".into());

        assert_eq!(tabs.empty_tab(&session, &state), None);

        let mut model = ModelClient::new(Default::default()).unwrap();
        let mut history = vec![ChatMessage::user("first message")];

        let second = crate::tui::session_actions::prepare_test_session(&session, "mock".into())
            .await
            .unwrap();
        let second_id = second.snapshot.session.id.clone();
        tabs.activate(
            &mut session,
            &mut state,
            &mut model,
            &mut history,
            (
                second,
                UiState::new("mock"),
                ModelClient::new(Default::default()).unwrap(),
                vec![ChatMessage::user("second message")],
            ),
        );
        state.input.set("second draft".into());

        assert!(tabs.restore(
            &first_id,
            &mut session,
            &mut state,
            &mut model,
            &mut history
        ));
        assert_eq!(state.input.text, "first draft");
        assert_eq!(history, vec![ChatMessage::user("first message")]);

        assert!(tabs.restore(
            &second_id,
            &mut session,
            &mut state,
            &mut model,
            &mut history
        ));
        assert_eq!(state.input.text, "second draft");
        assert_eq!(history, vec![ChatMessage::user("second message")]);

        assert!(tabs.close_tab(
            &second_id,
            &mut session,
            &mut state,
            &mut model,
            &mut history
        ));
        assert_eq!(session.id(), first_id);
        assert_eq!(
            tabs.prune_closed(&super::super::super::background::Runs::new())
                .as_slice(),
            std::slice::from_ref(&second_id)
        );
        assert!(session.store.acquire(&second_id).await.is_ok());
    }
}
