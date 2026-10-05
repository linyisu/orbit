use std::path::PathBuf;

use anyhow::Result;
use orbit_model::client::ModelClient;

use crate::{
    account_models, config::FileConfig, history, instructions, project_state, runtime, session,
    skills, subagents, verification,
};

#[derive(Clone)]
pub(crate) struct WorkspaceOptions {
    pub(crate) initial_directory: PathBuf,
    pub(crate) auth_directory: PathBuf,
    pub(crate) config: PathBuf,
    pub(crate) database: PathBuf,
    pub(crate) allow_mcp_start: bool,
    pub(crate) home: Option<PathBuf>,
    pub(crate) config_home: Option<PathBuf>,
    pub(crate) instruction_dir: Option<PathBuf>,
    pub(crate) editor_command: String,
}

pub(crate) struct PreparedWorkspace {
    pub(crate) session: session::SessionState,
    pub(crate) resources: WorkspaceResources,
}

pub(crate) struct WorkspaceResources {
    pub(crate) web_client: std::sync::Arc<orbit_tools::web_fetch::WebClient>,
    pub(crate) cursor_trail: bool,
    pub(crate) model: ModelClient,
    pub(crate) model_name: String,
    pub(crate) tools: orbit_agent::tool::ToolRegistry,
    pub(crate) undo_writer: orbit_tools::write_file::WriteFile,
    pub(crate) approvals: orbit_tools::approval::ApprovalReceiver,
    pub(crate) budget: RequestBudget,
    pub(crate) workspace: orbit_runtime::filesystem::Workspace,
    pub(crate) command_limits: orbit_tools::run_command::CommandLimits,
    pub(crate) moved: bool,
}

pub(crate) async fn prepare_workspace(
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
    let resources = prepare_workspace_resources(options, directory, &session).await?;
    session
        .initialize_audit(
            resources.workspace.clone(),
            resources.command_limits.clone(),
        )
        .await?;

    Ok(PreparedWorkspace { session, resources })
}

pub(crate) async fn prepare_workspace_resources(
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
    for diagnostic in account_models::refresh(&mut file_config, false).await {
        eprintln!("{diagnostic}");
    }
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

#[derive(Clone, Debug)]
pub struct ModelOption {
    pub max_output_tokens: u32,
    pub provider: String,
    pub model: String,
    /// Request budget passed to the agent, including the output upper bound.
    pub context_tokens: usize,
    /// Full usable input window shown to the user and used for compaction.
    pub input_context_tokens: usize,
    pub default_for_provider: bool,
    pub reasoning_efforts: Vec<orbit_model::types::ReasoningEffort>,
    pub supports_vision: bool,
    pub config: orbit_model::types::ModelConfig,
}

impl ModelOption {
    pub fn qualified_name(&self) -> String {
        format!("{}::{}", self.provider, self.model)
    }
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
