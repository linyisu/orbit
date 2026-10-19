#[cfg(test)]
use orbit_config::test_support::{TestConfig, TestProvider};
mod inspection;
mod project;

use anyhow::{Context, Result, ensure};
use orbit_model::types::ChatMessage;
use orbit_protocol::{Report, SessionAction};

use crate::PreparedWorkspace;

fn directory(
    root: &std::path::Path,
    home: Option<&std::path::Path>,
    path: String,
) -> Result<std::path::PathBuf> {
    let path = if path.starts_with(['\'', '"']) {
        #[cfg(windows)]
        {
            let quote = path.as_bytes()[0];
            ensure!(
                path.len() >= 2 && path.as_bytes().last() == Some(&quote),
                "路径引号未闭合"
            );
            path[1..path.len() - 1].to_owned()
        }
        #[cfg(not(windows))]
        {
            let words = shlex::split(&path).context("路径引号未闭合")?;
            ensure!(words.len() == 1, "请提供一个目录路径");
            words.into_iter().next().unwrap()
        }
    } else {
        path
    };
    let path = if path == "~" || path.starts_with("~/") {
        home.context("目标环境没有用户目录")?
            .join(
                path.trim_start_matches('~')
                    .trim_start_matches('/'),
            )
    } else {
        root.join(path)
    };
    let directory = path
        .canonicalize()
        .context("目标目录不存在")?;
    ensure!(directory.is_dir(), "目标路径必须是目录");
    Ok(directory)
}

pub(super) async fn browse(
    browsing: &mut super::session::browsing::Browsing,
    options: &crate::WorkspaceOptions,
    action: SessionAction,
) -> Result<super::protocol::Response> {
    use super::protocol::Response;
    let config = browsing.config(options)?;
    let policy = orbit_protocol::SessionPolicy {
        disabled: config.disabled_capabilities,
        ..Default::default()
    };
    let result = match action {
        SessionAction::ChangeDirectory { path } => {
            let directory = directory(&browsing.directory, browsing.home.as_deref(), path)?;
            let approval = orbit_runtime::policy::ApprovalPolicy::default();
            approval.set_mode(super::setup::approval_mode(browsing.mode));
            let workspace = crate::runtime::configured_workspace(
                &directory,
                Some(config.permissions),
                config.command.network,
                approval,
            )?;
            browsing
                .session
                .move_to(workspace, config.command)
                .await?;
            browsing.directory = directory;
            return Ok(Response::Profile(Box::new(browsing.info())));
        }
        SessionAction::Task { task } => project::task_report(
            super::tasks::execute_session(&mut browsing.session, &policy, task).await?,
        ),
        SessionAction::Memory { memory } => {
            self::memory(&browsing.session, &policy, memory).await?
        }
        SessionAction::Rename { title } => {
            ensure!(
                orbit_protocol::admin::valid_title(&title),
                "会话标题应为 1–120 个字符"
            );
            browsing
                .session
                .rename(title.clone())
                .await?;
            return Ok(Response::Profile(Box::new(browsing.info())));
        }
        SessionAction::RefreshSkills => {
            let skills = if policy.disabled(orbit_protocol::Capability::Skills) {
                Default::default()
            } else {
                super::library::session_skills(&browsing.session).await?
            };
            report("Skills", skills.render())
        }
        SessionAction::Inspect {
            section: orbit_protocol::Inspection::Context,
        } => report(
            "上下文预算",
            "尚未准备模型请求，执行预算未知；历史会话可以正常查看。".into(),
        ),
        SessionAction::Inspect { section } => {
            if let Some(report) = inspection::stored(&browsing.session, &section).await? {
                report
            } else {
                match section {
                    orbit_protocol::Inspection::Skills => {
                        ensure!(
                            !policy.disabled(orbit_protocol::Capability::Skills),
                            "技能功能已禁用"
                        );
                        report(
                            "Skills",
                            super::library::session_skills(&browsing.session)
                                .await?
                                .render(),
                        )
                    }
                    orbit_protocol::Inspection::Status => report(
                        "会话状态",
                        format!(
                            "会话：{}\n项目目录：{}\n执行尚未开始；发送请求时校验模型配置。",
                            browsing.session.id(),
                            browsing
                                .session
                                .snapshot
                                .session
                                .workspace,
                        ),
                    ),
                    orbit_protocol::Inspection::Mcp => {
                        let servers = super::resources::pending_mcp(&config.mcp, &policy)?;
                        report(
                            "MCP 状态",
                            if servers.is_empty() {
                                "未配置 MCP server。".into()
                            } else {
                                servers
                                    .iter()
                                    .map(|server| {
                                        format!(
                                            "{} · {} · {} 个工具",
                                            server.name,
                                            server.detail,
                                            server.tools.len()
                                        )
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            },
                        )
                    }
                    orbit_protocol::Inspection::ExportPatch => {
                        let (workspace, policy) =
                            super::files::browsing_access(browsing, options).await?;
                        ensure!(
                            !policy.disabled(orbit_protocol::Capability::Git),
                            "Git 功能已禁用"
                        );
                        report(
                            "Git patch",
                            tokio::task::spawn_blocking(move || {
                                orbit_tools::git_inspect::working_tree_patch(workspace.root())
                            })
                            .await??,
                        )
                    }
                    orbit_protocol::Inspection::Context
                    | orbit_protocol::Inspection::Diagnostics
                    | orbit_protocol::Inspection::Changes
                    | orbit_protocol::Inspection::Diff { .. } => unreachable!(),
                }
            }
        }
        SessionAction::LoadSkill { name } => {
            ensure!(
                !policy.disabled(orbit_protocol::Capability::Skills),
                "技能功能已禁用"
            );
            let skills = super::library::session_skills(&browsing.session).await?;
            load_skill(&mut browsing.session, &skills, name, false).await?
        }
    };
    Ok(Response::Report(result))
}

async fn memory(
    session: &crate::session::SessionState,
    policy: &orbit_protocol::SessionPolicy,
    action: orbit_protocol::project::MemoryAction,
) -> Result<Report> {
    ensure!(
        !policy.disabled(orbit_protocol::Capability::ProjectState),
        "记忆功能已禁用"
    );
    if let orbit_protocol::project::MemoryAction::Add { content, .. } = &action {
        ensure!(!content.trim().is_empty(), "记忆内容不能为空。");
        ensure!(session.is_persisted(), "请先开始会话，再添加会话记忆。");
    }
    let result = crate::project_state::actions::memory(session, &mut Vec::new(), action).await?;
    Ok(project::memory_report(result))
}

async fn load_skill(
    session: &mut crate::session::SessionState,
    skills: &crate::skills::Skills,
    name: String,
    running: bool,
) -> Result<Report> {
    ensure!(!running, "请在当前任务结束后载入 Skill");
    let skill = skills
        .get(&name)
        .ok_or_else(|| anyhow::anyhow!("未找到 Skill {name:?}"))?;
    let content = crate::skills::read_skill(&skill.path).map_err(anyhow::Error::msg)?;
    let command = skill.command.clone();
    let message = ChatMessage::user(format!(
        "请在后续工作中使用已载入的 Skill /{}：\n{content}",
        skill.command
    ));
    session.append_context(message).await?;
    Ok(report("Skill", format!("已载入 /{command}。")))
}

pub(super) async fn execute(
    setup: &mut PreparedWorkspace,
    action: SessionAction,
    running: bool,
) -> Result<Report> {
    match action {
        SessionAction::ChangeDirectory { path } => {
            ensure!(!running, "请先结束当前响应再移动会话");
            let root = std::path::Path::new(&setup.session.snapshot.session.workspace);
            let directory = directory(root, setup.resources.options.home.as_deref(), path)?;
            let mut options = setup.resources.options.clone();
            options.initial_directory = directory.clone();
            let mut resources = super::setup::prepare_resources(
                &options,
                &directory,
                &setup.session,
                setup.resources.policy.clone(),
                &setup.resources.disabled_tools,
            )
            .await?;
            super::profile::apply(
                &mut resources,
                &setup.resources.model_name,
                setup
                    .resources
                    .model
                    .config()
                    .reasoning_effort,
            )?;
            setup
                .session
                .move_to(
                    resources.workspace.clone(),
                    resources.command_limits.clone(),
                )
                .await?;
            resources.mode = setup.resources.mode;
            resources.mode_revision = setup.resources.mode_revision;
            if !setup.session.is_persisted()
                && setup
                    .resources
                    .planning
                    .snapshot()
                    .await
                    .mode
                    == orbit_protocol::plan::WorkMode::Plan
            {
                resources.planning.enter().await?;
            }
            setup.resources = resources;
            Ok(report("工作目录", directory.to_string_lossy().into_owned()))
        }
        SessionAction::RefreshSkills => {
            let skills = if setup
                .resources
                .policy
                .disabled(orbit_protocol::Capability::Skills)
            {
                Default::default()
            } else {
                super::library::session_skills(&setup.session).await?
            };
            skills.register_tool(&mut setup.resources.tools);
            super::setup::filter_configured_tools(
                &mut setup.resources.tools,
                &setup.resources.policy,
                &setup.resources.disabled_tools,
            );
            setup.resources.budget.skills = skills;
            setup
                .resources
                .budget
                .plugin_skill_diagnostics
                .clear();
            Ok(report("Skills", setup.resources.budget.skills.render()))
        }
        SessionAction::Task { task } => {
            let result = super::tasks::execute(setup, task).await?;
            Ok(project::task_report(result))
        }
        SessionAction::Memory { memory } => {
            self::memory(&setup.session, &setup.resources.policy, memory).await
        }
        SessionAction::Inspect { section } => inspection::inspect(setup, section).await,
        SessionAction::Rename { title } => {
            ensure!(
                orbit_protocol::admin::valid_title(&title),
                "会话标题应为 1–120 个字符"
            );
            setup
                .session
                .rename(title.clone())
                .await?;
            Ok(report("会话标题", title))
        }
        SessionAction::LoadSkill { name } => {
            load_skill(
                &mut setup.session,
                &setup.resources.budget.skills,
                name,
                running,
            )
            .await
        }
    }
}

fn report(title: &str, text: String) -> Report {
    Report {
        title: title.into(),
        text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_protocol::Inspection;

    #[tokio::test]
    async fn skill_refresh_does_not_apply_legacy_project_plugins_to_a_draft() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::create_dir(root.join(".git")).unwrap();
        TestConfig::new()
            .provider(
                TestProvider::new("test")
                    .key("test")
                    .models(["mock"]),
            )
            .write(root.join("config.toml"))
            .unwrap();
        let options = crate::WorkspaceOptions {
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
        let mut setup = super::super::setup::prepare_service(&options, None)
            .await
            .unwrap();
        assert!(!setup.session.is_persisted());
        assert!(
            setup
                .resources
                .budget
                .plugin_skill_diagnostics
                .is_empty()
        );
        let project = setup
            .session
            .store
            .resolve_project(root)
            .await
            .unwrap();
        setup
            .session
            .store
            .enable_project_plugin(orbit_protocol::project::ProjectPlugin {
                project_id: project.id,
                marketplace_source: format!("missing-market-{}", setup.session.id()),
                plugin_name: "example".into(),
                package_source: "plugins/example".into(),
                version: None,
                marketplace_revision: None,
                enabled_at: 0,
            })
            .await
            .unwrap();
        execute(&mut setup, SessionAction::RefreshSkills, false)
            .await
            .unwrap();
        // Legacy project enablement cannot opt a new session into a global package.
        assert_eq!(
            setup
                .resources
                .budget
                .plugin_skill_diagnostics
                .len(),
            0
        );
        assert!(!setup.session.is_persisted());
    }

    #[tokio::test]
    async fn actions_use_target_state_and_skill_context_survives_reopening() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::create_dir(root.join(".git")).unwrap();
        TestConfig::new()
            .provider(
                TestProvider::new("test")
                    .key("test")
                    .models(["mock"]),
            )
            .write(root.join("config.toml"))
            .unwrap();
        let options = crate::WorkspaceOptions {
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
        let mut setup = super::super::setup::prepare_service(&options, None)
            .await
            .unwrap();
        let id = setup.session.id().to_owned();
        let status = execute(
            &mut setup,
            SessionAction::Inspect {
                section: Inspection::Status,
            },
            false,
        )
        .await
        .unwrap();
        assert!(status.text.contains("普通执行"));
        for field in [
            "命令原始输出：当前会话尚无保留日志",
            "Git 基线：",
            "模型流空闲",
            "审批：",
            "上下文：",
        ] {
            assert!(status.text.contains(field), "{field}: {}", status.text);
        }
        let context = execute(
            &mut setup,
            SessionAction::Inspect {
                section: Inspection::Context,
            },
            false,
        )
        .await
        .unwrap();
        for field in [
            "系统提示：",
            "历史消息：",
            "工具定义：",
            "任务与会话记忆：",
            "服务端样本",
            "系数",
        ] {
            assert!(context.text.contains(field), "{field}: {}", context.text);
        }
        let mcp = execute(
            &mut setup,
            SessionAction::Inspect {
                section: Inspection::Mcp,
            },
            false,
        )
        .await
        .unwrap();
        assert_eq!(mcp.text, "未配置 MCP server。");
        execute(
            &mut setup,
            SessionAction::Rename {
                title: "review".into(),
            },
            false,
        )
        .await
        .unwrap();
        assert_eq!(setup.session.snapshot.session.title, "review");
        assert!(
            setup
                .session
                .store
                .session(&id)
                .await
                .is_err()
        );
        super::super::resources::execute(
            &mut setup,
            orbit_protocol::resources::Action::ApplyExtension {
                bundle: super::super::library::test_bundle(
                    "example",
                    "---\ndescription: Session fixture\n---\nKeep identifiers stable.",
                )
                .into(),
            },
            false,
        )
        .await
        .unwrap();

        assert!(
            execute(
                &mut setup,
                SessionAction::LoadSkill {
                    name: "example".into()
                },
                true
            )
            .await
            .is_err()
        );
        execute(
            &mut setup,
            SessionAction::LoadSkill {
                name: "example".into(),
            },
            false,
        )
        .await
        .unwrap();
        assert!(
            setup
                .session
                .store
                .session(&id)
                .await
                .is_ok()
        );
        setup
            .session
            .begin("use this skill".into(), "test::mock".into())
            .await
            .unwrap();
        setup
            .session
            .finish(orbit_store::types::RunCompletion::Cancelled)
            .await
            .unwrap();
        drop(setup);
        let reopened = super::super::setup::prepare_service(&options, Some(id))
            .await
            .unwrap();
        assert!(
            serde_json::to_string(&reopened.session.snapshot.history)
                .unwrap()
                .contains("Keep identifiers stable")
        );
    }
}
