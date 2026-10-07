use super::*;
use orbit_agent::tool::{ClosureTool, ToolPolicy};
use serde_json::json;

async fn fixture() -> (
    tempfile::TempDir,
    Planning,
    orbit_store::store::SessionWriter,
) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("sessions.db")).await.unwrap();
    let writer = store
        .create(
            dir.path().to_string_lossy().into_owned(),
            "test".into(),
            false,
        )
        .await
        .unwrap();
    let plan = Planning::new(
        store,
        writer.id().into(),
        orbit_runtime::filesystem::Workspace::new(dir.path()).unwrap(),
    )
    .await
    .unwrap();
    plan.enable();
    (dir, plan, writer)
}
async fn draft(plan: &Planning, files: Vec<&str>) -> State {
    plan.enter().await.unwrap();
    let current = plan.snapshot().await;
    plan.update(json!({"generation":current.generation,"title":"Implement feature","markdown":"## Scope\nRead relevant files and add the feature.\n## Validation\nRun regression tests.","steps":["Implement","Verify"],"files":files})).await.unwrap()
}
async fn reviewing(
    plan: &Planning,
) -> (
    tokio::task::JoinHandle<Result<State>>,
    CancellationToken,
    u64,
) {
    let token = CancellationToken::new();
    let cancel = token.clone();
    let service = plan.clone();
    let generation = plan.snapshot().await.generation;
    let task = tokio::spawn(async move { service.submit(generation, cancel).await });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !plan.waiting() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    (task, token, plan.snapshot().await.generation)
}
#[tokio::test]
async fn plan_read_only_checks_executions_not_only_tool_definitions() {
    let (_dir, plan, _writer) = fixture().await;
    let mut registry = ToolRegistry::new();
    for name in [
        "write_file",
        "run_command",
        "mcp__unsafe",
        "request_capability",
        "read_file",
        "ask_user",
        "spawn_agent",
    ] {
        registry.register(ClosureTool::new(name, "test", json!({}), |_, _| async {
            Ok("executed".into())
        }));
    }
    plan.install_policy(&mut registry);
    let copy = registry.clone();
    let pending = copy.execute(
        ToolCall::function("queued", "write_file", json!({})),
        CancellationToken::new(),
    );
    plan.enter().await.unwrap();
    assert!(pending.await.is_err());
    for name in [
        "write_file",
        "run_command",
        "mcp__unsafe",
        "request_capability",
    ] {
        assert!(!copy.definitions().iter().any(|d| d.function.name == name));
        assert!(
            copy.execute(
                ToolCall::function("c", name, json!({})),
                CancellationToken::new()
            )
            .await
            .is_err()
        );
    }
    for name in ["read_file", "ask_user"] {
        assert!(
            copy.execute(
                ToolCall::function("c", name, json!({})),
                CancellationToken::new()
            )
            .await
            .is_ok()
        );
    }
    assert!(
        copy.execute(
            ToolCall::function("c", "spawn_agent", json!({"role":"develop"})),
            CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert!(
        copy.execute(
            ToolCall::function("c", "spawn_agent", json!({"role":"explore"})),
            CancellationToken::new()
        )
        .await
        .is_ok()
    );
    // A child's cloned policy observes the same transitions.
    let mut child = ToolRegistry::new();
    child.register(ClosureTool::new(
        "write_file",
        "test",
        json!({}),
        |_, _| async { Ok("write".into()) },
    ));
    child.set_policy(copy.policy().unwrap());
    assert!(
        child
            .execute(
                ToolCall::function("c", "write_file", json!({})),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
}
#[tokio::test]
async fn plan_cannot_enter_until_mutating_tool_lease_finishes() {
    let (_dir, plan, _writer) = fixture().await;
    let lease = plan
        .0
        .gate
        .acquire(&ToolCall::function("write", "write_file", json!({})))
        .unwrap();
    assert!(plan.enter().await.is_err());
    assert_eq!(plan.snapshot().await.mode, WorkMode::Build);
    drop(lease);
    plan.enter().await.unwrap();
}
#[tokio::test]
async fn plan_feedback_revises_without_granting_permission_and_stale_approval_fails() {
    let (_dir, plan, _writer) = fixture().await;
    draft(&plan, vec![]).await;
    let (task, _cancel, generation) = reviewing(&plan).await;
    assert!(
        plan.action(Action::Feedback {
            generation,
            text: "  Add tests  ".into()
        })
        .await
        .unwrap()
        .is_none()
    );
    let state = task.await.unwrap().unwrap();
    assert_eq!(state.mode, WorkMode::Plan);
    assert_eq!(state.plan.as_ref().unwrap().feedback, "Add tests");
    let next = draft(&plan, vec![]).await;
    assert_eq!(next.plan.unwrap().revision, 2);
    assert!(plan.action(Action::Approve { generation }).await.is_err());
    assert!(
        plan.action(Action::Approve {
            generation: u64::MAX
        })
        .await
        .is_err()
    );
}
#[tokio::test]
async fn plan_approval_is_once_and_creates_linked_project_tasks_atomically() {
    let (_dir, plan, _writer) = fixture().await;
    draft(&plan, vec![]).await;
    let (task, _cancel, generation) = reviewing(&plan).await;
    let (a, b) = tokio::join!(
        plan.action(Action::Approve { generation }),
        plan.action(Action::Approve { generation })
    );
    assert!(a.unwrap().is_none());
    assert!(b.unwrap().is_none());
    let state = task.await.unwrap().unwrap();
    assert_eq!(state.mode, WorkMode::Build);
    let record = state.plan.unwrap();
    assert_eq!(record.task_ids.len(), 2);
    let session = plan.0.store.session(&plan.0.session).await.unwrap();
    assert_eq!(
        plan.0
            .store
            .tasks(&session.project_id, false)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        plan.0.store.planning(&plan.0.session).await.unwrap(),
        plan.snapshot().await
    );
}
#[tokio::test]
async fn plan_cancelled_review_survives_reopen_and_idle_approval_returns_one_prompt() {
    let (dir, plan, _writer) = fixture().await;
    draft(&plan, vec![]).await;
    let (task, cancel, generation) = reviewing(&plan).await;
    cancel.cancel();
    assert!(task.await.unwrap().is_err());
    let restored = Planning::new(
        plan.0.store.clone(),
        plan.0.session.clone(),
        orbit_runtime::filesystem::Workspace::new(dir.path()).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(restored.snapshot().await.mode, WorkMode::Plan);
    assert!(
        restored
            .action(Action::Approve { generation })
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        restored
            .action(Action::Approve { generation })
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn plan_changed_files_block_approval_until_new_revision() {
    let (dir, plan, _writer) = fixture().await;
    std::fs::write(dir.path().join("source.txt"), "before").unwrap();
    draft(&plan, vec!["source.txt"]).await;
    let (task, cancel, generation) = reviewing(&plan).await;
    std::fs::write(dir.path().join("source.txt"), "after").unwrap();
    assert!(
        plan.action(Action::Approve { generation })
            .await
            .unwrap_err()
            .to_string()
            .contains("文件已变化")
    );
    assert_eq!(plan.snapshot().await.mode, WorkMode::Plan);
    cancel.cancel();
    let _ = task.await;
}
#[tokio::test]
async fn plan_storage_rejects_stale_writes_and_file_paths_outside_workspace() {
    let (_dir, plan, _writer) = fixture().await;
    let state = draft(&plan, vec![]).await;
    assert!(
        plan.0
            .store
            .save_planning(&plan.0.session, state.generation - 1, &state)
            .await
            .is_err()
    );
    for path in ["../outside", "/etc/passwd"] {
        assert!(plan.fingerprints(vec![path.into()]).await.is_err());
    }
    plan.disable_file_tracking();
    assert!(
        plan.fingerprints(vec!["source.txt".into()])
            .await
            .unwrap_err()
            .to_string()
            .contains("禁用了文件读取")
    );
    plan.action(Action::Leave).await.unwrap();
    assert_eq!(plan.snapshot().await.plan.unwrap().status, Status::Draft);
}
#[tokio::test]
async fn plan_client_without_review_support_gets_error_instead_of_waiting_forever() {
    let (_dir, plan, _writer) = fixture().await;
    plan.0.enabled.store(false, Ordering::Release);
    let tool = PlanTool {
        planning: plan,
        name: "enter_plan",
    };
    assert!(
        tool.execute(
            ToolCall::function("p", "enter_plan", json!({})),
            CancellationToken::new()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn plan_waits_for_background_jobs_before_entering() {
    let (_dir, plan, _writer) = fixture().await;
    let mut registry = ToolRegistry::new();
    let jobs = registry.background_jobs().clone();
    plan.register(&mut registry);
    let cancel = CancellationToken::new();
    jobs.start(
        ToolCall::function("background", "run_command", json!({})),
        "test".into(),
        None,
        cancel.clone(),
        |token| async move {
            token.cancelled().await;
            Ok(ToolOutput::text("cancelled"))
        },
    )
    .unwrap();
    assert!(plan.enter().await.is_err());
    cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !plan.0.gate.idle() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    plan.enter().await.unwrap();
}

#[tokio::test]
async fn plan_policy_context_tracks_latest_mode_and_revision() {
    let (_dir, plan, _writer) = fixture().await;
    let mut registry = ToolRegistry::new();
    plan.install_policy(&mut registry);
    assert!(
        registry
            .policy_context()
            .unwrap()
            .contains("Parent work mode: build")
    );
    draft(&plan, vec![]).await;
    assert!(
        registry
            .policy_context()
            .unwrap()
            .contains("Parent work mode: plan")
    );
    assert!(
        registry
            .policy_context()
            .unwrap()
            .contains("Implement feature")
    );
    let (task, _cancel, generation) = reviewing(&plan).await;
    plan.action(Action::Approve { generation }).await.unwrap();
    task.await.unwrap().unwrap();
    assert!(
        registry
            .policy_context()
            .unwrap()
            .contains("Parent work mode: build")
    );
    assert!(registry.policy_context().unwrap().contains("approved"));
}
