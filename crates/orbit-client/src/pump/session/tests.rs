use super::*;
use orbit_config::test_support::{TestConfig, TestProvider};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn model_refresh_preserves_controls_and_applies_single_discovery() {
    const DISCOVERY_COMPLETION_TIMEOUT: Duration = Duration::from_secs(5);

    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let config = root.path().join("config.toml");
    let mut settings = TestConfig::new().provider(
        TestProvider::new("slow")
            .endpoint(format!("http://{}/v1", listener.local_addr().unwrap()))
            .automatic(),
    );
    let blocked_proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    settings.config.web.proxy = Some(format!("http://{}", blocked_proxy.local_addr().unwrap()));
    drop(blocked_proxy);
    settings.write(&config).unwrap();
    let launcher = orbit_build::build_test_peer(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        "orbit-client-test-peer",
    );
    let local = crate::configuration::Local {
        config: config.clone(),
        auth: root.path().join("auth"),
        model: None,
    };
    let options = SessionOptions {
        connection: Arc::new(Connection {
            local_configuration: Some(local),
            target: crate::transport::Target::Local,
            workspace: root
                .path()
                .to_string_lossy()
                .into_owned(),
            executable: launcher.to_string_lossy().into_owned(),
            config,
            database: None,
            auth_dir: None,
            allow_mcp_start: false,
            unattended: true,
            interactive: None,
        }),
        resume: None,
        model: Some("slow::declared-model".into()),
        relink_on_resume: false,
        recover_draft: false,
        attachment_cache: root.path().join("attachments"),
    };
    let (commands, receiver) = mpsc::channel(8);
    let (events, mut incoming) = mpsc::channel(16);
    let (progress, _updates) = crate::interaction::channel();
    let task = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        observe(options, receiver, &events, progress).await
    }));
    tokio::time::timeout(Duration::from_secs(3), async {
        while !matches!(
            incoming
                .recv()
                .await
                .expect("observer events remain open"),
            Event::Response(Response::Opened(_))
        ) {}
    })
    .await
    .expect("observer must open the isolated peer");

    commands
        .send(Command::RefreshModels)
        .await
        .unwrap();
    let (mut held, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .expect("refresh must start model discovery")
        .unwrap();
    commands
        .send(Command::RefreshModels)
        .await
        .unwrap();
    commands
        .send(Command::Request(Request::Cancel {
            session: "synthetic-session".into(),
        }))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_millis(500), async {
        loop {
            match incoming
                .recv()
                .await
                .expect("observer events remain open")
            {
                Event::Response(Response::Ok) => break,
                Event::Error(error) => panic!("refresh failed: {error}"),
                _ => {}
            }
        }
    })
    .await
    .expect("a stalled catalogue must not delay cancellation");
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err(),
        "repeated refresh commands must share the pending discovery"
    );
    let mut request = Vec::new();
    tokio::time::timeout(DISCOVERY_COMPLETION_TIMEOUT, async {
        while !request.ends_with(b"\r\n\r\n") {
            request.push(held.read_u8().await.unwrap());
        }
        let body = r#"{"data":[{"id":"refreshed-model"}]}"#;
        held.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    })
    .await
    .expect("model discovery must exchange its isolated HTTP request and response");
    tokio::time::timeout(DISCOVERY_COMPLETION_TIMEOUT, async {
        loop {
            match incoming
                .recv()
                .await
                .expect("observer events remain open")
            {
                Event::Response(Response::Ok) => break,
                Event::Error(error) => panic!("refresh failed: {error}"),
                _ => {}
            }
        }
    })
    .await
    .expect("completed discovery must apply its configuration to the original session");
    commands
        .send(Command::RefreshModels)
        .await
        .unwrap();
    let (_pending, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .expect("completed discovery must allow a later refresh")
        .unwrap();
    drop(commands);
    tokio::time::timeout(Duration::from_millis(500), task)
        .await
        .expect("closing the observer must cancel pending discovery")
        .unwrap()
        .unwrap();
}
