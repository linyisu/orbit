use std::collections::HashMap;

use orbit_model::{client::ModelClient, types::ChatMessage};

use crate::session::{PreparedResume, SessionState};

use super::app::{SessionTab, UiState};

struct ParkedTab {
    prepared: PreparedResume,
    state: UiState,
    model: ModelClient,
    history: Vec<ChatMessage>,
}

pub(super) struct Tabs {
    order: Vec<String>,
    parked: HashMap<String, ParkedTab>,
}

impl Tabs {
    pub fn new(session: &SessionState) -> Self {
        Self {
            order: vec![session.id().to_owned()],
            parked: HashMap::new(),
        }
    }

    pub fn empty_tab(&self, session: &SessionState, state: &UiState) -> Option<String> {
        (session.snapshot.runs.is_empty()
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
        self.order.iter().any(|item| item == id)
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
            prepared: session.swap_resume(prepared),
            state: std::mem::replace(state, next_state),
            model: std::mem::replace(model, next_model),
            history: std::mem::replace(history, next_history),
        };
        self.parked.insert(current_id, parked);
        state.tab_hover = None;
        state.tab_hover_tick = 0;

        let next_id = session.id().to_owned();
        if !self.contains(&next_id) {
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
        self.activate(
            session,
            state,
            model,
            history,
            (parked.prepared, parked.state, parked.model, parked.history),
        );
        true
    }

    pub fn close_parked(&mut self, id: &str) -> bool {
        if self.parked.remove(id).is_none() {
            return false;
        }
        self.order.retain(|item| item != id);
        true
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
        self.parked.remove(&current_id);
        self.order.retain(|id| id != &current_id);
        true
    }

    pub fn sync_ui(&self, state: &mut UiState, session: &SessionState) {
        state.tabs = self
            .order
            .iter()
            .filter_map(|id| {
                let (title, status) = if id == session.id() {
                    (&session.snapshot.session.title, state.status.clone())
                } else {
                    let parked = self.parked.get(id)?;
                    (
                        &parked.prepared.snapshot.session.title,
                        parked.state.status.clone(),
                    )
                };
                Some(SessionTab {
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
        assert!(session.store.acquire(&second_id).await.is_ok());
    }
}
