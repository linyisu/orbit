use anyhow::{Result, ensure};
use orbit_model::types::ChatMessage;
use orbit_protocol::{Inspection, Report, SessionAction};

use crate::PreparedWorkspace;

pub(super) async fn execute(
    setup: &mut PreparedWorkspace,
    action: SessionAction,
    running: bool,
) -> Result<Report> {
    match action {
        SessionAction::Inspect { section } => inspect(setup, section).await,
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
            let content = tokio::fs::read_to_string(&skill.path).await?;
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

async fn inspect(setup: &PreparedWorkspace, section: Inspection) -> Result<Report> {
    let session = &setup.session;
    let resources = &setup.resources;
    let budget = &resources.budget;
    Ok(match section {
        Inspection::Skills => report("Skills", budget.skills.render()),
        Inspection::Mcp => report(
            "MCP 状态",
            budget
                .mcp_statuses
                .iter()
                .map(|status| {
                    format!(
                        "{} · {} · {} 个工具",
                        status.name, status.detail, status.tool_count
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        Inspection::Diagnostics => report(
            "诊断记录",
            orbit_store::diagnostics::text(&session.store.diagnostics(session.id()).await?),
        ),
        Inspection::Changes => {
            let listing =
                super::changes::list(&session.store.workspace_changes(session.id()).await?);
            report(
                "文件改动",
                format!(
                    "{listing}{}",
                    crate::verification::changes_text(session).await
                ),
            )
        }
        Inspection::Diff { path } => {
            let changes = session.store.workspace_changes(session.id()).await?;
            let text = match path {
                Some(path) => super::changes::diff(&changes, &path),
                None => super::changes::list(&changes),
            };
            report("文件差异", text)
        }
        Inspection::Status => report(
            "会话状态",
            format!(
                "会话：{}\n工作目录：{}\n数据库：{}\n模型：{}\n执行：{}\n工具：{}\n{}",
                session.id(),
                session.snapshot.session.workspace,
                session.store.path().display(),
                resources.model_name,
                if resources.command_limits.sandbox {
                    "沙箱"
                } else {
                    "普通执行"
                },
                resources
                    .tools
                    .definitions()
                    .iter()
                    .map(|tool| tool.function.name.clone())
                    .collect::<Vec<_>>()
                    .join(" / "),
                crate::verification::summary(session).await
            ),
        ),
        Inspection::Context => {
            let mut history = session.store.side_question_context(session.id()).await?;
            crate::instructions::inject(&mut history, &budget.instructions);
            budget.skills.inject(&mut history);
            crate::project_state::inject(
                &mut history,
                &session.store,
                &session.snapshot.session.project_id,
                session.id(),
            )
            .await?;
            let estimated =
                orbit_agent::context::estimate_tokens(&history, &resources.tools.definitions());
            let calibration = session.calibration(&resources.model_name);
            report(
                "上下文预算",
                format!(
                    "原始估算：{estimated} tokens\n校准估算：{} tokens\n输入窗口：{} · 输出上限：{} · 请求预算：{}\n自动整理：{}\n项目指令：{}\nSkills：{}",
                    calibration.estimate(estimated),
                    budget.input_context_tokens,
                    budget.max_output_tokens,
                    budget.context_tokens,
                    if budget.auto_compact {
                        "开启"
                    } else {
                        "关闭"
                    },
                    budget.instructions.source_summary(),
                    budget.skills.len()
                ),
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        std::fs::write(root.join("config.toml"), "default_provider = \"test\"\n[providers.test]\nbase_url = \"http://127.0.0.1:1/v1\"\napi_key = \"test\"\nauto_refresh_models = false\nmodels = [{ id = \"mock\" }]\n").unwrap();
        let options = crate::WorkspaceOptions {
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
        execute(
            &mut setup,
            SessionAction::Rename {
                title: "review".into(),
            },
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            setup.session.store.session(&id).await.unwrap().title,
            "review"
        );
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
