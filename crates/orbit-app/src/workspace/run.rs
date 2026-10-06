use anyhow::{Result, bail};
use orbit_agent::{error::AgentError, events::AgentEvent};
use orbit_model::client::ModelClient;
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
    let mut tools = resources.tools.clone();
    if !resources.model.supports_vision() {
        tools.remove("read_image");
    }
    let started = crate::execution::start(
        session,
        prompt,
        Vec::new(),
        false,
        &messages,
        &resources.model,
        &tools,
        crate::execution::RunOptions {
            model_name: resources.model_name.clone(),
            generate_title: false,
            max_output_tokens: resources.budget.max_output_tokens,
            config: orbit_agent::agent::AgentConfig {
                context_tokens: resources.budget.context_tokens,
                auto_compact: resources.budget.auto_compact,
                max_turns: resources.budget.max_turns,
                max_tool_calls: resources.budget.max_tool_calls,
            },
            calibration: session.calibration(&resources.model_name),
            compaction: resources.budget.compaction.clone(),
        },
    )
    .await?;
    Ok((started.receiver, started.task, started.cancellation))
}
