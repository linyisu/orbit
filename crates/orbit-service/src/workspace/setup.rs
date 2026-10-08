#[cfg(test)]
use orbit_config::test_support::{TestConfig, TestModel, TestProvider};
use std::path::PathBuf;

use anyhow::Result;
use orbit_model::client::ModelClient;

use crate::{
    config::FileConfig, history, instructions, models::ModelOption, project_state, runtime,
    session, skills, subagents, verification,
};

#[derive(Clone)]
pub struct WorkspaceOptions {
    pub model: Option<String>,
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
    pub options: WorkspaceOptions,
    pub mode: orbit_protocol::Mode,
    pub mode_revision: u64,
    pub policy: orbit_protocol::SessionPolicy,
    pub diagnostics: Vec<String>,
    pub web_client: std::sync::Arc<orbit_tools::web_fetch::WebClient>,
    pub cursor_trail: bool,
    pub model: ModelClient,
    pub model_name: String,
    pub tools: orbit_agent::tool::ToolRegistry,
    pub undo_writer: orbit_tools::write_file::WriteFile,
    pub approvals: orbit_tools::approval::ApprovalReceiver,
    pub user_input: crate::user_input::Questions,
    pub planning: crate::planning::Planning,
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
    prepare(options, directory, resume, true, Default::default()).await
}

#[cfg(test)]
pub(super) async fn prepare_service(
    options: &WorkspaceOptions,
    resume: Option<String>,
) -> Result<PreparedWorkspace> {
    prepare_service_with_policy(options, resume, Default::default()).await
}

pub(super) async fn prepare_service_with_policy(
    options: &WorkspaceOptions,
    resume: Option<String>,
    policy: orbit_protocol::SessionPolicy,
) -> Result<PreparedWorkspace> {
    prepare(options, &options.initial_directory, resume, false, policy).await
}

async fn prepare(
    options: &WorkspaceOptions,
    directory: &std::path::Path,
    resume: Option<String>,
    shared: bool,
    policy: orbit_protocol::SessionPolicy,
) -> Result<PreparedWorkspace> {
    let mut config = FileConfig::load(&options.config, &options.auth_directory)?;
    let _ = orbit_config::models::updater::refresh(&mut config, false).await;
    let model = if resume.is_some() {
        String::new()
    } else {
        config
            .select_model(
                options
                    .model
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("请选择模型后开始会话"))?,
            )?
            .qualified_name()
    };
    let mut session = if shared {
        session::SessionState::open_shared_in(options.database.clone(), resume, model, directory)
            .await?
    } else {
        session::SessionState::open_in(options.database.clone(), resume, model, directory).await?
    };
    let mut resources = prepare_resources(options, directory, &session, policy).await?;
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
    prepare_resources(options, directory, session, Default::default()).await
}

pub(super) async fn prepare_resources(
    options: &WorkspaceOptions,
    directory: &std::path::Path,
    session: &session::SessionState,
    policy: orbit_protocol::SessionPolicy,
) -> Result<WorkspaceResources> {
    use orbit_protocol::Capability;
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
    let diagnostics = orbit_config::models::updater::refresh(&mut file_config, false)
        .await
        .diagnostics;
    let model_options = file_config.model_options()?;
    let selected_model = file_config.select_model(session.saved_model_profile().0)?;
    let agent_config = file_config.agent_config();
    let command_limits = file_config.command.clone();
    let web = file_config.web.clone();
    let mcp = if policy.disabled(Capability::Mcp) {
        Default::default()
    } else {
        file_config.mcp.clone()
    };
    let max_output_tokens = selected_model.max_output_tokens;
    let model_name = selected_model.qualified_name();
    let context_tokens = selected_model.context_tokens;
    let input_context_tokens = selected_model.input_context_tokens;
    let client = ModelClient::new(selected_model.config)?;
    let target_project = session.store.resolve_project(directory).await?;
    let enabled_plugins = session.store.project_plugins(&target_project.id).await?;
    let plugin_skill_roots = crate::marketplaces::enabled_skill_roots(&enabled_plugins);
    let mut skills = skills::Skills::discover_with_plugin_roots(
        directory,
        options.home.clone(),
        options.config_home.clone(),
        plugin_skill_roots.roots,
    );
    if policy.disabled(Capability::Skills) {
        skills = Default::default();
    }
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
        (!policy.read_only).then_some(approvals.clone()),
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
    let planning = crate::planning::Planning::new(
        session.store.clone(),
        session.id().into(),
        runtime.workspace.clone(),
    )
    .await?;
    if policy.disabled(Capability::Files) {
        planning.disable_file_tracking();
    }
    planning.install_policy(&mut tools);
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
    let user_input =
        crate::user_input::Questions::new(session.store.clone(), session.id().into()).await?;
    user_input.register(&mut tools, None);
    crate::policy::filter_tools(&mut tools, &policy.disabled.iter().copied().collect());
    if !policy.disabled(Capability::Subagents) {
        subagents::register(
            &mut tools,
            session.store.clone(),
            session.id().into(),
            config_path.clone(),
            options.auth_directory.clone(),
            instructions.clone(),
            file_config.subagents.clone(),
            (!policy.read_only).then_some(approvals),
            Some(user_input.clone()),
        )
        .await?;
    }
    if policy.disabled(Capability::Files) {
        tools.remove("verification_status");
    }

    if !policy.disabled(Capability::All) && !session.store.is_subagent(session.id()).await? {
        planning.register(&mut tools);
    }

    Ok(WorkspaceResources {
        mode_revision: 0,
        planning,
        mode: match session.store.tui_mode(session.id()).await?.as_str() {
            "ask" => orbit_protocol::Mode::Ask,
            "YOLO" => orbit_protocol::Mode::Yolo,
            _ => orbit_protocol::Mode::Auto,
        },
        options: options.clone(),
        policy,
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
        user_input,
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

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_protocol::{Capability, SessionPolicy};

    #[tokio::test]
    async fn service_policy_filters_tools_before_delegation_and_blocks_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("config.toml"),
            &TestConfig::new()
                .provider(
                    TestProvider::new("test")
                        .endpoint("http://127.0.0.1:1/v1")
                        .key("test")
                        .models([TestModel::new("mock")]),
                )
                .providers_toml()
                .unwrap(),
        )
        .unwrap();
        let options = WorkspaceOptions {
            model: Some("test::mock".into()),
            initial_directory: root.into(),
            config: root.join("config.toml"),
            auth_directory: root.join("auth"),
            database: root.join("sessions.db"),
            allow_mcp_start: false,
            home: None,
            config_home: None,
            instruction_dir: None,
            editor_command: "vi".into(),
        };
        let setup = prepare_service_with_policy(
            &options,
            None,
            SessionPolicy {
                read_only: true,
                disabled: vec![Capability::Web, Capability::Skills],
            },
        )
        .await
        .unwrap();
        let tools = &setup.resources.tools;
        assert!(tools.contains("read_file"));
        for name in [
            "write_file",
            "edit_file",
            "run_command",
            "git_push",
            "request_capability",
            "web_fetch",
            "web_search",
            "load_skill",
        ] {
            assert!(!tools.contains(name), "unexpected tool: {name}");
        }
        assert!(!setup.resources.command_limits.sandbox);
        drop(setup);
        let setup = prepare_service_with_policy(
            &options,
            None,
            SessionPolicy {
                read_only: false,
                disabled: vec![Capability::All],
            },
        )
        .await
        .unwrap();
        assert!(setup.resources.tools.definitions().is_empty());
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    #[tokio::test]
    async fn creation_requires_selection_and_restore_uses_saved_model() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("config.toml"),
            &TestConfig::new()
                .provider(
                    TestProvider::new("test")
                        .endpoint("http://127.0.0.1:1/v1")
                        .models([TestModel::new("first"), TestModel::new("chosen")]),
                )
                .providers_toml()
                .unwrap(),
        )
        .unwrap();
        let mut options = WorkspaceOptions {
            model: None,
            initial_directory: root.into(),
            config: root.join("config.toml"),
            auth_directory: root.join("auth"),
            database: root.join("sessions.db"),
            allow_mcp_start: false,
            home: None,
            config_home: None,
            instruction_dir: None,
            editor_command: "vi".into(),
        };
        assert!(prepare_service(&options, None).await.is_err());
        assert!(!options.database.exists());
        options.model = Some("test::chosen".into());
        let setup = prepare_service(&options, None).await.unwrap();
        assert_eq!(setup.resources.model_name, "test::chosen");
        let id = setup.session.id().to_owned();
        drop(setup);
        options.model = Some("test::first".into());
        let restored = prepare_service(&options, Some(id)).await.unwrap();
        assert_eq!(restored.resources.model_name, "test::chosen");
    }
}
