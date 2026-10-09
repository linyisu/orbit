use anyhow::Result;
use orbit_agent::{error::AgentError, events::AgentEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(super) type Started = (
    mpsc::Receiver<AgentEvent>,
    tokio::task::JoinHandle<Result<orbit_agent::agent::AgentResult, AgentError>>,
    CancellationToken,
    orbit_agent::inbox::AgentInbox,
    Vec<orbit_protocol::MessageImage>,
);

pub(super) async fn start(
    setup: &mut crate::PreparedWorkspace,
    prompt: String,
    uploads: Vec<orbit_protocol::ImageUpload>,
    compact: bool,
    model: Option<&str>,
) -> Result<Started> {
    if let Some(name) = model
        && name != setup.resources.model_name
    {
        super::profile::apply(&mut setup.resources, name, None)?;
    }
    anyhow::ensure!(
        setup
            .resources
            .budget
            .model_options
            .iter()
            .any(|choice| choice.qualified_name() == setup.resources.model_name),
        "当前模型已从配置中移除，请使用 /model 选择可用模型"
    );
    let messages = context_messages(setup).await?;
    let crate::PreparedWorkspace { session, resources } = setup;
    session
        .set_model_profile(
            resources.model_name.clone(),
            resources.model.config().reasoning_effort,
        )
        .await?;
    let mut tools = resources.tools.clone();
    if !resources.model.supports_vision() {
        tools.remove("read_image");
    }
    use base64::Engine;
    anyhow::ensure!(
        !compact || uploads.is_empty(),
        "compaction does not accept images"
    );
    anyhow::ensure!(
        uploads.is_empty() || resources.model.supports_vision(),
        "selected model does not support images"
    );
    anyhow::ensure!(
        uploads.is_empty() || !resources.policy.disabled(orbit_protocol::Capability::Image),
        "image capability is disabled"
    );
    let assets = orbit_tools::image_assets::ImageAssets::for_database(session.store.path());
    let attachments =
        tokio::task::spawn_blocking(move || -> Result<Vec<orbit_model::types::Attachment>> {
            uploads
                .into_iter()
                .map(|image| {
                    let bytes = base64::engine::general_purpose::STANDARD.decode(image.data)?;
                    assets.import_bytes(image.name, bytes)
                })
                .collect()
        })
        .await??;
    let image_assets = attachments
        .iter()
        .map(|attachment| match attachment {
            orbit_model::types::Attachment::Image { name, path, .. } => {
                orbit_protocol::MessageImage {
                    name: name.clone(),
                    path: path.to_string_lossy().into_owned(),
                }
            }
        })
        .collect();
    let started = crate::execution::start(
        session,
        prompt,
        attachments,
        compact,
        &messages,
        &resources.model,
        &tools,
        crate::execution::RunOptions {
            model_name: resources.model_name.clone(),
            generate_title: !compact,
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
    Ok((
        started.receiver,
        started.task,
        started.cancellation,
        started.inbox,
        image_assets,
    ))
}

pub(super) async fn context_messages(
    setup: &crate::PreparedWorkspace,
) -> Result<Vec<orbit_model::types::ChatMessage>> {
    let mut messages = setup.session.snapshot.history.clone();
    crate::instructions::inject(&mut messages, &setup.resources.budget.instructions);
    setup.resources.budget.skills.inject(&mut messages);
    if !setup
        .resources
        .policy
        .disabled(orbit_protocol::Capability::ProjectState)
    {
        crate::project_state::inject(
            &mut messages,
            &setup.session.store,
            &setup.session.snapshot.session.project_id,
            setup.session.id(),
        )
        .await?;
    }
    orbit_model::runtime_context::append_state(
        &mut messages,
        orbit_model::types::RuntimeStateKind::ToolPolicy,
        setup.resources.planning.context().await,
    );
    Ok(messages)
}

pub(super) async fn needs_compaction(setup: &crate::PreparedWorkspace) -> Result<bool> {
    if !setup.resources.budget.auto_compact || setup.session.snapshot.history.is_empty() {
        return Ok(false);
    }
    let messages = context_messages(setup).await?;
    let estimated =
        orbit_agent::context::estimate_tokens(&messages, &setup.resources.tools.definitions());
    let calibrated = setup
        .session
        .calibration(&setup.resources.model_name)
        .estimate(estimated);
    Ok(calibrated
        >= setup
            .resources
            .budget
            .input_context_tokens
            .saturating_mul(85)
            / 100)
}
