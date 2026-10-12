//! Settings operations execute on the owning session, including remote workspaces.
use crate::{PreparedWorkspace, skills::SkillSource};
use anyhow::{Context, Result, ensure};
use orbit_protocol::{
    Capability, SessionAction,
    resources::{Action, McpServer, McpTool, Skill, Snapshot},
};
use std::{fs, io::Write, path::Path};

pub(super) async fn execute(
    setup: &mut PreparedWorkspace,
    action: Action,
    busy: bool,
) -> Result<Snapshot> {
    if matches!(action, Action::Mcp) {
        ensure!(
            !setup.resources.policy.disabled(Capability::Mcp),
            "MCP 工具已禁用"
        );
        let visible = setup
            .resources
            .tools
            .definitions()
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<std::collections::HashSet<_>>();
        return Ok(Snapshot::Mcp(
            setup
                .resources
                .budget
                .mcp_statuses
                .iter()
                .map(|server| McpServer {
                    name: server.name.clone(),
                    connected: server.connected,
                    detail: server.detail.clone(),
                    tools: server
                        .tools
                        .iter()
                        .filter(|tool| visible.contains(&tool.name))
                        .map(|tool| McpTool {
                            name: tool.name.clone(),
                            description: tool.description.clone(),
                        })
                        .collect(),
                })
                .collect(),
        ));
    }
    if matches!(action, Action::Memories | Action::Memory(_)) {
        ensure!(
            !setup.resources.policy.disabled(Capability::ProjectState),
            "记忆功能已禁用"
        );
        if let Action::Memory(memory) = action {
            super::actions::execute(setup, SessionAction::Memory { memory }, busy).await?;
        }
        return Ok(Snapshot::Memories(
            setup
                .session
                .store
                .session_memories(setup.session.id(), false)
                .await?,
        ));
    }
    ensure!(
        !setup.resources.policy.disabled(Capability::Skills),
        "技能功能已禁用"
    );
    super::actions::execute(setup, SessionAction::RefreshSkills, busy).await?;
    match action {
        Action::ReadSkill { name } => {
            let skill = setup
                .resources
                .budget
                .skills
                .get(&name)
                .context("技能不存在，请刷新列表")?;
            Ok(Snapshot::Skill {
                name,
                content: crate::skills::read_skill(&skill.path).map_err(anyhow::Error::msg)?,
                editable: editable(skill.source),
            })
        }
        Action::SaveSkill {
            name,
            original,
            content,
        } => {
            ensure!(!busy, "请等待当前任务结束后保存技能");
            let cwd = Path::new(&setup.session.snapshot.session.workspace);
            let path = if original.is_some() {
                let skill = setup
                    .resources
                    .budget
                    .skills
                    .get(&name)
                    .context("技能不存在，请刷新列表")?;
                ensure!(editable(skill.source), "导入的技能为只读，请新建项目技能");
                skill.path.clone()
            } else {
                ensure!(
                    setup.resources.budget.skills.get(&name).is_none(),
                    "技能名称已存在"
                );
                let root =
                    crate::instructions::project_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
                root.join(".orbit/skills").join(&name).join("SKILL.md")
            };
            // Disk IO is bounded (64 KiB), off the GUI thread and serialized by the session actor.
            save_skill(&path, &name, original.as_deref(), &content)?;
            super::actions::execute(setup, SessionAction::RefreshSkills, busy).await?;
            Ok(skills(setup))
        }
        Action::Skills => Ok(skills(setup)),
        _ => unreachable!(),
    }
}
fn editable(source: SkillSource) -> bool {
    matches!(source, SkillSource::OrbitUser | SkillSource::OrbitProject)
}
fn skills(setup: &PreparedWorkspace) -> Snapshot {
    Snapshot::Skills(
        setup
            .resources
            .budget
            .skills
            .completions()
            .into_iter()
            .filter_map(|(name, _)| {
                let skill = setup.resources.budget.skills.get(&name)?;
                Some(Skill {
                    name,
                    title: skill.title.clone(),
                    description: skill.description.clone(),
                    source: match skill.source {
                        SkillSource::OrbitUser => "个人技能",
                        SkillSource::OrbitProject => "项目技能",
                        SkillSource::ClaudeUser => "个人 Claude 技能",
                        SkillSource::ClaudeProject => "项目 Claude 技能",
                        SkillSource::MarketplacePlugin => "插件技能",
                    }
                    .into(),
                    editable: editable(skill.source),
                })
            })
            .collect(),
    )
}
fn save_skill(path: &Path, name: &str, original: Option<&str>, content: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "名称只能包含字母、数字、横线和下划线（最多 64 字符）"
    );
    ensure!(
        !content.trim().is_empty() && content.len() <= 64 * 1024,
        "技能内容不能为空，且不能超过 64 KiB"
    );
    for part in path.ancestors() {
        match fs::symlink_metadata(part) {
            Ok(meta) => ensure!(!meta.file_type().is_symlink(), "不能编辑符号链接中的技能"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let parent = path.parent().context("技能目录无效")?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    if let Ok(meta) = fs::metadata(path) {
        file.as_file().set_permissions(meta.permissions())?;
    }
    file.write_all(content.as_bytes())?;
    file.as_file().sync_all()?;
    if let Some(original) = original {
        ensure!(
            crate::skills::read_skill(path).map_err(anyhow::Error::msg)? == original,
            "技能已被其他程序修改，请重新打开后编辑"
        );
        file.persist(path)?;
    } else {
        file.persist_noclobber(path)
            .context("技能已存在，请换一个名称")?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn settings_round_trip_uses_the_target_workspace_and_session() {
        use orbit_config::test_support::{TestConfig, TestProvider};
        use orbit_protocol::project::{MemoryAction, MemoryCategory};
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join(".git")).unwrap();
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
        // The settings snapshot exposes registration results without starting
        // any new processes and obeys the same capability policy as tools.
        setup
            .resources
            .budget
            .mcp_statuses
            .push(orbit_tools::mcp::McpServerStatus {
                name: "blocked".into(),
                detail: "启动需显式授权".into(),
                tool_count: 0,
                connected: false,
                tools: Vec::new(),
            });
        let Snapshot::Mcp(servers) = execute(&mut setup, Action::Mcp, false).await.unwrap() else {
            panic!("MCP snapshot expected")
        };
        assert_eq!(servers[0].name, "blocked");
        assert!(!servers[0].connected);
        assert!(servers[0].tools.is_empty());
        setup.resources.policy.disabled.push(Capability::Mcp);
        assert!(execute(&mut setup, Action::Mcp, false).await.is_err());
        setup.resources.policy.disabled.clear();
        let save = || Action::SaveSkill {
            name: "review".into(),
            original: None,
            content: "---\ndescription: Review code\n---\nRead the diff carefully.\n".into(),
        };
        assert!(execute(&mut setup, save(), true).await.is_err());
        let Snapshot::Skills(skills) = execute(&mut setup, save(), false).await.unwrap() else {
            panic!("skills expected")
        };
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "review");
        assert!(root.join(".orbit/skills/review/SKILL.md").is_file());
        let Snapshot::Skill {
            content, editable, ..
        } = execute(
            &mut setup,
            Action::ReadSkill {
                name: "review".into(),
            },
            false,
        )
        .await
        .unwrap()
        else {
            panic!("skill expected")
        };
        assert!(editable);
        assert!(content.contains("Read the diff"));
        setup
            .session
            .begin("settings test".into(), "test::mock".into())
            .await
            .unwrap();
        execute(
            &mut setup,
            Action::Memory(MemoryAction::Add {
                category: MemoryCategory::Preference,
                content: "Use Chinese".into(),
            }),
            false,
        )
        .await
        .unwrap();
        let Snapshot::Memories(memories) =
            execute(&mut setup, Action::Memories, false).await.unwrap()
        else {
            panic!("memories expected")
        };
        assert_eq!(memories.len(), 1);
        assert_eq!(memories[0].session_id, setup.session.id());
        let Snapshot::Memories(memories) = execute(
            &mut setup,
            Action::Memory(MemoryAction::Forget { id: memories[0].id }),
            false,
        )
        .await
        .unwrap() else {
            panic!("memories expected")
        };
        assert!(memories.is_empty());
    }

    #[test]
    fn skill_edits_preserve_conflicts_and_reject_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("demo/SKILL.md");
        assert!(save_skill(&path, "../bad", None, "hello").is_err());
        save_skill(&path, "demo", None, "first").unwrap();
        assert!(save_skill(&path, "demo", None, "replace").is_err());
        assert!(save_skill(&path, "demo", Some("stale"), "replace").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "first");
        save_skill(&path, "demo", Some("first"), "second").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
    }
}
