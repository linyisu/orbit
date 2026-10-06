//! Rebind the execution workspace and its prompt together when switching sessions.
use orbit_model::{client::ModelClient, types::ChatMessage};

use super::{
    app::{TranscriptItem, UiState},
    restore_model_profile,
};
use crate::{WorkspaceResources, session::SessionState};

pub(super) async fn bind(
    resources: &WorkspaceResources,
    session: &mut SessionState,
    state: &mut UiState,
    model: &mut ModelClient,
    history: &mut Vec<ChatMessage>,
) -> anyhow::Result<()> {
    session
        .initialize_audit(
            resources.workspace.clone(),
            resources.command_limits.clone(),
        )
        .await?;
    state.workspace = session.snapshot.session.workspace.clone();
    state.media.set_client(resources.web_client.clone());
    state.subagent_parent = session.store.subagent_parent(session.id()).await?;
    state.cursor_motion.enabled = resources.cursor_trail;
    state.cursor_motion.clear();
    state.auto_compact = resources.budget.auto_compact;
    state.skill_completions = resources.budget.skills.completions();
    state.model_completions = resources
        .budget
        .model_options
        .iter()
        .map(super::ModelOption::qualified_name)
        .collect();
    restore_model_profile(state, model, &resources.budget.model_options, session);
    crate::instructions::inject(history, &resources.budget.instructions);
    resources.budget.skills.inject(history);
    crate::project_state::inject(
        history,
        &session.store,
        &session.snapshot.session.project_id,
        session.id(),
    )
    .await?;
    for diagnostic in &resources.diagnostics {
        state.report_local(TranscriptItem::Notice(diagnostic.clone()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn moving_workspace_preserves_model_and_new_tabs_use_the_launch_directory() {
        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join("old");
        let new = directory.path().join("new");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(old.join("AGENTS.md"), "old-workspace-rule").unwrap();
        std::fs::write(new.join("AGENTS.md"), "new-workspace-rule").unwrap();
        std::fs::write(new.join("marker.txt"), "destination").unwrap();
        let config = directory.path().join("config.toml");
        std::fs::write(
            &config,
            r#"default_provider = "test"
[providers.test]
base_url = "http://127.0.0.1:1/v1"
auto_refresh_models = false
api_key = "test-only"
[[providers.test.models]]
id = "default-model"
[[providers.test.models]]
id = "selected-model"
"#,
        )
        .unwrap();
        let options = crate::WorkspaceOptions {
            initial_directory: old.clone(),
            auth_directory: directory.path().join("auth"),
            config,
            database: directory.path().join("sessions.db"),
            allow_mcp_start: false,
            home: None,
            config_home: None,
            instruction_dir: None,
            editor_command: "vi".into(),
        };
        let prepared = crate::prepare_workspace(&options, &old, None)
            .await
            .unwrap();
        let mut session = prepared.session;
        session
            .set_model_profile("test::selected-model".into(), None)
            .await
            .unwrap();
        session.enable_shared();
        let mut peer = SessionState::open_shared_in(
            options.database.clone(),
            Some(session.id().into()),
            "test::selected-model".into(),
            &old,
        )
        .await
        .unwrap();

        peer.initialize_audit(
            prepared.resources.workspace.clone(),
            prepared.resources.command_limits.clone(),
        )
        .await
        .unwrap();
        let mut model = prepared.resources.model;
        let mut state = UiState::new("test::selected-model");
        let mut history = Vec::new();
        crate::instructions::inject(&mut history, &prepared.resources.budget.instructions);
        let resources = crate::prepare_workspace_resources(&options, &new, &session)
            .await
            .unwrap();
        session
            .move_to(
                resources.workspace.clone(),
                resources.command_limits.clone(),
            )
            .await
            .unwrap();
        bind(
            &resources,
            &mut session,
            &mut state,
            &mut model,
            &mut history,
        )
        .await
        .unwrap();
        assert_eq!(model.config().model, "selected-model");
        assert_eq!(state.model, "test::selected-model");
        assert_eq!(session.audit_workspace.as_ref().unwrap().root(), new);
        let prompt = format!("{history:?}");
        assert!(prompt.contains("new-workspace-rule"));
        assert!(!prompt.contains("old-workspace-rule"));
        let output = resources
            .tools
            .execute(
                orbit_model::types::ToolCall::function(
                    "read",
                    "read_file",
                    serde_json::json!({"path":"marker.txt"}),
                ),
                tokio_util::sync::CancellationToken::new(),
            )
            .await
            .unwrap();

        assert!(output.content.contains("destination"));
        assert!(
            peer.begin("peer request".into(), state.model.clone())
                .await
                .unwrap_err()
                .is::<crate::session::SessionRefreshRequired>()
        );
        let mut peer_state = UiState::new("test::selected-model");
        let mut peer_history = Vec::new();
        super::super::session_sync::SessionSync::default()
            .poll(
                &peer.store,
                &mut peer.snapshot,
                &mut peer_state,
                &mut peer_history,
            )
            .await
            .unwrap();
        let peer_resources = crate::prepare_workspace_resources(
            &options,
            std::path::Path::new(&peer.snapshot.session.workspace),
            &peer,
        )
        .await
        .unwrap();

        bind(
            &peer_resources,
            &mut peer,
            &mut peer_state,
            &mut model,
            &mut peer_history,
        )
        .await
        .unwrap();
        assert_eq!(peer_state.workspace, new.to_string_lossy());
        assert_eq!(peer.audit_workspace.as_ref().unwrap().root(), new);
        peer.begin("peer request".into(), peer_state.model.clone())
            .await
            .unwrap();
        peer.finish("completed", Some(peer_history)).await.unwrap();
        session.snapshot = session.store.load(session.id()).await.unwrap();
        session
            .begin("hello".into(), state.model.clone())
            .await
            .unwrap();
        session.finish("completed", Some(history)).await.unwrap();

        let moved_id = session.id().to_owned();
        let mut tabs = super::super::tabs::Tabs::new(&session);
        let mut history = session.snapshot.history.clone();
        super::super::new_tab(
            &mut tabs,
            &mut session,
            &mut state,
            &mut model,
            &mut history,
            &options.initial_directory,
            super::super::CommandContext {
                tools: &resources.tools,
                instructions: &resources.budget.instructions,
                skills: &resources.budget.skills,
                mcp_statuses: &resources.budget.mcp_statuses,
                model_options: &resources.budget.model_options,
            },
        )
        .await
        .unwrap();
        assert_ne!(session.id(), moved_id);
        assert_eq!(state.workspace, old.to_str().unwrap());
        assert_eq!(session.snapshot.session.workspace, old.to_str().unwrap());
        assert!(state.transcript.is_empty());
        let fresh_resources = crate::prepare_workspace_resources(&options, &old, &session)
            .await
            .unwrap();
        bind(
            &fresh_resources,
            &mut session,
            &mut state,
            &mut model,
            &mut history,
        )
        .await
        .unwrap();
        assert_eq!(model.config().model, "selected-model");
        assert_eq!(session.audit_workspace.as_ref().unwrap().root(), old);
        assert!(format!("{history:?}").contains("old-workspace-rule"));
        assert!(!format!("{history:?}").contains("new-workspace-rule"));
        assert!(tabs.restore(
            &moved_id,
            &mut session,
            &mut state,
            &mut model,
            &mut history
        ));
        assert_eq!(session.snapshot.session.workspace, new.to_str().unwrap());
        assert_eq!(session.audit_workspace.as_ref().unwrap().root(), new);
    }
}
