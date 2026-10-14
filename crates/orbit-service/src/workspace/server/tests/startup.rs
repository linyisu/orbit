use super::*;
use orbit_protocol::resources::{Action, Snapshot};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn background_model_refresh_updates_the_open_session_catalog() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = Fixture::new(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
    let mut config = crate::config::FileConfig::load(
        &fixture.service.options.config,
        &fixture.service.options.auth_directory,
    )
    .unwrap();
    config
        .providers
        .get_mut("test")
        .unwrap()
        .spec
        .auto_refresh_models = true;
    fs::write(
        &fixture.service.options.config,
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    // A fresh public-metadata cache keeps this test entirely offline.
    let path = fixture
        .service
        .options
        .auth_directory
        .join("model-metadata/catalog.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, r#"{"source":{"url":"https://models.dev/api.json"},"providers":{"fixture":{"models":{"test":{"limit":{"context":128000}}}}}}"#).unwrap();
    let info = open(&fixture, None).await;
    let handle = fixture.service.session(&info.id).await.unwrap();
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        header.push(socket.read_u8().await.unwrap());
    }
    assert!(
        String::from_utf8(header)
            .unwrap()
            .starts_with("GET /v1/models")
    );
    let body = r#"{"data":[{"id":"test","context_tokens":200000}]}"#;
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if handle.current_info().await.context_tokens == 200000 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("background refresh never reached the opened session");
    handle.command(session::Command::Close).await.unwrap();
}

const MCP: &str = r#"touch "$1"
while [ ! -f "$2" ]; do sleep 0.02; done
while IFS= read -r line; do
case "$line" in
*'"method":"initialize"'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}' ;;
*'"method":"tools/list"'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"fixture","inputSchema":{"type":"object"}}]}}' ;;
esac
done"#;

fn enable_mcp(fixture: &mut Fixture) -> (PathBuf, PathBuf) {
    let started = fixture._root.path().join("mcp-started");
    let release = fixture._root.path().join("mcp-release");
    let mut config = crate::config::FileConfig::load(
        &fixture.service.options.config,
        &fixture.service.options.auth_directory,
    )
    .unwrap();
    config.mcp = serde_json::from_value(serde_json::json!({"servers": {"echo": {
        "command": "/bin/sh", "enabled": true, "timeout_seconds": 5,
        "args": ["-c", MCP, "fixture", started, release]
    }}}))
    .unwrap();
    fs::write(
        &fixture.service.options.config,
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    fixture.service.options.allow_mcp_start = true;
    (started, release)
}

async fn open(fixture: &Fixture, resume: Option<String>) -> SessionInfo {
    let started = std::time::Instant::now();
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        fixture.request(Request::Open {
            model: Some("test::test".into()),
            policy: Default::default(),
            resume,
            configuration: None,
        }),
    )
    .await
    .expect("session opening waited for optional initialization")
    .unwrap();
    eprintln!("isolated session open: {:?}", started.elapsed());
    let Response::Opened(info) = response else {
        panic!("expected opened session")
    };
    info
}

#[tokio::test]
async fn new_and_saved_sessions_open_while_model_refresh_and_mcp_are_stalled() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut fixture = Fixture::new(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
    let (_started, _release) = enable_mcp(&mut fixture);
    let mut config = crate::config::FileConfig::load(
        &fixture.service.options.config,
        &fixture.service.options.auth_directory,
    )
    .unwrap();
    config
        .providers
        .get_mut("test")
        .unwrap()
        .spec
        .auto_refresh_models = true;
    fs::write(
        &fixture.service.options.config,
        toml::to_string(&config).unwrap(),
    )
    .unwrap();
    let mut writer = fixture
        .handle
        .store
        .create(
            fixture
                .service
                .options
                .initial_directory
                .display()
                .to_string(),
            "test::test".into(),
        )
        .await
        .unwrap();
    let saved = writer.id().to_owned();
    writer
        .begin("saved message".into(), "test::test".into())
        .await
        .unwrap();
    writer
        .finish(
            "completed",
            vec![],
            Some(vec![orbit_model::types::ChatMessage::user("saved message")]),
        )
        .await
        .unwrap();
    drop(writer);
    let restored = open(&fixture, Some(saved.clone())).await;
    assert_eq!(restored.id, saved);
    let (_stalled, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let fresh = open(&fixture, None).await;
    assert_ne!(fresh.id, saved);
    assert!(!fresh.persisted);
    assert_eq!(open(&fixture, Some(saved.clone())).await.id, saved);
    let handle = fixture.service.session(&saved).await.unwrap();
    let page = handle.page(0, 0).await.unwrap();
    assert!(page.updates.iter().any(|update| matches!(update, orbit_protocol::Update::Started { prompt, .. } if prompt == "saved message")));
    let Snapshot::Mcp(statuses) = handle.resources(Action::Mcp, None).await.unwrap() else {
        panic!()
    };
    assert_eq!(statuses[0].detail, "正在连接");
    assert!(!statuses[0].connected);
    for info in [restored, fresh] {
        fixture
            .service
            .session(&info.id)
            .await
            .unwrap()
            .command(session::Command::Close)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn first_send_waits_for_mcp_tools_without_blocking_control_commands() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut fixture = Fixture::new(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
    let (started, release) = enable_mcp(&mut fixture);
    let info = open(&fixture, None).await;
    let handle = fixture.service.session(&info.id).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let submit = |id: &'static str| {
        let handle = handle.clone();
        tokio::spawn(async move {
            handle
                .command(session::Command::Start {
                    request_id: id.into(),
                    prompt: "use the available tools".into(),
                    images: vec![],
                    compact: false,
                    model: None,
                })
                .await
        })
    };
    let canceled = submit("cancel-before-ready");
    tokio::task::yield_now().await;
    tokio::time::timeout(
        Duration::from_secs(1),
        handle.command(session::Command::SetMode { mode: Mode::Yolo }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!canceled.is_finished());
    handle.command(session::Command::Cancel).await.unwrap();
    assert!(
        canceled
            .await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("取消")
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
    let send = submit("ready-send");
    fs::write(release, "ready").unwrap();
    tokio::time::timeout(Duration::from_secs(3), send)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let (mut socket, request) = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(socket.read_u8().await.unwrap());
            }
            let length: usize = String::from_utf8(header).unwrap().lines().find_map(|line| {
                line.to_ascii_lowercase().strip_prefix("content-length:")
                    .map(|value| value.trim().parse().unwrap())
            }).unwrap();
            let mut body = vec![0; length];
            socket.read_exact(&mut body).await.unwrap();
            let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
            if request["stream"] == true { break (socket, request); }
            // Session title generation races with the main streaming request.
            let body = r#"{"choices":[{"message":{"role":"assistant","content":"test"},"finish_reason":"stop"}]}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    }).await.unwrap();
    assert!(
        request["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "mcp__echo__echo")
    );
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"}}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
    handle.command(session::Command::Cancel).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while handle.view.lock().await.busy {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    handle.command(session::Command::Close).await.unwrap();
}
