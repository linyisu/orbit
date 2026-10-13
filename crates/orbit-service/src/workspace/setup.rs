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

impl PreparedWorkspace {
    /// Promote a draft once, before any run/receipt/tool can write child records.
    pub(crate) async fn start_session(&mut self) -> Result<()> {
        self.session.persist().await?;
        self.session
            .store
            .set_tui_mode(
                self.session.id(),
                match self.resources.mode {
                    orbit_protocol::Mode::Ask => "ask",
                    orbit_protocol::Mode::Auto => "auto",
                    orbit_protocol::Mode::Yolo => "YOLO",
                },
            )
            .await?;
        self.resources.planning.persist().await?;
        Ok(())
    }
}

pub struct WorkspaceResources {
    pub(super) skill_import: Option<super::skill_import::Scan>,
    pub(super) mcp_import: Option<super::mcp_import::Scan>,
    pub options: WorkspaceOptions,
    pub mode: orbit_protocol::Mode,
    pub mode_revision: u64,
    pub policy: orbit_protocol::SessionPolicy,
    /// The startup tool ceiling survives refreshes for this session.
    pub disabled_tools: Vec<String>,
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
    let mut resources = prepare_resources(options, directory, &session, policy, &[]).await?;
    if session.saved_model_profile().0 != resources.model_name {
        // Repair only the model profile after resources have been prepared;
        // opening history must not depend on a provider that was removed.
        session
            .set_model_profile(
                resources.model_name.clone(),
                resources.model.config().reasoning_effort,
            )
            .await?;
    }
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
    prepare_resources(options, directory, session, Default::default(), &[]).await
}

pub(super) async fn prepare_resources(
    options: &WorkspaceOptions,
    directory: &std::path::Path,
    session: &session::SessionState,
    policy: orbit_protocol::SessionPolicy,
    inherited_disabled_tools: &[String],
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
    file_config
        .disabled_tools
        .extend(inherited_disabled_tools.iter().cloned());
    file_config.disabled_tools.sort_unstable();
    file_config.disabled_tools.dedup();
    // Persisted ceilings only narrow the caller's policy; they never replace it.
    let mut policy = policy;
    policy
        .disabled
        .extend(file_config.disabled_capabilities.iter().copied());
    policy.disabled.sort_unstable();
    policy.disabled.dedup();
    let diagnostics = orbit_config::models::updater::refresh(&mut file_config, false)
        .await
        .diagnostics;
    let model_options = file_config.model_options()?;
    let selected_model = restored_model(
        &file_config,
        session.saved_model_profile().0,
        options.model.as_deref(),
    )?;
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
    let enabled_plugins = match session.store.project_at(directory).await? {
        Some(project) => session.store.project_plugins(&project.id).await?,
        None => Vec::new(),
    };
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
    if !session.is_persisted() {
        planning.defer_persistence();
    }
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
    // Delegated agents capture this allowlist at registration.
    filter_configured_tools(&mut tools, &policy, &file_config.disabled_tools);
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
    // Apply ceilings after every group, including subagents and planning, so
    // a later registration cannot accidentally reintroduce a hidden tool.
    filter_configured_tools(&mut tools, &policy, &file_config.disabled_tools);

    Ok(WorkspaceResources {
        skill_import: None,
        mcp_import: None,
        mode_revision: 0,
        planning,
        mode: match session.store.tui_mode(session.id()).await?.as_str() {
            "ask" => orbit_protocol::Mode::Ask,
            "YOLO" => orbit_protocol::Mode::Yolo,
            _ => orbit_protocol::Mode::Auto,
        },
        options: options.clone(),
        policy,
        disabled_tools: file_config.disabled_tools.clone(),
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

pub(super) fn filter_configured_tools(
    tools: &mut orbit_agent::tool::ToolRegistry,
    policy: &orbit_protocol::SessionPolicy,
    disabled_tools: &[String],
) {
    crate::policy::filter_tools(tools, &policy.disabled.iter().copied().collect());
    for name in disabled_tools {
        tools.remove(name);
    }
}

fn restored_model(
    config: &FileConfig,
    saved: &str,
    preferred: Option<&str>,
) -> Result<ModelOption> {
    if let Ok(model) = config.select_model(saved) {
        return Ok(model);
    }
    if let Some(model) = preferred.and_then(|name| config.select_model(name).ok()) {
        return Ok(model);
    }
    config
        .model_options()?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("没有可用模型，请先在设置中添加模型连接"))
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
    async fn refreshed_skill_and_source_tools_keep_the_startup_ceiling() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::create_dir(root.join(".git")).unwrap();
        let skill = root.join(".orbit/skills/example");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: example\ndescription: Example skill\n---\nKeep identifiers stable.\n",
        )
        .unwrap();
        let mut configuration =
            TestConfig::new().provider(TestProvider::new("test").models([TestModel::new("mock")]));
        configuration.config.disabled_capabilities = vec![Capability::Files];
        configuration.config.disabled_tools = vec!["load_skill".into()];
        configuration.write(root.join("config.toml")).unwrap();
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
        let mut setup = prepare_service(&options, None).await.unwrap();
        assert!(setup.resources.budget.skills.get("example").is_some());
        for name in ["code_symbols", "load_skill"] {
            assert!(!setup.resources.tools.contains(name));
            assert!(matches!(
                setup
                    .resources
                    .tools
                    .execute(
                        orbit_model::types::ToolCall::function(
                            "ceiling",
                            name,
                            serde_json::json!({"query":"example","name":"example"})
                        ),
                        tokio_util::sync::CancellationToken::new(),
                    )
                    .await,
                Err(orbit_agent::tool::ToolError::UnknownTool(_))
            ));
        }
        // Changes on disk affect later sessions. Refreshing the current skill
        // catalogue (also used after plugin updates) must keep its startup ceiling.
        configuration.config.disabled_capabilities.clear();
        configuration.config.disabled_tools.clear();
        configuration.write(&options.config).unwrap();
        super::super::actions::execute(
            &mut setup,
            orbit_protocol::SessionAction::RefreshSkills,
            false,
        )
        .await
        .unwrap();
        let snapshot = super::super::resources::execute(
            &mut setup,
            orbit_protocol::resources::Action::Skills,
            false,
        )
        .await
        .unwrap();
        assert!(
            matches!(snapshot, orbit_protocol::resources::Snapshot::Skills(skills) if skills.iter().any(|skill| skill.name == "example"))
        );
        for name in ["code_symbols", "load_skill"] {
            assert!(
                !setup.resources.tools.contains(name),
                "refresh reintroduced {name}"
            );
        }
        assert_eq!(setup.resources.disabled_tools, ["load_skill"]);
        let moved = root.join("moved");
        std::fs::create_dir(&moved).unwrap();
        super::super::actions::execute(
            &mut setup,
            orbit_protocol::SessionAction::ChangeDirectory {
                path: moved.display().to_string(),
            },
            false,
        )
        .await
        .unwrap();
        assert_eq!(setup.resources.disabled_tools, ["load_skill"]);
        assert!(!setup.resources.tools.contains("load_skill"));
        assert!(!setup.resources.tools.contains("code_symbols"));
        drop(setup);
        let fresh = prepare_service(&options, None).await.unwrap();
        assert!(fresh.resources.tools.contains("code_symbols"));
        assert!(fresh.resources.tools.contains("load_skill"));
    }

    #[tokio::test]
    async fn persisted_ceilings_narrow_callers_and_filter_late_registered_tools() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let mut configuration =
            TestConfig::new().provider(TestProvider::new("test").models([TestModel::new("mock")]));
        configuration.config.max_turns = 12;
        configuration.config.max_tool_calls = 99;
        configuration.config.disabled_capabilities = vec![Capability::Web, Capability::Mcp];
        configuration.config.disabled_tools =
            vec!["read_file".into(), "spawn_agent".into(), "task_list".into()];
        configuration.write(root.join("config.toml")).unwrap();
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
                disabled: vec![Capability::Skills],
            },
        )
        .await
        .unwrap();
        assert!(setup.resources.policy.read_only);
        for capability in [Capability::Web, Capability::Mcp, Capability::Skills] {
            assert!(setup.resources.policy.disabled(capability));
        }
        assert_eq!(setup.resources.budget.max_turns, 12);
        assert_eq!(setup.resources.budget.max_tool_calls, 99);
        assert!(setup.resources.tools.contains("search_text"));
        for name in [
            "web_fetch",
            "load_skill",
            "read_file",
            "spawn_agent",
            "task_list",
            "write_file",
        ] {
            assert!(
                !setup.resources.tools.contains(name),
                "unexpected tool {name}"
            );
        }
        drop(setup);
        configuration.config.disabled_capabilities = vec![Capability::All];
        configuration.config.disabled_tools.clear();
        configuration.write(root.join("config.toml")).unwrap();
        let setup = prepare_service_with_policy(&options, None, SessionPolicy::default())
            .await
            .unwrap();
        assert!(setup.resources.tools.definitions().is_empty());
        assert!(!setup.resources.policy.read_only);
    }

    #[tokio::test]
    async fn service_policy_filters_tools_before_delegation_and_blocks_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("config.toml"),
            TestConfig::new()
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
            TestConfig::new()
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
        let mut setup = prepare_service(&options, None).await.unwrap();
        assert!(setup.session.store.list().await.unwrap().is_empty());
        assert_eq!(setup.resources.model_name, "test::chosen");
        setup
            .session
            .begin("question".into(), "test::chosen".into())
            .await
            .unwrap();
        setup
            .session
            .finish(
                "completed",
                Some(vec![
                    orbit_model::types::ChatMessage::user("question"),
                    orbit_model::types::ChatMessage::assistant("answer"),
                ]),
            )
            .await
            .unwrap();
        let id = setup.session.id().to_owned();
        drop(setup);
        options.model = Some("test::first".into());
        let restored = prepare_service(&options, Some(id.clone())).await.unwrap();
        assert_eq!(restored.resources.model_name, "test::chosen");
        drop(restored);

        // Removing a provider/model must not strand an otherwise valid history.
        std::fs::write(
            &options.config,
            TestConfig::new()
                .provider(
                    TestProvider::new("test")
                        .endpoint("http://127.0.0.1:1/v1")
                        .models([TestModel::new("first"), TestModel::new("preferred")]),
                )
                .providers_toml()
                .unwrap(),
        )
        .unwrap();
        options.model = Some("test::preferred".into());
        let restored = prepare_service(&options, Some(id.clone())).await.unwrap();
        assert_eq!(restored.resources.model_name, "test::preferred");
        assert_eq!(restored.session.id(), id);
        assert_eq!(restored.session.saved_model_profile().0, "test::preferred");
        assert!(!restored.session.snapshot.history.is_empty());
        drop(restored);
        options.model = Some("test::first".into());
        let restored = prepare_service(&options, Some(id)).await.unwrap();
        assert_eq!(
            restored.resources.model_name, "test::preferred",
            "the repaired choice must persist"
        );
        let config = FileConfig::load(&options.config, &options.auth_directory).unwrap();
        assert_eq!(
            restored_model(&config, "gone::model", Some("also::gone"))
                .unwrap()
                .qualified_name(),
            "test::first"
        );
    }
}
