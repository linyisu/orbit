use std::{
    collections::HashSet,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Result, ensure};
use orbit_agent::model::ModelClientBackend;
use orbit_model::{client::ModelClient, types::ChatMessage};
use orbit_store::store::Store;
use tokio::sync::Mutex;

use crate::{PreparedWorkspace, side_questions::SideQuestions};

#[derive(Clone)]
struct Context {
    model: ModelClient,
    history: Vec<ChatMessage>,
    max_tokens: u32,
    persisted: bool,
}

#[derive(Default)]
struct State {
    questions: SideQuestions,
    viewers: HashSet<u64>,
}

impl State {
    fn release(&mut self, viewer: u64) -> bool {
        if self.viewers.remove(&viewer) && self.viewers.is_empty() {
            self.questions.clear();
            return true;
        }
        false
    }
}

#[derive(Clone)]
pub(super) struct Questions {
    service: Arc<Mutex<State>>,
    context: Arc<RwLock<Context>>,
    revision: Arc<AtomicU64>,
    store: Store,
    session: String,
}

impl Questions {
    pub fn new(setup: &PreparedWorkspace) -> Self {
        let service = Arc::new(Mutex::new(State::default()));
        let revision = Arc::new(AtomicU64::new(1));
        let weak = Arc::downgrade(&service);
        let changed = revision.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(100));
            loop {
                interval.tick().await;
                let Some(service) = weak.upgrade() else {
                    break;
                };
                if service.lock().await.questions.poll() {
                    changed.fetch_add(1, Ordering::Release);
                }
            }
        });
        Self {
            service,
            context: Arc::new(RwLock::new(Self::context(setup))),
            revision,
            store: setup.session.store.clone(),
            session: setup.session.id().into(),
        }
    }

    fn context(setup: &PreparedWorkspace) -> Context {
        let mut history = setup.session.snapshot.history.clone();
        crate::instructions::inject(&mut history, &setup.resources.budget.instructions);
        Context {
            model: setup.resources.model.clone(),
            history,
            max_tokens: setup.resources.budget.max_output_tokens,
            persisted: setup.session.is_persisted(),
        }
    }

    pub fn update(&self, setup: &PreparedWorkspace) {
        *self
            .context
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Self::context(setup);
    }

    pub async fn start(
        &self,
        viewer: u64,
        question: &str,
        follow_up: Option<usize>,
    ) -> Result<usize> {
        let question = question.trim();
        let context = self
            .context
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        // Read the main task's live checkpoint without acquiring a writer lease.
        // Untouched drafts have no database row and use their in-memory context.
        let mut history = if context.persisted {
            self.store.live_context(&self.session).await?
        } else {
            context.history.clone()
        };
        if !history
            .iter()
            .any(|m| matches!(m, ChatMessage::System { .. }))
        {
            history.splice(
                0..0,
                context
                    .history
                    .iter()
                    .filter(|m| matches!(m, ChatMessage::System { .. }))
                    .cloned(),
            );
        }
        let mut state = self.service.lock().await;
        ensure!(
            follow_up.is_none() || state.viewers.contains(&viewer),
            "旁路提问已关闭。请重新提问。"
        );
        let index = state.questions.start(
            &context.model,
            &history,
            question,
            context.max_tokens,
            Arc::new(ModelClientBackend::new(context.model.clone())),
            follow_up,
        )?;
        state.viewers.insert(viewer);
        self.revision.fetch_add(1, Ordering::Release);
        Ok(index)
    }

    pub async fn snapshot(
        &self,
        viewer: u64,
        after: Option<u64>,
        selected: Option<usize>,
    ) -> Result<orbit_protocol::Questions> {
        let state = self.service.lock().await;
        ensure!(state.viewers.contains(&viewer), "旁路提问已关闭。");
        let service = &state.questions;
        let revision = self.revision.load(Ordering::Acquire);
        Ok(orbit_protocol::Questions {
            revision,
            selected,
            running: service.running(),
            records: (after != Some(revision)).then(|| service.records().to_vec()),
        })
    }

    pub async fn open(&self, viewer: u64) -> Result<orbit_protocol::Questions> {
        let mut state = self.service.lock().await;
        ensure!(
            !state.questions.records().is_empty(),
            "没有正在查看的旁路对话，请使用 /btw <问题>。"
        );
        state.viewers.insert(viewer);
        Ok(orbit_protocol::Questions {
            revision: self.revision.load(Ordering::Acquire),
            selected: Some(state.questions.records().len() - 1),
            running: state.questions.running(),
            records: Some(state.questions.records().to_vec()),
        })
    }

    pub async fn release(&self, viewer: u64) {
        if self.service.lock().await.release(viewer) {
            self.revision.fetch_add(1, Ordering::Release);
        }
    }

    pub async fn cancel(&self, viewer: u64, index: usize) {
        let state = self.service.lock().await;
        if state.viewers.contains(&viewer) {
            state.questions.cancel(index);
        }
    }

    pub async fn running(&self) -> bool {
        self.service.lock().await.questions.running()
    }
}
