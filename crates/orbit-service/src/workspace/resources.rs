//! Session memory, MCP state and explicitly applied extension snapshots.
mod import;
mod mcp;
pub(super) use import::Import;
pub(super) use mcp::{Reload, Startup};

use crate::{PreparedWorkspace, skills::SkillSource};
use anyhow::{Context, Result, ensure};
use orbit_protocol::{
    Capability, SessionAction,
    resources::{Action, McpServer, McpTool, Skill, Snapshot},
};

pub(super) async fn execute(
    setup: &mut PreparedWorkspace,
    action: Action,
    busy: bool,
) -> Result<Snapshot> {
    if matches!(action, Action::Mcp) {
        ensure!(
            !setup
                .resources
                .policy
                .disabled(Capability::Mcp),
            "MCP 工具已禁用"
        );
        return Ok(mcp::snapshot(setup));
    }
    if matches!(action, Action::Memories | Action::Memory(_)) {
        ensure!(
            !setup
                .resources
                .policy
                .disabled(Capability::ProjectState),
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
        !setup
            .resources
            .policy
            .disabled(Capability::Skills),
        "技能功能已禁用"
    );
    let changed = matches!(
        action,
        Action::ApplyExtension { .. } | Action::RemoveExtension { .. }
    );
    let snapshot = session_action(
        &mut setup.session,
        action,
        busy,
        Some((&setup.resources.planning, setup.resources.mode)),
    )
    .await?;
    if changed {
        let skills = super::library::session_skills(&setup.session).await?;
        install_skills(setup, skills);
    }
    Ok(snapshot)
}

pub(super) async fn browse(
    browsing: &mut super::session::browsing::Browsing,
    options: &crate::WorkspaceOptions,
    action: Action,
) -> Result<Snapshot> {
    let config = super::session::browsing::Browsing::configuration(options)?;
    let policy = orbit_protocol::SessionPolicy {
        disabled: config.disabled_capabilities,
        ..Default::default()
    };
    if matches!(action, Action::Mcp) {
        ensure!(!policy.disabled(Capability::Mcp), "MCP 工具已禁用");
        return Ok(Snapshot::Mcp(
            config
                .mcp
                .servers
                .iter()
                .map(|(name, server)| McpServer {
                    name: name.clone(),
                    connected: false,
                    detail: if server.enabled {
                        "尚未启动执行"
                    } else {
                        "已禁用"
                    }
                    .into(),
                    tools: Vec::new(),
                })
                .collect(),
        ));
    }
    if matches!(action, Action::Memories | Action::Memory(_)) {
        ensure!(!policy.disabled(Capability::ProjectState), "记忆功能已禁用");
        if let Action::Memory(memory) = action {
            super::actions::browse(browsing, options, SessionAction::Memory { memory }).await?;
        }
        return Ok(Snapshot::Memories(
            browsing
                .session
                .store
                .session_memories(browsing.session.id(), false)
                .await?,
        ));
    }
    ensure!(!policy.disabled(Capability::Skills), "技能功能已禁用");
    session_action(&mut browsing.session, action, false, None).await
}

async fn session_action(
    session: &mut crate::session::SessionState,
    action: Action,
    busy: bool,
    execution: Option<(&crate::planning::Planning, orbit_protocol::Mode)>,
) -> Result<Snapshot> {
    match action {
        Action::AppliedExtensions => super::library::applied(session).await,
        Action::ApplyExtension { bundle } => {
            ensure!(!busy, "请等待当前任务结束后应用扩展");
            super::library::package::validate(&bundle)?;
            let mut bundles = session
                .store
                .session_extensions(session.id())
                .await?;
            bundles.retain(|current| current.extension.key != bundle.extension.key);
            bundles.push((*bundle).clone());
            super::library::package::materialize(&bundles)?;
            if let Some((planning, mode)) = execution {
                super::setup::start_session(session, mode, planning).await?;
            } else {
                session.persist().await?;
            }
            session
                .store
                .apply_extension(session.id(), &bundle)
                .await?;
            Ok(Snapshot::Extensions(
                bundles
                    .into_iter()
                    .map(|bundle| bundle.extension)
                    .collect(),
            ))
        }
        Action::RemoveExtension { key } => {
            ensure!(!busy, "请等待当前任务结束后移除会话扩展");
            let mut bundles = session
                .store
                .session_extensions(session.id())
                .await?;
            bundles.retain(|bundle| bundle.extension.key != key);
            super::library::package::materialize(&bundles)?;
            session
                .store
                .remove_extension(session.id(), &key)
                .await?;
            Ok(Snapshot::Extensions(
                bundles
                    .into_iter()
                    .map(|bundle| bundle.extension)
                    .collect(),
            ))
        }
        Action::Skills => Ok(skills(&super::library::session_skills(session).await?)),
        Action::ReadSkill { name } => {
            let skills = super::library::session_skills(session).await?;
            let skill = skills
                .get(&name)
                .context("会话中未应用此技能")?;
            Ok(Snapshot::Skill {
                name,
                content: crate::skills::read_skill(&skill.path).map_err(anyhow::Error::msg)?,
                editable: false,
            })
        }
        _ => anyhow::bail!("技能管理已迁移至全局库，请更新客户端后重试"),
    }
}

/// Replace the next-turn catalogue without broadening the session tool policy.
fn install_skills(setup: &mut PreparedWorkspace, skills: crate::skills::Skills) {
    skills.register_tool(&mut setup.resources.tools);
    super::setup::filter_configured_tools(
        &mut setup.resources.tools,
        &setup.resources.policy,
        &setup.resources.disabled_tools,
    );
    setup.resources.budget.skills = skills;
}

fn skills(skills: &crate::skills::Skills) -> Snapshot {
    Snapshot::Skills(
        skills
            .completions()
            .into_iter()
            .filter_map(|(name, _)| {
                let skill = skills.get(&name)?;
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
                        SkillSource::AppliedSession => "当前会话已应用",
                    }
                    .into(),
                    editable: false,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[tokio::test]
    async fn memory_and_mcp_operations_remain_session_scoped() {
        use orbit_config::test_support::{TestConfig, TestProvider};
        use orbit_protocol::project::{MemoryAction, MemoryCategory};
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::create_dir(root.join(".git")).unwrap();
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
        let Snapshot::Mcp(servers) = execute(&mut setup, Action::Mcp, false)
            .await
            .unwrap()
        else {
            panic!("MCP snapshot expected")
        };
        assert_eq!(servers[0].name, "blocked");
        assert!(!servers[0].connected);
        assert!(servers[0].tools.is_empty());
        setup
            .resources
            .policy
            .disabled
            .push(Capability::Mcp);
        assert!(
            execute(&mut setup, Action::Mcp, false)
                .await
                .is_err()
        );
        setup.resources.policy.disabled.clear();
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
        let Snapshot::Memories(memories) = execute(&mut setup, Action::Memories, false)
            .await
            .unwrap()
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
}
