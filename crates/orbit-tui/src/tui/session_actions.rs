use super::*;

pub(super) async fn new_tab(
    tabs: &mut tabs::Tabs,
    session: &mut SessionState,
    state: &mut UiState,
    model: &mut ModelClient,
    history: &mut Vec<ChatMessage>,
    directory: &std::path::Path,
    context: CommandContext<'_>,
) -> Result<()> {
    let prepared = session
        .prepare_new_in(state.model.clone(), directory)
        .await?;
    let mut next_history = prepared.snapshot.history.clone();
    crate::instructions::inject(&mut next_history, context.instructions);
    context.skills.inject(&mut next_history);
    crate::project_state::inject(
        &mut next_history,
        &session.store,
        &prepared.snapshot.session.project_id,
        &prepared.snapshot.session.id,
    )
    .await?;

    let mut next_state = resume_ui_shell(state);
    next_state.workspace = prepared.snapshot.session.workspace.clone();
    next_state.permission_mode = permissions::Mode::default();
    next_state.reasoning_effort = state.reasoning_effort;
    next_state.reasoning_efforts = state.reasoning_efforts.clone();
    let next_model = ModelClient::new(model.config().clone())?;
    tabs.activate(
        session,
        state,
        model,
        history,
        (prepared, next_state, next_model, next_history),
    );
    refresh_context_usage(state, history, context.tools);
    session
        .set_model_profile(state.model.clone(), state.reasoning_effort)
        .await?;

    Ok(())
}

pub(super) async fn activate_resume(
    options: &crate::WorkspaceOptions,
    tabs: &mut tabs::Tabs,
    session: &mut SessionState,
    state: &mut UiState,
    model: &mut ModelClient,
    history: &mut Vec<ChatMessage>,
    next: (crate::session::PreparedResume, Vec<ChatMessage>),
) -> Result<crate::WorkspaceResources> {
    let (prepared, loaded_history) = next;
    let previous_id = session.id().to_owned();
    let restored_id = prepared.snapshot.session.id.clone();
    let directory = PathBuf::from(&prepared.snapshot.session.workspace);
    let mut restored = resume_ui_shell(state);
    restored.workspace = prepared.snapshot.session.workspace.clone();
    restored.resume_loading = Some("正在显示会话历史…".into());
    let next_model = ModelClient::new(model.config().clone())?;
    tabs.activate(
        session,
        state,
        model,
        history,
        (prepared, restored, next_model, loaded_history),
    );

    let result: Result<_> = async {
        let resources = crate::prepare_workspace_resources(options, &directory, session).await?;
        workspace::bind(&resources, session, state, model, history).await?;
        Ok::<_, anyhow::Error>(resources)
    }
    .await
    .with_context(|| format!("会话 {restored_id}，工作区 {}", directory.display()));

    if result.is_err() {
        tabs.restore(&previous_id, session, state, model, history);
        tabs.close_parked(&restored_id);
    }

    result
}

pub(super) async fn load_resume(
    store: orbit_store::store::Store,
    project: String,
    id: String,
    instructions: crate::instructions::Instructions,
    skills: crate::skills::Skills,
) -> Result<(crate::session::PreparedResume, Vec<ChatMessage>)> {
    let prepared = SessionState::prepare_shared(store.clone(), project, id).await?;
    let mut history = prepared.snapshot.history.clone();
    crate::instructions::inject(&mut history, &instructions);
    skills.inject(&mut history);
    crate::project_state::inject(
        &mut history,
        &store,
        &prepared.snapshot.session.project_id,
        &prepared.snapshot.session.id,
    )
    .await?;

    Ok((prepared, history))
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(super) async fn resume_session(
    id: &str,
    state: &mut UiState,
    model: &mut ModelClient,
    history: &mut Vec<ChatMessage>,
    session: &mut SessionState,
    instructions: &crate::instructions::Instructions,
    skills: &crate::skills::Skills,
    model_options: &[ModelOption],
    _memory_scope: &crate::project_state::MemoryScope,
    tools: &ToolRegistry,
) -> Result<()> {
    if id == session.id() {
        return Ok(());
    }

    let (prepared, loaded_history) = load_resume(
        session.store.clone(),
        session.snapshot.session.project_id.clone(),
        id.into(),
        instructions.clone(),
        skills.clone(),
    )
    .await?;

    drop(session.replace_with(prepared));
    *history = loaded_history;
    let mut restored = resume_ui_shell(state);
    while !crate::tui::replay::HistoryReplay::default().advance(
        &session.snapshot,
        &mut restored,
        usize::MAX,
        Duration::MAX,
    ) {}
    *state = restored;
    restore_model_profile(state, model, model_options, session);
    refresh_context_usage(state, history, tools);

    Ok(())
}

pub(super) fn resume_ui_shell(state: &UiState) -> UiState {
    let mut restored = UiState::new(state.model.clone());
    // Capabilities and media workers belong to the terminal, not the session.
    restored.media = state.media.clone();
    restored.cursor_motion.enabled = state.cursor_motion.enabled;
    restored.context_tokens = state.context_tokens;
    restored.input_context_tokens = state.input_context_tokens;
    restored.auto_compact = state.auto_compact;
    restored.max_output_tokens = state.max_output_tokens;
    restored.model_completions = state.model_completions.clone();
    restored.completion_home = state.completion_home.clone();
    restored.skill_completions = state.skill_completions.clone();
    restored.permission_mode = state.permission_mode;
    restored.supports_vision = state.supports_vision;

    restored
}

pub(super) use orbit_service::tools::command_output::read_command_stdout;

pub(super) fn restore_model_profile(
    state: &mut UiState,
    model: &mut ModelClient,
    model_options: &[ModelOption],
    session: &SessionState,
) {
    let (saved_model, saved_effort) = session.saved_model_profile();
    let Some(choice) = model_options
        .iter()
        .find(|choice| choice.qualified_name() == saved_model)
    else {
        return;
    };

    let mut config = choice.config.clone();

    if let Some(effort) = saved_effort.filter(|effort| choice.reasoning_efforts.contains(effort)) {
        config.reasoning_effort = Some(effort);
    }

    let Ok(client) = ModelClient::new(config) else {
        return;
    };

    *model = client;
    state.model = choice.qualified_name();
    state.max_output_tokens = choice.max_output_tokens;
    state.context_tokens = choice.context_tokens;
    state.input_context_tokens = choice.input_context_tokens;
    state.reasoning_effort = model.config().reasoning_effort;
    state.reasoning_efforts = choice.reasoning_efforts.clone();
    state.supports_vision = choice.supports_vision;
}

#[cfg(test)]
pub(super) async fn prepare_test_session(
    session: &SessionState,
    model: String,
) -> Result<crate::session::PreparedResume> {
    session
        .prepare_new_in(
            model,
            std::path::Path::new(&session.snapshot.session.workspace),
        )
        .await
}

#[cfg(test)]
pub(super) async fn new_test_session(session: &mut SessionState, model: String) -> Result<()> {
    let prepared = prepare_test_session(session, model).await?;
    drop(session.replace_with(prepared));
    if let Some(workspace) = session.audit_workspace.clone() {
        session
            .initialize_audit(workspace, session.audit_limits.clone())
            .await?;
    }
    Ok(())
}
