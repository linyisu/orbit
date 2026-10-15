use super::*;
use orbit_config::test_support::{TestConfig, TestModel, TestProvider};
use orbit_model::types::{ChatMessage, RuntimeStateKind};
use orbit_store::types::RunCompletion;
use tokio::io::AsyncReadExt;

#[tokio::test]
async fn resumed_pending_files_restore_only_their_read_grants() {
    use base64::Engine;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let workspace = root.join("project");
    std::fs::create_dir(&workspace).unwrap();
    TestConfig::new()
        .provider(
            TestProvider::new("test")
                .endpoint("http://127.0.0.1:1/v1")
                .key("test-only")
                .models([TestModel::new("test")]),
        )
        .write(root.join("config.toml"))
        .unwrap();
    let options = crate::WorkspaceOptions {
        model: Some("test::test".into()),
        initial_directory: workspace.clone(),
        auth_directory: root.join("auth"),
        config: root.join("config.toml"),
        database: root.join("sessions.db"),
        allow_mcp_start: false,
        home: Some(root.into()),
        config_home: None,
        instruction_dir: None,
        editor_command: "vi".into(),
    };
    let mut setup = crate::workspace::setup::prepare_workspace(&options, &workspace, None)
        .await
        .unwrap();
    setup.start_session().await.unwrap();
    let attachments = import_files(
        &options.database,
        vec![orbit_protocol::FileUpload {
            name: "observations.txt".into(),
            data: base64::engine::general_purpose::STANDARD
                .encode("independent generated observation\n".repeat(10_000)),
        }],
    )
    .await
    .unwrap();
    let path = attachments[0].path().to_owned();
    assert!(matches!(
        &attachments[0],
        orbit_model::types::Attachment::File { text: None, .. }
    ));
    let sibling = path
        .parent()
        .unwrap()
        .join("unselected.txt");
    std::fs::write(&sibling, b"unselected generated material").unwrap();
    grant_files(&setup.resources.workspace, &attachments).unwrap();
    let session = setup.session.id().to_owned();
    setup
        .session
        .store
        .queue_session_input_with_attachments(&session, String::new(), attachments)
        .await
        .unwrap();
    drop(setup);

    let mut resumed =
        crate::workspace::setup::prepare_workspace(&options, &workspace, Some(session))
            .await
            .unwrap();
    assert!(
        resumed
            .resources
            .workspace
            .authorize_read(&path)
            .is_err()
    );
    let Started { execution, .. } = start(&mut resumed, None, Vec::new(), Vec::new(), false, None)
        .await
        .unwrap();
    let readable = resumed
        .resources
        .workspace
        .authorize_read(&path)
        .is_ok();
    let sibling_readable = resumed
        .resources
        .workspace
        .authorize_read(&sibling)
        .is_ok();
    execution.cancellation.cancel();
    let _ = execution.task.await.unwrap();
    resumed
        .session
        .finish(RunCompletion::Cancelled)
        .await
        .unwrap();
    assert!(readable, "resumed queued files must retain read access");
    assert!(!sibling_readable, "the attachment directory is not granted");
}

#[tokio::test]
async fn next_model_request_includes_history_recovered_during_finalization() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    TestConfig::new()
        .provider(
            TestProvider::new("test")
                .endpoint(format!("http://{}/v1", listener.local_addr().unwrap()))
                .key("test-only")
                .models([TestModel::new("test")]),
        )
        .write(root.join("config.toml"))
        .unwrap();
    let options = crate::WorkspaceOptions {
        model: Some("test::test".into()),
        initial_directory: root.into(),
        auth_directory: root.join("auth"),
        config: root.join("config.toml"),
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
        .session
        .begin("earlier request".into(), "test::test".into())
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(&options.database).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_terminal BEFORE UPDATE OF status ON runs \
         BEGIN SELECT RAISE(ABORT, 'injected terminal failure'); END;",
        )
        .unwrap();
    let history = vec![
        ChatMessage::user("earlier request"),
        ChatMessage::assistant("recovered answer"),
    ];
    assert!(
        setup
            .session
            .finish(RunCompletion::Completed { history })
            .await
            .is_err()
    );
    assert!(
        setup
            .session
            .snapshot
            .history
            .is_empty()
    );
    connection
        .execute_batch("DROP TRIGGER reject_terminal;")
        .unwrap();
    let Started { execution, .. } = start(
        &mut setup,
        Some("next request".into()),
        Vec::new(),
        Vec::new(),
        false,
        None,
    )
    .await
    .unwrap();
    let (mut socket, _) =
        tokio::time::timeout(std::time::Duration::from_secs(3), listener.accept())
            .await
            .unwrap()
            .unwrap();
    let request = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            header.push(socket.read_u8().await.unwrap());
        }
        let length = String::from_utf8(header)
            .unwrap()
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|value| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        let mut body = vec![0; length];
        socket
            .read_exact(&mut body)
            .await
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()
    })
    .await
    .unwrap();
    let runtime_prefixes = [RuntimeStateKind::ToolPolicy, RuntimeStateKind::ProjectState]
        .map(|kind| orbit_model::runtime_context::render(kind, ""));
    let dialogue: Vec<_> = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| matches!(message["role"].as_str(), Some("user" | "assistant")))
        .map(|message| {
            (
                message["role"].as_str().unwrap(),
                message["content"].as_str().unwrap(),
            )
        })
        .filter(|(role, content)| {
            *role != "user"
                || !runtime_prefixes
                    .iter()
                    .any(|prefix| content.starts_with(prefix))
        })
        .collect();
    assert_eq!(
        dialogue,
        vec![
            ("user", "earlier request"),
            ("assistant", "recovered answer"),
            ("user", "next request"),
        ]
    );
    execution.cancellation.cancel();
    let _ = execution.task.await.unwrap();
    setup
        .session
        .finish(RunCompletion::Cancelled)
        .await
        .unwrap();
}
