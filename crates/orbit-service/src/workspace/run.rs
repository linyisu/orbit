use anyhow::Result;
use orbit_agent::{error::AgentError, events::AgentEvent};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(super) type Started = (
    mpsc::Receiver<AgentEvent>,
    tokio::task::JoinHandle<Result<orbit_agent::agent::AgentResult, AgentError>>,
    CancellationToken,
    orbit_agent::inbox::AgentInbox,
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
    let crate::PreparedWorkspace { session, resources } = setup;
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
    use base64::Engine;
    anyhow::ensure!(
        !compact || uploads.is_empty(),
        "compaction does not accept images"
    );
    anyhow::ensure!(
        uploads.is_empty() || resources.model.supports_vision(),
        "selected model does not support images"
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
    ))
}
