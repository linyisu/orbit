#[cfg(test)]
use orbit_config::test_support::{TestConfig, TestProvider};
mod inspection;
mod project;

use anyhow::{Context, Result, ensure};
use orbit_model::types::ChatMessage;
use orbit_protocol::{Report, SessionAction};

use crate::PreparedWorkspace;

pub(super) async fn execute(
    setup: &mut PreparedWorkspace,
    action: SessionAction,
    running: bool,
) -> Result<Report> {
    match action {
        SessionAction::ChangeDirectory { path } => {
            ensure!(!running, "请先结束当前响应再移动会话");
            let path = if path.starts_with(['\'', '"']) {
                let words = shlex::split(&path).context("路径引号未闭合")?;
                ensure!(words.len() == 1, "请提供一个目录路径");
                words.into_iter().next().unwrap()
            } else {
                path
            };
            let root = std::path::Path::new(&setup.session.snapshot.session.workspace);
            let path = if path == "~" || path.starts_with("~/") {
                setup
                    .resources
                    .options
                    .home
                    .as_ref()
                    .context("目标环境没有 HOME")?
                    .join(path.trim_start_matches('~').trim_start_matches('/'))
            } else {
                root.join(path)
            };
            let directory = path.canonicalize().context("目标目录不存在")?;
            ensure!(directory.is_dir(), "目标路径必须是目录");
            let mut options = setup.resources.options.clone();
            options.initial_directory = directory.clone();
            let mut resources = super::setup::prepare_resources(
                &options,
                &directory,
                &setup.session,
                setup.resources.policy.clone(),
            )
            .await?;
            super::profile::apply(
                &mut resources,
                &setup.resources.model_name,
                setup.resources.model.config().reasoning_effort,
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
                && setup.resources.planning.snapshot().await.mode
                    == orbit_protocol::plan::WorkMode::Plan
            {
                resources.planning.enter().await?;
            }
            setup.resources = resources;
            Ok(report("工作目录", directory.to_string_lossy().into_owned()))
        }
        SessionAction::RefreshSkills => {
            let options = &setup.resources.options;
            let enabled = setup
                .session
                .store
                .project_plugins(&setup.session.snapshot.session.project_id)
                .await?;
            let roots = crate::marketplaces::enabled_skill_roots(&enabled);
            let skills = if setup
                .resources
                .policy
                .disabled(orbit_protocol::Capability::Skills)
            {
                Default::default()
            } else {
                crate::skills::Skills::discover_with_plugin_roots(
                    std::path::Path::new(&setup.session.snapshot.session.workspace),
                    options.home.clone(),
                    options.config_home.clone(),
                    roots.roots,
                )
            };
            skills.register_tool(&mut setup.resources.tools);
            setup.resources.budget.skills = skills;
            setup.resources.budget.plugin_skill_diagnostics = roots.diagnostics;
            Ok(report("Skills", setup.resources.budget.skills.render()))
        }
        SessionAction::Task { task } => {
            ensure!(
                !setup
                    .resources
                    .policy
                    .disabled(orbit_protocol::Capability::ProjectState),
                "任务功能已禁用"
            );
            let project = if matches!(&task, orbit_protocol::project::TaskAction::Add { .. }) {
                Some(
                    setup
                        .session
                        .store
                        .resolve_project(&setup.session.snapshot.session.workspace)
                        .await?,
                )
            } else {
                setup
                    .session
                    .store
                    .project_at(&setup.session.snapshot.session.workspace)
                    .await?
            };
            setup.session.snapshot.session.project_id = project.map(|p| p.id).unwrap_or_default();
            let mut history = Vec::new();
            let result =
                crate::project_state::actions::task(&setup.session, &mut history, task).await?;
            Ok(project::task_report(result))
        }
        SessionAction::Memory { memory } => {
            ensure!(
                !setup
                    .resources
                    .policy
                    .disabled(orbit_protocol::Capability::ProjectState),
                "记忆功能已禁用"
            );
            if let orbit_protocol::project::MemoryAction::Add { content, .. } = &memory {
                ensure!(!content.trim().is_empty(), "记忆内容不能为空。");
                ensure!(
                    setup.session.is_persisted(),
                    "请先开始会话，再添加会话记忆。"
                );
            }
            let mut history = Vec::new();
            let result =
                crate::project_state::actions::memory(&setup.session, &mut history, memory).await?;
            Ok(project::memory_report(result))
        }
        SessionAction::Inspect { section } => inspection::inspect(setup, section).await,
        SessionAction::Rename { title } => {
            ensure!(
                !title.trim().is_empty() && title.chars().count() <= 120,
                "会话标题应为 1–120 个字符"
            );
            setup.session.rename(title.clone()).await?;
            Ok(report("会话标题", title))
        }
        SessionAction::LoadSkill { name } => {
            ensure!(!running, "请在当前任务结束后载入 Skill");
            let skill = setup
                .resources
                .budget
                .skills
                .get(&name)
                .ok_or_else(|| anyhow::anyhow!("未找到 Skill {name:?}"))?;
            let content = crate::skills::read_skill(&skill.path).map_err(anyhow::Error::msg)?;
            let command = skill.command.clone();
            let message = ChatMessage::user(format!(
                "请在后续工作中使用已载入的 Skill /{}：\n{content}",
                skill.command
            ));
            setup.session.append_context(message).await?;
            Ok(report("Skill", format!("已载入 /{command}。")))
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
    async fn actions_use_target_state_and_skill_context_survives_reopening() {
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
        TestConfig::new()
            .provider(TestProvider::new("test").key("test").models(["mock"]))
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
            "命令原始输出：无临时日志",
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
        assert!(setup.session.store.session(&id).await.is_err());
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
        assert!(setup.session.store.session(&id).await.is_err());
        setup
            .session
            .begin("use this skill".into(), "test::mock".into())
            .await
            .unwrap();
        setup.session.finish("cancelled", None).await.unwrap();
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
