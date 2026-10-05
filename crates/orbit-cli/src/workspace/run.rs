use std::sync::Arc;

use anyhow::{Result, bail};
use orbit_agent::{agent::Agent, error::AgentError, events::AgentEvent, model::ModelClientBackend};
use orbit_model::{
    client::ModelClient,
    types::{ChatMessage, ChatRequest},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(super) type Started = (
    mpsc::Receiver<AgentEvent>,
    tokio::task::JoinHandle<Result<orbit_agent::agent::AgentResult, AgentError>>,
    CancellationToken,
);

pub(super) async fn start(
    setup: &mut crate::PreparedWorkspace,
    prompt: String,
    model: Option<&str>,
) -> Result<Started> {
    let crate::PreparedWorkspace { session, resources } = setup;
    if let Some(name) = model {
        let selected = resources
            .budget
            .model_options
            .iter()
            .find(|option| option.qualified_name() == name);
        let Some(selected) = selected else {
            bail!("model is not configured: {name}");
        };
        resources.model = ModelClient::new(selected.config.clone())?;
        resources.model_name = selected.qualified_name();
        resources.budget.context_tokens = selected.context_tokens;
        resources.budget.input_context_tokens = selected.input_context_tokens;
        resources.budget.max_output_tokens = selected.max_output_tokens;
    }
    session
        .set_model_profile(
            resources.model_name.clone(),
            resources.model.config().reasoning_effort,
        )
        .await?;
    let mut messages = session.snapshot.history.clone();
    crate::instructions::inject(&mut messages, &resources.budget.instructions);
    resources.budget.skills.inject(&mut messages);
    crate::project_state::inject(
        &mut messages,
        &session.store,
        &session.snapshot.session.project_id,
        session.id(),
    )
    .await?;
    messages.push(ChatMessage::user(prompt.clone()));
    let mut request = ChatRequest::new(messages);
    request.max_tokens = Some(resources.budget.max_output_tokens);
    request.reasoning_effort = resources.model.config().reasoning_effort;
    let mut tools = resources.tools.clone();
    if !resources.model.supports_vision() {
        tools.remove("read_image");
    }
    let files = orbit_agent::memory::FileActivity::from_events(
        session.snapshot.runs.iter().flat_map(|run| &run.events),
    );
    let archive = Arc::new(crate::history::SessionArchive::new(
        session.store.clone(),
        session.id().into(),
    ));
    let mut agent = Agent::new(
        ModelClientBackend::new(resources.model.clone()),
        tools,
        orbit_agent::agent::AgentConfig {
            context_tokens: resources.budget.context_tokens,
            auto_compact: resources.budget.auto_compact,
            max_turns: resources.budget.max_turns,
            max_tool_calls: resources.budget.max_tool_calls,
        },
    )
    .with_file_activity(files)
    .with_compaction(resources.budget.compaction.clone())
    .with_context_archive(archive);
    session.begin(prompt, resources.model_name.clone()).await?;
    agent = agent
        .with_calibration(
            resources.model_name.clone(),
            session.calibration(&resources.model_name),
        )
        .with_execution_journal(session.execution_journal()?);
    let (sender, receiver) = mpsc::channel(256);
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    let task =
        tokio::spawn(async move { agent.run_with_agent_events(request, token, sender).await });
    Ok((receiver, task, cancellation))
}
