use super::*;
use orbit_agent::tool::ClosureTool;
use orbit_config::test_support::{TestConfig, TestModel, TestProvider};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

async fn setup(root: &std::path::Path) -> crate::PreparedWorkspace {
    let config = root.join("config.toml");
    TestConfig::new()
        .provider(
            TestProvider::new("test")
                .endpoint("http://127.0.0.1:1/v1")
                .key("test-only")
                .models([TestModel::new("test")]),
        )
        .write(&config)
        .unwrap();
    let options = crate::WorkspaceOptions {
        model: Some("test::test".into()),
        initial_directory: root.into(),
        auth_directory: root.join("auth"),
        config,
        database: root.join("sessions.db"),
        allow_mcp_start: false,
        home: Some(root.into()),
        config_home: None,
        instruction_dir: None,
        editor_command: "vi".into(),
    };
    let mut setup = crate::workspace::setup::prepare_workspace(&options, root, None)
        .await
        .unwrap();
    setup.start_session().await.unwrap();
    setup
}

#[tokio::test]
async fn catalogue_discovers_stored_children_and_their_own_workspace_without_execution_events() {
    let root = tempfile::tempdir().unwrap();
    let mut setup = setup(root.path()).await;
    let store = setup.session.store.clone();
    let parent = setup.session.id().to_owned();
    let child_workspace = root.path().join("isolated-child");
    std::fs::create_dir(&child_workspace).unwrap();
    let mut ids = Vec::new();
    for index in 0..35 {
        let mut child = store
            .create_subagent(
                &parent,
                "test::test".into(),
                "develop".into(),
                format!("历史子任务 {index}"),
            )
            .await
            .unwrap();
        child
            .move_to(&child_workspace, None)
            .await
            .unwrap();
        child
            .begin("stored child run".into(), "test::test".into())
            .await
            .unwrap();
        child
            .finish(
                &orbit_store::types::RunCompletion::Completed {
                    history: vec![
                        orbit_model::types::ChatMessage::user("stored child run"),
                        orbit_model::types::ChatMessage::assistant("stored child result"),
                    ],
                },
                vec![],
            )
            .await
            .unwrap();
        ids.push(child.id().to_owned());
    }
    // Listing is independent of model-tool visibility, while the capability
    // ceiling still applies to this native request.
    setup
        .resources
        .tools
        .remove("ReadAgent");
    let Snapshot::List { agents, next } = execute(&mut setup, Action::List { offset: 0 })
        .await
        .unwrap()
    else {
        panic!("list expected")
    };
    assert_eq!(agents.len(), 32);
    assert_eq!(next, Some(32));
    assert_eq!(agents[0].id, ids[0]);
    assert_eq!(agents[0].workspace, child_workspace.to_string_lossy());
    assert_eq!(agents[0].status, "completed");
    assert!(agents[0].run > 0);
    let Snapshot::List { agents, next } = execute(&mut setup, Action::List { offset: 32 })
        .await
        .unwrap()
    else {
        panic!("list expected")
    };
    assert_eq!(agents.len(), 3);
    assert_eq!(agents[2].id, ids[34]);
    assert!(next.is_none());
    setup
        .resources
        .policy
        .disabled
        .push(Capability::Subagents);
    assert!(
        execute(&mut setup, Action::List { offset: 0 })
            .await
            .unwrap_err()
            .to_string()
            .contains("已禁用")
    );
}

#[tokio::test]
async fn native_mutations_check_parent_and_run_without_executing_untyped_tools() {
    let root = tempfile::tempdir().unwrap();
    let mut setup = setup(root.path()).await;
    let store = setup.session.store.clone();
    let parent = setup.session.id().to_owned();
    let mut child = store
        .create_subagent(
            &parent,
            "test::test".into(),
            "explore".into(),
            "owned child".into(),
        )
        .await
        .unwrap();
    child
        .begin("hold".into(), "test::test".into())
        .await
        .unwrap();
    let id = child.id().to_owned();
    let run = child.active_run_id().unwrap();
    let foreign_parent = store
        .create(root.path().display().to_string(), "test::test".into())
        .await
        .unwrap();
    let foreign = store
        .create_subagent(
            foreign_parent.id(),
            "test::test".into(),
            "explore".into(),
            "foreign child".into(),
        )
        .await
        .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    setup
        .resources
        .tools
        .register(ClosureTool::new(
            "StopAgents",
            "test routing",
            serde_json::json!({}),
            move |arguments, _| {
                let count = count.clone();
                async move {
                    assert_eq!(arguments["expected_run_id"], run);
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok("{}".into())
                }
            },
        ));
    assert!(
        execute(
            &mut setup,
            Action::Cancel {
                id: foreign.id().into(),
                run
            }
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("does not belong")
    );
    assert!(
        execute(
            &mut setup,
            Action::Send {
                id: foreign.id().into(),
                message: "must not deliver".into()
            }
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("does not belong")
    );
    assert!(
        execute(
            &mut setup,
            Action::Cancel {
                id: id.clone(),
                run: run + 1
            }
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("另一轮任务")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let error = execute(
        &mut setup,
        Action::Cancel {
            id: id.clone(),
            run,
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("inactive"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        store
            .session_revision(&id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "running",
        "acknowledgement must not forge terminal storage state"
    );
    setup
        .resources
        .tools
        .remove("SendAgentMessage");
    assert!(
        execute(
            &mut setup,
            Action::Send {
                id,
                message: "retained message".into()
            }
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("不能")
    );
}

#[tokio::test]
async fn actor_rejects_a_child_catalogue_from_a_previous_workspace() {
    let root = tempfile::tempdir().unwrap();
    let setup = setup(root.path()).await;
    let workspace = setup
        .session
        .snapshot
        .session
        .workspace
        .clone();
    let handle = crate::workspace::session::Handle::spawn(setup, None)
        .await
        .unwrap();
    let error = handle
        .subagents(Action::List { offset: 0 }, "previous-workspace".into())
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("工作区已切换")
    );
    assert!(matches!(
        handle
            .subagents(Action::List { offset: 0 }, workspace)
            .await
            .unwrap(),
        Snapshot::List { .. }
    ));
}
