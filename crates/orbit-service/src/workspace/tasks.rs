//! Terminal reports and desktop snapshots use the same task rules and actor.
use crate::{PreparedWorkspace, project_state::actions::TaskResult};
use anyhow::{Result, ensure};
use orbit_protocol::{
    Capability,
    project::{TaskAction, TaskSnapshot},
};

pub(super) async fn execute(
    setup: &mut PreparedWorkspace,
    action: TaskAction,
) -> Result<TaskResult> {
    ensure!(
        !setup
            .resources
            .policy
            .disabled(Capability::ProjectState),
        "任务功能已禁用"
    );
    let workspace = &setup.session.snapshot.session.workspace;
    let project = if matches!(action, TaskAction::Add { .. } | TaskAction::Create { .. }) {
        Some(
            setup
                .session
                .store
                .resolve_project(workspace)
                .await?,
        )
    } else {
        setup
            .session
            .store
            .project_at(workspace)
            .await?
    };
    setup
        .session
        .snapshot
        .session_mut()
        .project_id = project
        .map(|p| p.id)
        .unwrap_or_default();
    crate::project_state::actions::task(&setup.session, &mut Vec::new(), action).await
}

pub(super) async fn snapshot(
    setup: &mut PreparedWorkspace,
    action: TaskAction,
) -> Result<TaskSnapshot> {
    match execute(setup, action).await? {
        TaskResult::List(tasks) => Ok(TaskSnapshot {
            tasks,
            changed: None,
        }),
        TaskResult::Added(task) | TaskResult::Updated(task) => {
            let tasks = setup
                .session
                .store
                .tasks(
                    &setup
                        .session
                        .snapshot
                        .session
                        .project_id,
                    true,
                )
                .await?;
            Ok(TaskSnapshot {
                tasks,
                changed: Some(task.id),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_config::test_support::{TestConfig, TestProvider};
    use orbit_protocol::project::TaskStatus;

    async fn tasks(
        setup: &mut PreparedWorkspace,
        action: TaskAction,
    ) -> Result<Vec<orbit_protocol::project::ProjectTask>> {
        Ok(snapshot(setup, action).await?.tasks)
    }

    fn create(title: &str, depends_on: Vec<i64>) -> TaskAction {
        TaskAction::Create {
            title: title.into(),
            details: "由桌面创建".into(),
            priority: 2,
            parent_id: None,
            depends_on,
        }
    }

    #[tokio::test]
    async fn desktop_tasks_keep_dependency_rules_and_terminal_reports() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
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
        assert!(
            tasks(
                &mut setup,
                TaskAction::List {
                    include_completed: true
                }
            )
            .await
            .unwrap()
            .is_empty()
        );
        let first = tasks(&mut setup, create("先完成基础", vec![]))
            .await
            .unwrap()
            .remove(0);
        let all = tasks(&mut setup, create("依赖基础的任务", vec![first.id]))
            .await
            .unwrap();
        let second = all
            .iter()
            .find(|task| task.id != first.id)
            .unwrap()
            .clone();
        assert_eq!(second.details, "由桌面创建");
        assert_eq!(second.blocked_by, [first.id]);
        assert!(
            tasks(
                &mut setup,
                TaskAction::SetStatus {
                    id: second.id,
                    status: TaskStatus::InProgress,
                }
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("unfinished dependencies")
        );
        assert!(
            tasks(
                &mut setup,
                TaskAction::Update {
                    id: first.id,
                    title: None,
                    details: None,
                    status: None,
                    priority: None,
                    depends_on: Some(vec![second.id]),
                }
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("cycle")
        );
        let all = tasks(
            &mut setup,
            TaskAction::SetStatus {
                id: first.id,
                status: TaskStatus::Completed,
            },
        )
        .await
        .unwrap();
        assert!(
            all.iter()
                .find(|task| task.id == second.id)
                .unwrap()
                .blocked_by
                .is_empty()
        );
        let all = tasks(
            &mut setup,
            TaskAction::Update {
                id: second.id,
                title: Some("更新后的任务".into()),
                details: Some("保留说明".into()),
                status: Some(TaskStatus::InProgress),
                priority: Some(5),
                depends_on: Some(vec![]),
            },
        )
        .await
        .unwrap();
        let task = all
            .iter()
            .find(|task| task.id == second.id)
            .unwrap();
        assert_eq!(task.title, "更新后的任务");
        assert_eq!(task.details, "保留说明");
        assert_eq!(task.priority, 5);
        assert!(task.depends_on.is_empty());
        assert_eq!(
            tasks(
                &mut setup,
                TaskAction::List {
                    include_completed: false
                }
            )
            .await
            .unwrap()
            .len(),
            1
        );
        let report = super::super::actions::execute(
            &mut setup,
            orbit_protocol::SessionAction::Task {
                task: TaskAction::List {
                    include_completed: true,
                },
            },
            false,
        )
        .await
        .unwrap();
        assert!(report.text.contains("更新后的任务"));
        assert!(report.text.contains("先完成基础"));
    }
}
