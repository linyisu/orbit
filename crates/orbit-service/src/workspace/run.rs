use anyhow::Result;

pub(super) struct Started {
    pub execution: crate::execution::StartedRun,
    pub image_assets: Vec<orbit_protocol::MessageImage>,
    pub file_assets: Vec<orbit_protocol::MessageImage>,
}

pub(super) async fn start(
    setup: &mut crate::PreparedWorkspace,
    prompt: Option<String>,
    uploads: Vec<orbit_protocol::ImageUpload>,
    files: Vec<orbit_protocol::FileUpload>,
    compact: bool,
    model: Option<&str>,
    inbox: Option<orbit_agent::inbox::AgentInbox>,
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
    let queued_input = prompt.is_none();
    let prompt = prompt.unwrap_or_default();
    setup
        .session
        .set_model_profile(
            setup.resources.model_name.clone(),
            setup
                .resources
                .model
                .config()
                .reasoning_effort,
        )
        .await?;
    let messages = context_messages(setup).await?;
    let crate::PreparedWorkspace { session, resources } = setup;
    let mut tools = resources.tools.clone();
    if !resources.model.supports_vision() {
        tools.remove("ReadImage");
    }
    anyhow::ensure!(
        !compact || (uploads.is_empty() && files.is_empty()),
        "compaction does not accept images"
    );
    let mut attachments = import_images(
        session.store.path(),
        resources.model.supports_vision(),
        resources
            .policy
            .disabled(orbit_protocol::Capability::Image),
        uploads,
    )
    .await?;
    attachments.extend(import_files(session.store.path(), files).await?);
    grant_files(&resources.workspace, &attachments)?;
    for message in &messages {
        if let orbit_model::types::ChatMessage::User { attachments, .. } = message {
            grant_files(&resources.workspace, attachments)?;
        }
    }
    if !compact {
        for message in session
            .store
            .pending_agent_messages(session.id())
            .await?
        {
            grant_files(&resources.workspace, &message.attachments)?;
        }
    }
    let metadata = |attachment: &orbit_model::types::Attachment| orbit_protocol::MessageImage {
        name: attachment.name().to_owned(),
        path: attachment
            .path()
            .to_string_lossy()
            .into_owned(),
    };
    let file_assets = attachments
        .iter()
        .filter(|a| !a.is_image())
        .map(metadata)
        .collect();
    let image_assets = attachments
        .iter()
        .filter(|a| a.is_image())
        .map(|attachment| orbit_protocol::MessageImage {
            name: attachment.name().to_owned(),
            path: attachment
                .path()
                .to_string_lossy()
                .into_owned(),
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
            generate_title: !compact && !queued_input,
            queued_input,
            inbox,
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
    Ok(Started {
        execution: started,
        image_assets,
        file_assets,
    })
}

pub(super) async fn context_messages(
    setup: &crate::PreparedWorkspace,
) -> Result<Vec<orbit_model::types::ChatMessage>> {
    let mut messages = setup.session.snapshot.history.clone();
    crate::instructions::inject(&mut messages, &setup.resources.budget.instructions);
    setup
        .resources
        .budget
        .skills
        .inject(&mut messages);
    if !setup
        .resources
        .policy
        .disabled(orbit_protocol::Capability::ProjectState)
    {
        crate::project_state::inject(
            &mut messages,
            &setup.session.store,
            &setup
                .session
                .snapshot
                .session
                .project_id,
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
    if !setup.resources.budget.auto_compact
        || setup
            .session
            .snapshot
            .history
            .is_empty()
    {
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

pub(super) async fn import_images(
    database: &std::path::Path,
    vision: bool,
    disabled: bool,
    uploads: Vec<orbit_protocol::ImageUpload>,
) -> Result<Vec<orbit_model::types::Attachment>> {
    use base64::Engine;
    anyhow::ensure!(
        uploads.is_empty() || vision,
        "selected model does not support images"
    );
    anyhow::ensure!(
        uploads.is_empty() || !disabled,
        "image capability is disabled"
    );
    let assets = orbit_tools::image_assets::ImageAssets::for_database(database);
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

    Ok(attachments)
}

const MAX_FILE_UPLOADS: usize = 16;
const MAX_FILE_NAME_BYTES: usize = 255;
const MAX_INLINE_FILE_BYTES: usize = 256 * 1_024;

/// Import immutable client files on the target host and retain their storage leases.
pub(super) async fn import_files(
    database: &std::path::Path,
    uploads: Vec<orbit_protocol::FileUpload>,
) -> Result<Vec<orbit_model::types::Attachment>> {
    use base64::Engine;
    anyhow::ensure!(
        uploads.len() <= MAX_FILE_UPLOADS,
        "单次最多添加 16 个文件附件"
    );
    let directory = orbit_runtime::assets::AssetDirectory::for_database(database);
    tokio::task::spawn_blocking(move || {
        uploads
            .into_iter()
            .map(|upload| {
                use orbit_model::types::Attachment;
                anyhow::ensure!(
                    !upload.name.is_empty()
                        && upload.name.len() <= MAX_FILE_NAME_BYTES
                        && !upload
                            .name
                            .chars()
                            .any(|c| c.is_control() || c == '/' || c == '\\'),
                    "附件文件名无效"
                );
                anyhow::ensure!(
                    upload.data.len() <= Attachment::MAX_FILE_BYTES.div_ceil(3) * 4,
                    "附件超过 20 MiB"
                );
                let bytes = base64::engine::general_purpose::STANDARD.decode(upload.data)?;
                anyhow::ensure!(bytes.len() <= Attachment::MAX_FILE_BYTES, "附件超过 20 MiB");
                let extension = std::path::Path::new(&upload.name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .filter(|e| {
                        !e.is_empty()
                            && e.len() <= orbit_runtime::assets::AssetDirectory::MAX_EXTENSION_BYTES
                            && e.bytes()
                                .all(|b| b.is_ascii_alphanumeric())
                    })
                    .unwrap_or("bin")
                    .to_ascii_lowercase();
                let text = (bytes.len() <= MAX_INLINE_FILE_BYTES && !bytes.contains(&0))
                    .then(|| {
                        std::str::from_utf8(&bytes)
                            .ok()
                            .map(str::to_owned)
                    })
                    .flatten();
                let (path, lease) = directory.import(&bytes, &extension)?;
                Ok(Attachment::File {
                    name: upload.name,
                    path,
                    text,
                    lease: Some(lease),
                })
            })
            .collect()
    })
    .await?
}

/// Attachment selection grants read access to those exact immutable files, never their parent.
pub(super) fn grant_files(
    workspace: &orbit_runtime::filesystem::Workspace,
    attachments: &[orbit_model::types::Attachment],
) -> Result<()> {
    for attachment in attachments
        .iter()
        .filter(|a| !a.is_image() && a.path().is_file())
    {
        let path = orbit_runtime::filesystem::ResolvedPath::resolve(attachment.path(), None)?;
        workspace
            .policy()
            .record_grant(orbit_runtime::policy::FilesystemGrant::read_only(
                orbit_runtime::capability::EnvironmentId::local(),
                path,
            ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
