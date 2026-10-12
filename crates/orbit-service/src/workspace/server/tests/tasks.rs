use super::*;
use orbit_protocol::project::{TaskAction, TaskStatus};

#[tokio::test]
async fn structured_tasks_use_the_owning_session_actor() {
    let fixture = Fixture::new("https://example.com/v1").await;
    let session = fixture.handle.info.id.clone();
    // Another client can create a task after the desktop's last list snapshot.
    // The response identifies this request's task independently of list order.
    fixture
        .request(Request::Tasks {
            session: session.clone(),
            expected_workspace: None,
            action: TaskAction::Create {
                title: "后台 Agent 创建的任务".into(),
                details: String::new(),
                priority: 10,
                parent_id: None,
                depends_on: Vec::new(),
            },
        })
        .await
        .unwrap();
    let Response::Tasks(snapshot) = fixture
        .request(Request::Tasks {
            session: session.clone(),
            expected_workspace: None,
            action: TaskAction::Create {
                title: "  桌面项目任务  ".into(),
                details: "持久化说明".into(),
                priority: 3,
                parent_id: None,
                depends_on: Vec::new(),
            },
        })
        .await
        .unwrap()
    else {
        panic!("missing structured task snapshot");
    };
    let tasks = snapshot.tasks;
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].title, "后台 Agent 创建的任务");
    let task = tasks
        .iter()
        .find(|task| task.title == "桌面项目任务")
        .unwrap();
    assert_eq!(snapshot.changed, Some(task.id));
    assert_eq!(task.title, "桌面项目任务");
    assert_eq!(task.details, "持久化说明");
    assert_eq!(task.priority, 3);
    assert_eq!(task.status, TaskStatus::Pending);

    let invalid = fixture
        .request(Request::Tasks {
            session: session.clone(),
            expected_workspace: None,
            action: TaskAction::Update {
                id: task.id,
                title: None,
                details: None,
                status: None,
                priority: None,
                depends_on: Some(vec![task.id]),
            },
        })
        .await;
    assert!(invalid.is_err(), "self dependency must be rejected");

    let Response::Tasks(reloaded) = fixture
        .request(Request::Tasks {
            session,
            expected_workspace: None,
            action: TaskAction::List {
                include_completed: true,
            },
        })
        .await
        .unwrap()
    else {
        panic!("missing reloaded task snapshot");
    };
    assert_eq!(reloaded.changed, None);
    assert_eq!(reloaded.tasks.len(), 2);
    let reloaded = reloaded
        .tasks
        .iter()
        .find(|reloaded| reloaded.id == task.id)
        .unwrap();
    assert!(reloaded.depends_on.is_empty());
}

#[tokio::test]
async fn tasks_workspace_guard_rejects_stale_creates_in_the_actor() {
    let fixture = Fixture::new("https://example.com/v1").await;
    fs::create_dir(fixture.external.join(".git")).unwrap();
    let previous = fixture.handle.info.workspace.clone();
    fixture
        .handle
        .action(orbit_protocol::SessionAction::ChangeDirectory {
            path: fixture.external.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let current = fixture.handle.current_info().await.workspace;
    assert_ne!(previous, current);

    let create = || TaskAction::Create {
        title: "目录切换前的任务表单".into(),
        details: String::new(),
        priority: 0,
        parent_id: None,
        depends_on: Vec::new(),
    };
    // Check the owning actor directly as well as the wire dispatcher: a check
    // before enqueueing alone would miss a directory change ahead in its queue.
    let error = fixture
        .handle
        .tasks(create(), Some(previous.clone()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("工作区已切换"));
    let error = fixture
        .request(Request::Tasks {
            session: fixture.handle.info.id.clone(),
            expected_workspace: Some(previous),
            action: create(),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("工作区已切换"));

    let Response::Tasks(snapshot) = fixture
        .request(Request::Tasks {
            session: fixture.handle.info.id.clone(),
            expected_workspace: Some(current.clone()),
            action: TaskAction::List {
                include_completed: true,
            },
        })
        .await
        .unwrap()
    else {
        panic!("missing task snapshot after rejected stale creates");
    };
    assert!(snapshot.tasks.is_empty());

    // Legacy requests without a guard still operate on the current workspace.
    let Response::Tasks(snapshot) = fixture
        .request(Request::Tasks {
            session: fixture.handle.info.id.clone(),
            expected_workspace: None,
            action: create(),
        })
        .await
        .unwrap()
    else {
        panic!("missing legacy task create response");
    };
    assert_eq!(snapshot.tasks.len(), 1);
    assert_eq!(snapshot.changed, Some(snapshot.tasks[0].id));
    let Response::Tasks(reloaded) = fixture
        .request(Request::Tasks {
            session: fixture.handle.info.id.clone(),
            expected_workspace: Some(current),
            action: TaskAction::List {
                include_completed: true,
            },
        })
        .await
        .unwrap()
    else {
        panic!("missing guarded task snapshot");
    };
    assert_eq!(reloaded.tasks, snapshot.tasks);
}
