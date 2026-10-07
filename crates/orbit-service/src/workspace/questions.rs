use std::{
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
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
}

#[derive(Clone)]
pub(super) struct Questions {
    service: Arc<Mutex<SideQuestions>>,
    context: Arc<RwLock<Context>>,
    revision: Arc<AtomicU64>,
    store: Store,
    session: String,
}

impl Questions {
    pub fn new(setup: &PreparedWorkspace) -> Self {
        let service = Arc::new(Mutex::new(SideQuestions::default()));
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
                if service.lock().await.poll() {
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
        }
    }

    pub fn update(&self, setup: &PreparedWorkspace) {
        *self
            .context
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Self::context(setup);
    }

    pub async fn start(&self, question: &str, follow_up: Option<usize>) -> Result<usize> {
        let question = question.trim();
        let context = self
            .context
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let index = self
            .service
            .lock()
            .await
            .start(
                &self.store,
                &self.session,
                &context.model,
                &context.history,
                question,
                context.max_tokens,
                Arc::new(ModelClientBackend::new(context.model.clone())),
                follow_up,
            )
            .await?;
        self.revision.fetch_add(1, Ordering::Release);
        Ok(index)
    }

    pub async fn snapshot(
        &self,
        after: Option<u64>,
        selected: Option<usize>,
    ) -> Result<orbit_protocol::Questions> {
        let mut service = self.service.lock().await;
        service.load(&self.store, &self.session).await?;
        let revision = self.revision.load(Ordering::Acquire);
        Ok(orbit_protocol::Questions {
            revision,
            selected,
            running: service.running(),
            records: (after != Some(revision)).then(|| service.records().to_vec()),
        })
    }

    pub async fn cancel(&self, index: usize) {
        self.service.lock().await.cancel(index);
    }

    pub async fn running(&self) -> bool {
        self.service.lock().await.running()
    }
}
