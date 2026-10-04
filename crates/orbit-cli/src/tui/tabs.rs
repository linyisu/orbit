use std::collections::HashMap;

use orbit_model::{client::ModelClient, types::ChatMessage};

use crate::session::{PreparedResume, SessionState};

use super::app::{SessionTab, UiState};

pub(super) struct ParkedTab {
    pub session: SessionState,
    pub state: UiState,
    pub model: ModelClient,
    pub history: Vec<ChatMessage>,
}

pub(super) struct Tabs {
    order: Vec<String>,
    parked: HashMap<String, ParkedTab>,
    remembered: Vec<String>,
    window: Option<orbit_store::tui_windows::TuiWindow>,
}

impl Tabs {
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
        sync: &mut super::session_sync::SessionSync,
        owned: &super::background::Runs,
    ) -> anyhow::Result<bool> {
        let mut changed = false;
        for tab in self.parked.values_mut() {
            if owned.get(tab.session.id()).is_some_and(|run| run.busy()) {
                continue;
            }
            changed |= super::session_sync::sync_mode(
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
            changed |= super::session_sync::sync_runtime(
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
    }

    pub fn new(session: &SessionState) -> Self {
        Self {
            order: vec![session.id().to_owned()],
            parked: HashMap::new(),
            remembered: Vec::new(),
            window: None,
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

    pub fn adjacent(&self, id: &str, backwards: bool) -> Option<&str> {
        let index = self.order.iter().position(|item| item == id)?;
        let next = if backwards {
            (index + self.order.len() - 1) % self.order.len()
        } else {
            (index + 1) % self.order.len()
        };

        self.target(next)
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

    pub fn prune_closed(&mut self, runs: &super::background::Runs) -> Vec<String> {
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

    pub fn close_active(
        &mut self,
        session: &mut SessionState,
        state: &mut UiState,
        model: &mut ModelClient,
        history: &mut Vec<ChatMessage>,
    ) -> bool {
        let current_id = session.id().to_owned();
        let Some(next_id) = self.adjacent(&current_id, false).map(str::to_owned) else {
            return false;
        };
        if next_id == current_id {
            return false;
        }

        self.restore(&next_id, session, state, model, history);
        self.close_parked(&current_id);

        true
    }

    pub fn sync_ui(&self, state: &mut UiState, session: &SessionState) {
        state.tabs = self
            .order
            .iter()
            .filter_map(|id| {
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
                    active: id == session.id(),
                })
            })
            .collect();
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
        use super::super::{TabAction, tab_target};

        use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

        let dir = tempfile::tempdir().unwrap();
        let session = SessionState::open(dir.path().join("sessions.db"), None, "mock".into())
            .await
            .unwrap();
        let tabs = Tabs::new(&session);
        let mut state = UiState::new("mock");
        state.new_tab_area = ratatui::layout::Rect::new(20, 0, 3, 1);
        let event = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 21,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(tab_target(&event, &state, &tabs), Some(TabAction::New));

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
            tab_target(&close, &state, &tabs),
            Some(TabAction::Close("first".into()))
        );

        state.show_help = true;

        assert_eq!(tab_target(&event, &state, &tabs), None);
        assert_eq!(tab_target(&close, &state, &tabs), None);
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

        let second = session.prepare_new("mock".into()).await.unwrap();
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

        assert!(tabs.close_active(&mut session, &mut state, &mut model, &mut history));
        assert_eq!(session.id(), first_id);
        assert_eq!(
            tabs.prune_closed(&super::super::background::Runs::new())
                .as_slice(),
            std::slice::from_ref(&second_id)
        );
        assert!(session.store.acquire(&second_id).await.is_ok());
    }
}
