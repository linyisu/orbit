use super::*;
use crate::store::SessionWriter;

struct Board {
    root: tempfile::TempDir,
    store: Store,
    project: String,
    first: SessionWriter,
    second: SessionWriter,
}
impl Board {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path().join("orbit.db")).await.unwrap();
        let first = store
            .create(root.path().to_string_lossy().into_owned(), "mock".into())
            .await
            .unwrap();
        let second = store
            .create(root.path().to_string_lossy().into_owned(), "mock".into())
            .await
            .unwrap();
        let project = store.session(first.id()).await.unwrap().project_id;
        Self {
            root,
            store,
            project,
            first,
            second,
        }
    }
    async fn task(&self, title: &str, depends_on: Vec<i64>) -> ProjectTask {
        self.store
            .create_task(
                &self.project,
                NewTask {
                    title: title.into(),
                    depends_on,
                    ..Default::default()
                },
            )
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn competing_connections_cannot_share_ownership_and_only_owner_or_user_can_release() {
    let board = Board::new().await;
    let task = board.task("implement parser", vec![]).await;
    assert!(
        board
            .store
            .update_task(
                &board.project,
                task.id,
                TaskChanges {
                    status: Some(TaskStatus::Completed),
                    ..Default::default()
                },
                TaskActor::Session(board.first.id())
            )
            .await
            .is_err()
    );
    let other = Store::open(board.store.path()).await.unwrap();
    let (first, second) = tokio::join!(
        board
            .store
            .claim_task(&board.project, task.id, board.first.id()),
        other.claim_task(&board.project, task.id, board.second.id())
    );
    assert_ne!(first.is_ok(), second.is_ok());
    let (winner, loser) = if first.is_ok() {
        (board.first.id(), board.second.id())
    } else {
        (board.second.id(), board.first.id())
    };
    assert!(
        board
            .store
            .update_task(
                &board.project,
                task.id,
                TaskChanges {
                    status: Some(TaskStatus::Completed),
                    ..Default::default()
                },
                TaskActor::Session(loser)
            )
            .await
            .is_err()
    );
    assert!(
        board
            .store
            .release_task(&board.project, task.id, TaskActor::Session(loser))
            .await
            .is_err()
    );
    let current = board
        .store
        .tasks(&board.project, false)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(current.owner_session_id.as_deref(), Some(winner));
    let released = board
        .store
        .release_task(&board.project, task.id, TaskActor::User)
        .await
        .unwrap();
    assert_eq!(released.owner_session_id, None);
    assert_eq!(released.status, TaskStatus::Pending);
    assert!(
        board
            .store
            .claim_task(&board.project, task.id, loser)
            .await
            .is_ok()
    );
    let directory = board.root.path().join("moved-project");
    std::fs::create_dir(&directory).unwrap();
    let owner = if loser == board.first.id() {
        &board.first
    } else {
        &board.second
    };
    owner.move_to(&directory, None).await.unwrap();
    let released = board
        .store
        .tasks(&board.project, false)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(released.owner_session_id, None);
    assert_eq!(released.status, TaskStatus::Pending);
}

#[tokio::test]
async fn dependency_changes_are_atomic_acyclic_project_scoped_and_gate_execution() {
    let board = Board::new().await;
    let first = board.task("implement", vec![]).await;
    let second = board.task("verify", vec![first.id]).await;
    let third = board.task("release", vec![second.id]).await;
    assert_eq!(third.blocked_by, [second.id]);
    assert!(
        board
            .store
            .claim_task(&board.project, second.id, board.first.id())
            .await
            .is_err()
    );
    assert!(
        board
            .store
            .update_task(
                &board.project,
                second.id,
                TaskChanges {
                    status: Some(TaskStatus::Completed),
                    ..Default::default()
                },
                TaskActor::User
            )
            .await
            .is_err()
    );
    assert!(
        board
            .store
            .update_task(
                &board.project,
                first.id,
                TaskChanges {
                    title: Some("must roll back".into()),
                    depends_on: Some(vec![third.id]),
                    ..Default::default()
                },
                TaskActor::User
            )
            .await
            .is_err()
    );
    let current = board.store.tasks(&board.project, false).await.unwrap();
    assert_eq!(
        current.iter().find(|t| t.id == first.id).unwrap().title,
        "implement"
    );
    let foreign_dir = board.root.path().join("other");
    std::fs::create_dir(&foreign_dir).unwrap();
    let foreign = board.store.resolve_project(&foreign_dir).await.unwrap();
    let external = board
        .store
        .create_task(&foreign.id, NewTask::new("external"))
        .await
        .unwrap();
    assert!(
        board
            .store
            .update_task(
                &board.project,
                first.id,
                TaskChanges {
                    depends_on: Some(vec![external.id]),
                    ..Default::default()
                },
                TaskActor::User
            )
            .await
            .is_err()
    );
    assert!(
        board
            .store
            .claim_task(&foreign.id, external.id, board.first.id())
            .await
            .is_err()
    );
    board
        .store
        .update_task(
            &board.project,
            first.id,
            TaskChanges {
                status: Some(TaskStatus::Completed),
                ..Default::default()
            },
            TaskActor::User,
        )
        .await
        .unwrap();
    let claimed = board
        .store
        .claim_task(&board.project, second.id, board.second.id())
        .await
        .unwrap();
    assert!(claimed.blocked_by.is_empty());
    // Reopening the prerequisite cannot invalidate work already in progress.
    assert!(
        board
            .store
            .update_task(
                &board.project,
                first.id,
                TaskChanges {
                    status: Some(TaskStatus::Pending),
                    ..Default::default()
                },
                TaskActor::User
            )
            .await
            .is_err()
    );
    drop(board.first);
    drop(board.second);
    drop(board.store);
    let reopened = Store::open(board.root.path().join("orbit.db"))
        .await
        .unwrap();
    let restored = reopened.tasks(&board.project, true).await.unwrap();
    assert_eq!(
        restored.iter().find(|t| t.id == second.id).unwrap(),
        &claimed
    );
}

#[tokio::test]
async fn child_worktrees_share_the_board_and_terminal_receipts_release_unfinished_claims() {
    let board = Board::new().await;
    let mut parent = board.first;
    parent
        .begin("delegate".into(), "mock".into())
        .await
        .unwrap();
    let mut child = board
        .store
        .create_subagent(
            parent.id(),
            "mock".into(),
            "develop".into(),
            "implement".into(),
        )
        .await
        .unwrap();
    let directory = board.root.path().join("child-workspace");
    std::fs::create_dir(&directory).unwrap();
    let moved = child.move_to(&directory, None).await.unwrap();
    assert_eq!(moved.project_id, board.project);
    let task = board
        .store
        .create_task(&board.project, NewTask::new("unfinished work"))
        .await
        .unwrap();
    board
        .store
        .claim_task(&board.project, task.id, child.id())
        .await
        .unwrap();
    child.begin("work".into(), "mock".into()).await.unwrap();
    let invocation = board
        .store
        .begin_agent_invocation(
            parent.id(),
            child.id(),
            orbit_model::types::ToolCall::function("child", "spawn_agent", serde_json::json!({})),
        )
        .await
        .unwrap();
    child
        .finish_subagent(
            "cancelled",
            None,
            parent.id().into(),
            invocation,
            serde_json::json!({"status":"cancelled"}),
        )
        .await
        .unwrap();
    let released = board
        .store
        .tasks(&board.project, false)
        .await
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(released.owner_session_id, None);
    assert_eq!(released.status, TaskStatus::Pending);
    board
        .store
        .claim_task(&board.project, task.id, board.second.id())
        .await
        .unwrap();
}
