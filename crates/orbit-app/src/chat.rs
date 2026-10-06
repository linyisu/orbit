use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use anyhow::{Result, bail};
use orbit_agent::{
    agent::Agent, error::AgentError, events::AgentEvent, memory::FileActivity,
    model::ModelClientBackend,
};
use orbit_model::{
    client::ModelClient,
    types::{ChatMessage, ChatRequest},
};
use tokio_util::sync::CancellationToken;

use crate::{
    account_models, config::FileConfig, history, instructions, marketplaces, models, project_state,
    runtime, session, skills, subagents, verification,
};

pub struct ChatOptions {
    pub workspace: PathBuf,
    pub prompt: String,
    pub model: Option<String>,
    pub mode: ChatMode,
    pub resume: Option<String>,
    pub allow_mcp_start: bool,
    pub images: Vec<PathBuf>,
    pub disabled: Vec<ChatCapability>,
    pub export_patch: bool,
}

pub enum ChatUpdate<'a> {
    Agent(&'a AgentEvent),
    Diagnostic(String),
}

pub enum ChatOutcome {
    Completed {
        answer: String,
        model_name: String,
        patch: Option<String>,
    },
    Cancelled,
}

#[derive(Clone, Copy, Debug, Default)]
pub enum ChatMode {
    /// Register only read-only tools.
    ReadOnly,
    /// Automatically run operations inside the workspace.
    #[default]
    Auto,
    /// Automatically run sandbox commands and permit workspace edits.
    Yolo,
}

impl ChatMode {
    fn allows(
        self,
        preview: &orbit_tools::approval::ApprovalPreview,
        workspace: &std::path::Path,
    ) -> bool {
        match self {
            Self::ReadOnly => false,
            Self::Auto => orbit_tools::approval::auto_allows(preview, workspace),
            Self::Yolo => true,
        }
    }
}

/// Capability categories omitted from both tool registration and model requests.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ChatCapability {
    /// Every optional tool group below.
    All,
    /// Independent read-only delegated sessions.
    Subagents,
    /// web_search and web_fetch.
    Web,
    /// Configured local MCP processes and their tools.
    Mcp,
    /// Installed skill catalogue and load_skill.
    Skills,
    /// Native image attachments and read_image.
    Image,
    /// read_document.
    Document,
    /// read_file, list_files, search_text and project_overview.
    Files,
    /// git_inspect and git_export_patch.
    Git,
    /// write_file and edit_file.
    Write,
    /// request_capability for paths outside the workspace.
    Capability,
    /// run_command, read_command_output, verification_status and export_artifact.
    Command,
    /// Archived context retrieval and tool-output pruning.
    History,
    /// task and durable-memory tools.
    ProjectState,
}

fn capability_is_disabled(disabled: &BTreeSet<ChatCapability>, capability: ChatCapability) -> bool {
    disabled.contains(&ChatCapability::All) || disabled.contains(&capability)
}

pub async fn run(
    config_path: PathBuf,
    database: Option<PathBuf>,
    args: ChatOptions,
    auth_directory: PathBuf,
    cancellation: CancellationToken,
    mut report: impl FnMut(ChatUpdate<'_>) -> Result<()>,
) -> Result<ChatOutcome> {
    let prompt = args.prompt.clone();
    let disabled = args.disabled.iter().copied().collect::<BTreeSet<_>>();

    if capability_is_disabled(&disabled, ChatCapability::Image) && !args.images.is_empty() {
        bail!("--image cannot be used when the image capability is disabled");
    }

    let mut file_config = FileConfig::load(&config_path, &auth_directory)?;
    for diagnostic in account_models::refresh(&mut file_config, false).await {
        report(ChatUpdate::Diagnostic(diagnostic))?;
    }
    let database = session::database_path(database)?;

    // Open the persistent conversation before choosing a resumed model. A new
    // run starts with the configured default, while a resumed run keeps its
    // prior configured provider/model unless the caller explicitly overrides it.
    let default_model = file_config.default_model_option()?;
    let mut session = session::SessionState::open_in(
        database,
        args.resume.clone(),
        default_model.qualified_name(),
        &args.workspace,
    )
    .await?;
    let saved_model = args
        .resume
        .as_ref()
        .map(|_| session.snapshot.session.model.clone());
    let selected_model = select_chat_model(
        &file_config,
        args.model.as_deref().or(saved_model.as_deref()),
    )?;

    if !selected_model.supports_vision && !args.images.is_empty() {
        bail!(
            "--image cannot be used because model {:?} does not support image input",
            selected_model.qualified_name()
        );
    }

    let model_name = selected_model.qualified_name();
    let agent_config = file_config.agent_config_for(selected_model.context_tokens);
    let client = ModelClient::new(selected_model.config.clone())?;
    session
        .set_model_profile(model_name.clone(), client.config().reasoning_effort)
        .await?;

    let skills = if capability_is_disabled(&disabled, ChatCapability::Skills) {
        skills::Skills::default()
    } else {
        let enabled_plugins = session
            .store
            .project_plugins(&session.snapshot.session.project_id)
            .await?;
        let plugin_skill_roots = marketplaces::enabled_skill_roots(&enabled_plugins);

        skills::Skills::discover_with_plugin_roots(
            &args.workspace,
            std::env::var_os("HOME").map(PathBuf::from),
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            plugin_skill_roots.roots,
        )
    };

    let instructions = instructions::load_for(
        &args.workspace,
        instructions::default_user_instruction_dir().as_deref(),
        instructions::MAX_INSTRUCTION_BYTES,
    );
    let attachments = args
        .images
        .iter()
        .map(|path| session.import_image(path))
        .collect::<Result<Vec<_>>>()?;

    let (approvals, approval_task) = match args.mode {
        ChatMode::ReadOnly => (None, None),
        mode => {
            let (sender, mut receiver) = orbit_tools::approval::channel();
            let approval_workspace = std::path::PathBuf::from(&session.snapshot.session.workspace);
            let task = tokio::spawn(async move {
                while let Some(request) = receiver.recv().await {
                    let _ = request
                        .decision
                        .send(mode.allows(&request.preview, &approval_workspace));
                }
            });

            (Some(sender), Some(task))
        }
    };

    let runtime = runtime::workspace_tools(
        &args.workspace,
        &config_path,
        &auth_directory,
        Some(session.store.path()),
        approvals,
        file_config.command.clone(),
        file_config.web.clone(),
        if capability_is_disabled(&disabled, ChatCapability::Mcp) {
            orbit_tools::mcp::McpConfig::default()
        } else {
            file_config.mcp.clone()
        },
        args.allow_mcp_start,
        Some(session.scratch.clone()),
        Some(session.command_output.clone()),
        Some(file_config.permissions.clone()),
    )
    .await?;

    if !capability_is_disabled(&disabled, ChatCapability::Files)
        && !capability_is_disabled(&disabled, ChatCapability::Command)
    {
        session
            .initialize_audit(runtime.workspace.clone(), runtime.command_limits.clone())
            .await?;
    }

    let audit_workspace = runtime.workspace;
    let mut tools = runtime.tools;
    verification::register(
        &mut tools,
        session.store.clone(),
        project_state::MemoryScope::new(session.id().into()),
        audit_workspace,
        runtime.command_limits,
    );

    if !capability_is_disabled(&disabled, ChatCapability::History) {
        history::register(
            &mut tools,
            session.store.clone(),
            project_state::MemoryScope::new(session.id().into()),
        );
    }

    if !capability_is_disabled(&disabled, ChatCapability::ProjectState) {
        let memory_scope = project_state::MemoryScope::new(session.id().into());
        project_state::register(&mut tools, session.store.clone(), memory_scope);
    }

    if !capability_is_disabled(&disabled, ChatCapability::Skills) {
        skills.register_tool(&mut tools);
    }

    disable_chat_capabilities(&mut tools, &disabled);

    if !selected_model.supports_vision {
        tools.remove("read_image");
    }

    if capability_is_disabled(&disabled, ChatCapability::Files) {
        tools.remove("verification_status");
    }

    if session.store.is_subagent(session.id()).await? {
        subagents::restrict_child_tools(&mut tools);
    } else if !capability_is_disabled(&disabled, ChatCapability::Subagents) {
        subagents::register(
            &mut tools,
            session.store.clone(),
            session.id().into(),
            config_path.clone(),
            auth_directory.clone(),
            instructions.clone(),
            file_config.subagents.clone(),
        )
        .await?;
    }

    let mut messages = session.snapshot.history.clone();
    instructions::inject(&mut messages, &instructions);

    if !capability_is_disabled(&disabled, ChatCapability::Skills) {
        skills.inject(&mut messages);
    }

    if !capability_is_disabled(&disabled, ChatCapability::ProjectState) {
        project_state::inject(
            &mut messages,
            &session.store,
            &session.snapshot.session.project_id,
            session.id(),
        )
        .await?;
    }

    messages.push(ChatMessage::user_with_attachments(
        prompt.clone(),
        attachments,
    ));
    let mut request = ChatRequest::new(messages);
    request.max_tokens = Some(selected_model.max_output_tokens);
    request.reasoning_effort = client.config().reasoning_effort;

    let files = FileActivity::from_events(
        session
            .snapshot
            .runs
            .iter()
            .flat_map(|run| run.events.iter()),
    );
    let archive_enabled = tools.contains("read_history");
    let mut agent = Agent::new(ModelClientBackend::new(client), tools, agent_config)
        .with_file_activity(files)
        .with_compaction(file_config.compaction_runtime()?);

    if archive_enabled {
        agent = agent.with_context_archive(std::sync::Arc::new(history::SessionArchive::new(
            session.store.clone(),
            session.id().into(),
        )));
    }

    session.begin(prompt, model_name.clone()).await?;
    agent = agent
        .with_calibration(model_name.clone(), session.calibration(&model_name))
        .with_execution_journal(session.execution_journal()?);

    let task_cancellation = cancellation.clone();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(256);
    let task = tokio::spawn(async move {
        agent
            .run_with_agent_events(request, task_cancellation, sender)
            .await
    });

    while let Some(event) = receiver.recv().await {
        if let Err(error) = report(ChatUpdate::Agent(&event)) {
            cancellation.cancel();
            drop(receiver);
            let _ = task.await;
            if let Some(task) = approval_task {
                task.abort();
            }
            session.finish("failed", None).await?;
            return Err(error);
        }
    }

    let outcome = match task.await {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some(task) = approval_task {
                task.abort();
            }

            session.finish("failed", None).await?;
            return Err(error.into());
        }
    };

    if let Some(task) = approval_task {
        task.abort();
    }

    let result = match outcome {
        Ok(result) => result,
        Err(AgentError::Cancelled) if cancellation.is_cancelled() => {
            session.finish("cancelled", None).await?;
            return Ok(ChatOutcome::Cancelled);
        }

        Err(error) => {
            session.finish("failed", None).await?;
            return Err(error.into());
        }
    };

    let answer = match &result.final_message {
        ChatMessage::Assistant { content, .. } => content.clone().unwrap_or_default(),
        _ => String::new(),
    };

    session.finish("completed", Some(result.history)).await?;

    let patch = if args.export_patch {
        Some(orbit_tools::git_inspect::working_tree_patch(Path::new(
            &session.snapshot.session.workspace,
        ))?)
    } else {
        None
    };
    Ok(ChatOutcome::Completed {
        answer,
        model_name,
        patch,
    })
}

pub(crate) fn select_chat_model(
    config: &FileConfig,
    requested: Option<&str>,
) -> Result<models::ModelOption> {
    let Some(requested) = requested else {
        return config.default_model_option();
    };

    let requested = requested.trim();

    if requested.is_empty() {
        bail!("model must not be empty");
    }

    let options = config.model_options()?;

    if requested.contains("::") {
        return options
            .into_iter()
            .find(|option| option.qualified_name() == requested)
            .ok_or_else(|| anyhow::anyhow!("model {requested:?} is not configured"));
    }

    let matches = options
        .into_iter()
        .filter(|option| option.model == requested)
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [option] => Ok(option.clone()),
        [] => bail!("model {requested:?} is not configured"),
        _ => bail!("model {requested:?} is configured by multiple providers; use provider::model"),
    }
}

pub(crate) fn disable_chat_capabilities(
    tools: &mut orbit_agent::tool::ToolRegistry,
    disabled: &BTreeSet<ChatCapability>,
) {
    if capability_is_disabled(disabled, ChatCapability::Subagents) {
        for name in [
            "spawn_agent",
            "send_agent_message",
            "wait_agents",
            "read_agent",
            "cancel_agents",
        ] {
            tools.remove(name);
        }
    }

    if capability_is_disabled(disabled, ChatCapability::History) {
        tools.remove("read_history");
    }

    if capability_is_disabled(disabled, ChatCapability::Web) {
        tools.remove("web_search");
        tools.remove("web_fetch");
    }

    if capability_is_disabled(disabled, ChatCapability::Mcp) {
        tools.remove_prefix("mcp__");
    }

    if capability_is_disabled(disabled, ChatCapability::Skills) {
        tools.remove("load_skill");
    }

    if capability_is_disabled(disabled, ChatCapability::Image) {
        tools.remove("read_image");
    }

    if capability_is_disabled(disabled, ChatCapability::Document) {
        tools.remove("read_document");
    }

    if capability_is_disabled(disabled, ChatCapability::Files) {
        for name in [
            "read_file",
            "list_files",
            "search_text",
            "project_overview",
            "verification_status",
        ] {
            tools.remove(name);
        }
    }

    if capability_is_disabled(disabled, ChatCapability::Git) {
        tools.remove("git_inspect");
        tools.remove("git_export_patch");
    }

    if capability_is_disabled(disabled, ChatCapability::Write) {
        tools.remove("write_file");
        tools.remove("edit_file");
    }

    if capability_is_disabled(disabled, ChatCapability::Capability) {
        tools.remove("request_capability");
    }

    if capability_is_disabled(disabled, ChatCapability::Command) {
        tools.remove("run_command");
        tools.remove("read_command_output");
        tools.remove("get_command_status");
        tools.remove("wait_commands");
        tools.remove("cancel_commands");
        tools.remove("export_artifact");
        tools.remove("verification_status");
    }

    if capability_is_disabled(disabled, ChatCapability::ProjectState) {
        for name in [
            "task_list",
            "task_update",
            "memory_list",
            "memory_search",
            "memory_save",
            "memory_forget",
        ] {
            tools.remove(name);
        }
    }
}

#[cfg(test)]
mod tests;
