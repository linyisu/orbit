use std::path::PathBuf;

use anyhow::Result;
use orbit_model::client::ModelClient;

use crate::{
    account_models, config::FileConfig, history, instructions, models::ModelOption, project_state,
    runtime, session, skills, subagents, verification,
};

#[derive(Clone)]
pub struct WorkspaceOptions {
    pub initial_directory: PathBuf,
    pub auth_directory: PathBuf,
    pub config: PathBuf,
    pub database: PathBuf,
    pub allow_mcp_start: bool,
    pub home: Option<PathBuf>,
    pub config_home: Option<PathBuf>,
    pub instruction_dir: Option<PathBuf>,
    pub editor_command: String,
}

pub struct PreparedWorkspace {
    pub session: session::SessionState,
    pub resources: WorkspaceResources,
}

pub struct WorkspaceResources {
    pub diagnostics: Vec<String>,
    pub web_client: std::sync::Arc<orbit_tools::web_fetch::WebClient>,
    pub cursor_trail: bool,
    pub model: ModelClient,
    pub model_name: String,
    pub tools: orbit_agent::tool::ToolRegistry,
    pub undo_writer: orbit_tools::write_file::WriteFile,
    pub approvals: orbit_tools::approval::ApprovalReceiver,
    pub budget: RequestBudget,
    pub workspace: orbit_runtime::filesystem::Workspace,
    pub command_limits: orbit_tools::run_command::CommandLimits,
    pub moved: bool,
}

pub async fn prepare_workspace(
    options: &WorkspaceOptions,
    directory: &std::path::Path,
    resume: Option<String>,
) -> Result<PreparedWorkspace> {
    prepare(options, directory, resume, true).await
}

pub(super) async fn prepare_service(
    options: &WorkspaceOptions,
    resume: Option<String>,
) -> Result<PreparedWorkspace> {
    prepare(options, &options.initial_directory, resume, false).await
}

async fn prepare(
    options: &WorkspaceOptions,
    directory: &std::path::Path,
    resume: Option<String>,
    shared: bool,
) -> Result<PreparedWorkspace> {
    let config = FileConfig::load(&options.config, &options.auth_directory)?;
    let model = config.default_model_option()?.qualified_name();
    let mut session = if shared {
        session::SessionState::open_shared_in(options.database.clone(), resume, model, directory)
            .await?
    } else {
        session::SessionState::open_in(options.database.clone(), resume, model, directory).await?
    };
    let mut resources = prepare_workspace_resources(options, directory, &session).await?;
    let (saved_model, effort) = session.saved_model_profile();
    if let Some(choice) = resources
        .budget
        .model_options
        .iter()
        .find(|model| model.qualified_name() == saved_model)
    {
        let effort = effort.filter(|effort| choice.reasoning_efforts.contains(effort));
        super::profile::apply(&mut resources, saved_model, effort)?;
    }
    session
        .initialize_audit(
            resources.workspace.clone(),
            resources.command_limits.clone(),
        )
        .await?;

    Ok(PreparedWorkspace { session, resources })
}

pub async fn prepare_workspace_resources(
    options: &WorkspaceOptions,
    directory: &std::path::Path,
    session: &session::SessionState,
) -> Result<WorkspaceResources> {
    let config_path = &options.config;
    let instructions = instructions::load_for(
        directory,
        options.instruction_dir.as_deref(),
        instructions::MAX_INSTRUCTION_BYTES,
    );
    let (approvals, approval_receiver) = orbit_tools::approval::channel();
    let mut file_config = FileConfig::load(config_path, &options.auth_directory)?;
    if file_config.command.sandbox {
        orbit_runtime::execution::capabilities::check_sandbox().await?;
    }
    let diagnostics = account_models::refresh(&mut file_config, false).await;
    let model_options = file_config.model_options()?;
    let default_model = file_config.default_model_option()?;
    let agent_config = file_config.agent_config();
    let command_limits = file_config.command.clone();
    let web = file_config.web.clone();
    let mcp = file_config.mcp.clone();
    let max_output_tokens = default_model.max_output_tokens;
    let model_name = default_model.qualified_name();
    let context_tokens = default_model.context_tokens;
    let input_context_tokens = default_model.input_context_tokens;
    let client = ModelClient::new(default_model.config)?;
    let target_project = session.store.resolve_project(directory).await?;
    let enabled_plugins = session.store.project_plugins(&target_project.id).await?;
    let plugin_skill_roots = crate::marketplaces::enabled_skill_roots(&enabled_plugins);
    let skills = skills::Skills::discover_with_plugin_roots(
        directory,
        options.home.clone(),
        options.config_home.clone(),
        plugin_skill_roots.roots,
    );
    let undo_writer = runtime::workspace_writer(
        directory,
        config_path,
        &options.auth_directory,
        session.store.path(),
        approvals.clone(),
        Some(file_config.permissions.clone()),
    )?;
    let runtime = runtime::workspace_tools(
        directory,
        config_path,
        &options.auth_directory,
        Some(session.store.path()),
        Some(approvals),
        command_limits,
        web,
        mcp,
        options.allow_mcp_start,
        Some(session.scratch.clone()),
        Some(session.command_output.clone()),
        Some(file_config.permissions.clone()),
    )
    .await?;
    let audit_workspace = runtime.workspace.clone();
    let mut tools = runtime.tools;
    let memory_scope = project_state::MemoryScope::new(session.id().into());
    verification::register(
        &mut tools,
        session.store.clone(),
        memory_scope.clone(),
        audit_workspace,
        runtime.command_limits.clone(),
    );
    history::register(&mut tools, session.store.clone(), memory_scope.clone());
    project_state::register(&mut tools, session.store.clone(), memory_scope.clone());
    skills.register_tool(&mut tools);
    subagents::register(
        &mut tools,
        session.store.clone(),
        session.id().into(),
        config_path.clone(),
        options.auth_directory.clone(),
        instructions.clone(),
        file_config.subagents.clone(),
    )
    .await?;

    Ok(WorkspaceResources {
        diagnostics,
        web_client: runtime.web_client,
        cursor_trail: file_config.tui.cursor_trail,
        moved: false,
        workspace: runtime.workspace,
        command_limits: runtime.command_limits,
        model: client,
        model_name,
        tools,
        undo_writer,
        approvals: approval_receiver,
        budget: RequestBudget {
            context_tokens,
            input_context_tokens,
            max_output_tokens,
            auto_compact: agent_config.auto_compact,
            max_turns: agent_config.max_turns,
            max_tool_calls: agent_config.max_tool_calls,
            compaction: file_config.compaction_runtime()?,
            instructions,
            model_options,
            skills,
            plugin_skill_diagnostics: plugin_skill_roots.diagnostics,
            mcp_statuses: runtime.mcp_statuses,
            memory_scope,
        },
    })
}

pub struct RequestBudget {
    pub context_tokens: usize,
    pub input_context_tokens: usize,
    pub auto_compact: bool,
    pub max_output_tokens: u32,
    pub max_turns: usize,
    pub max_tool_calls: usize,
    pub instructions: crate::instructions::Instructions,
    pub skills: crate::skills::Skills,
    pub plugin_skill_diagnostics: Vec<String>,
    pub mcp_statuses: Vec<orbit_tools::mcp::McpServerStatus>,
    pub model_options: Vec<ModelOption>,
    pub compaction: orbit_agent::compaction::CompactionRuntime,
    pub memory_scope: crate::project_state::MemoryScope,
}
