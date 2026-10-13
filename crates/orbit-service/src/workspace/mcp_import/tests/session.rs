use super::*;
use crate::workspace::session::{Command, Handle};
use orbit_config::test_support::{TestConfig, TestModel, TestProvider};
use orbit_protocol::resources::Action;
use std::sync::Arc;

#[tokio::test]
async fn imports_while_awaiting_an_answer_without_blocking_the_session() {
    let (root, workspace, source) = fixture();
    let root = root.path();
    TestConfig::new()
        .provider(
            TestProvider::new("test")
                .key("test-only")
                .models([TestModel::new("mock")]),
        )
        .write(root.join("config.toml"))
        .unwrap();
    let options = crate::WorkspaceOptions {
        model: Some("test::mock".into()),
        initial_directory: workspace.clone(),
        config: root.join("config.toml"),
        auth_directory: root.join("auth"),
        database: root.join("sessions.db"),
        home: Some(root.into()),
        config_home: None,
        instruction_dir: None,
        allow_mcp_start: false,
        editor_command: "vi".into(),
    };
    let mut setup = crate::workspace::setup::prepare_service(&options, None)
        .await
        .unwrap();
    setup.start_session().await.unwrap();
    fs::write(
        source.join(".mcp.json"),
        r#"{"mcpServers":{"echo":{"command":"/bin/echo"}}}"#,
    )
    .unwrap();
    let mut scan = Scan::acquire(
        setup.session.id().into(),
        workspace.clone(),
        source.display().to_string(),
        Some(root.into()),
    )
    .await
    .unwrap();
    let token = scan.token();
    let id = scan.candidates[0].id.clone();
    // A controlled installer, with no network or dependency on Node/Python.
    let started = root.join("started");
    let release = root.join("release");
    scan.candidates[0].install = vec![vec![
        "/bin/sh".into(),
        "-c".into(),
        "touch \"$1\"; while [ ! -f \"$2\" ]; do sleep 0.05; done".into(),
        "installer".into(),
        started.display().to_string(),
        release.display().to_string(),
    ]];
    setup.resources.mcp_import = Some(scan);
    let tools = setup.resources.tools.clone();
    let handle = Arc::new(Handle::spawn(setup, None).await.unwrap());
    handle.command(Command::EnableUserInput).await.unwrap();
    let asking = tokio::spawn(async move {
        tools.execute(orbit_model::types::ToolCall::function("ask", "ask_user",
            serde_json::json!({"questions":[{"id":"q","title":"说明","prompt":"需要什么？","kind":"text"}]})),
            tokio_util::sync::CancellationToken::new()).await.unwrap()
    });
    let record = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(record) = handle
                .view
                .lock()
                .await
                .page(0)
                .unwrap()
                .user_inputs
                .first()
            {
                break record.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let installing = {
        let handle = handle.clone();
        tokio::spawn(async move {
            handle
                .resources(Action::InstallMcp { token, id }, None)
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while !started.exists() {
            assert!(
                !installing.is_finished(),
                "installation rejected while awaiting an answer"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // The installer remains blocked, but another actor command must complete.
    tokio::time::timeout(
        Duration::from_secs(1),
        handle.command(Command::SetMode {
            mode: orbit_protocol::Mode::Yolo,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!asking.is_finished());
    assert!(!installing.is_finished());
    fs::write(release, "go").unwrap();
    assert!(matches!(
        installing.await.unwrap().unwrap(),
        Snapshot::McpInstalled { .. }
    ));
    // Importing did not hot-swap the session's tools.
    let Snapshot::Mcp(servers) = handle.resources(Action::Mcp, None).await.unwrap() else {
        panic!()
    };
    assert!(servers.is_empty());
    assert!(matches!(
        handle
            .resources(
                Action::ScanMcp {
                    source: source.display().to_string()
                },
                None
            )
            .await
            .unwrap(),
        Snapshot::McpSource { .. }
    ));
    tokio::time::timeout(
        Duration::from_secs(1),
        handle.command(Command::AnswerUserInput {
            id: record.id,
            answers: Some(vec![orbit_protocol::user_input::Answer {
                question_id: "q".into(),
                text: "继续".into(),
                ..Default::default()
            }]),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(asking.await.unwrap().content.contains("继续"));
    handle.command(Command::Close).await.unwrap();
}
