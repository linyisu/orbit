//! Conversation access without a model. Explicit edits keep their existing owner and policy.
use super::super::protocol::{Page, SessionInfo};
use crate::session::SessionState;
use anyhow::{Result, ensure};

pub(in crate::workspace) struct Browsing {
    pub session: SessionState,
    pub directory: std::path::PathBuf,
    pub home: Option<std::path::PathBuf>,
    pub selection: Option<(String, Option<String>)>,
    pub clients: usize,
    pub mode: orbit_protocol::Mode,
    pub mode_revision: u64,
    pub plan_mode: Option<orbit_protocol::plan::WorkMode>,
}

impl Browsing {
    pub(in crate::workspace) fn configuration(
        options: &crate::WorkspaceOptions,
    ) -> Result<crate::config::FileConfig> {
        let content = match std::fs::read_to_string(&options.config) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => "[providers]\n".into(),
            Err(error) => return Err(error.into()),
        };
        crate::config::FileConfig::parse(&content, &options.auth_directory)
    }

    pub fn info(&self) -> SessionInfo {
        let session = &self.session;
        let saved = &session.snapshot.session;
        SessionInfo {
            phase: orbit_protocol::SessionPhase::Browsing,
            id: saved.id.clone(),
            persisted: session.is_persisted(),
            title: saved.title.clone(),
            workspace: saved.workspace.clone(),
            home: self
                .home
                .as_ref()
                .map(|home| home.to_string_lossy().into_owned()),
            model: self
                .selection
                .as_ref()
                .map_or_else(|| saved.model.clone(), |(model, _)| model.clone()),
            models: Vec::new(),
            model_catalog: Vec::new(),
            mode: self.mode,
            mode_revision: self.mode_revision,
            branch: None,
            cursor_trail: false,
            auto_compact: false,
            skills: Vec::new(),
            supports_vision: false,
            reasoning_effort: self
                .selection
                .as_ref()
                .and_then(|(_, effort)| effort.clone())
                .or_else(|| saved.reasoning_effort.clone()),
            reasoning_efforts: Vec::new(),
            context_tokens: 0,
            input_context_tokens: 0,
            max_output_tokens: 0,
            context_used_tokens: None,
        }
    }

    pub async fn page(&self, after: usize) -> Result<Page> {
        let session = &self.session;
        let history = if session.is_persisted() {
            session
                .store
                .transcript_index(session.id())
                .await?
        } else {
            Vec::new()
        };
        let total = history
            .last()
            .map_or(0, |run| run.end());
        ensure!(
            after <= total,
            "event cursor no longer exists; reopen the session"
        );
        let updates = super::history_page(&session.store, session.id(), &history, after).await?;
        let next = after + updates.len();
        let mut planning = session
            .store
            .planning(session.id())
            .await?;
        if let Some(mode) = self.plan_mode {
            planning.mode = mode;
        }
        Ok(Page {
            planning,
            tool_progress: Vec::new(),
            timing: None,
            next,
            more: next < total,
            busy: false,
            model: self.info().model,
            updates,
            approvals: Vec::new(),
            user_inputs: Vec::new(),
        })
    }
}
