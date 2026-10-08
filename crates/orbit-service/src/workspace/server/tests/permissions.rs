use super::*;
use orbit_protocol::{Mode, SessionInfo};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Fixture {
    _root: tempfile::TempDir,
    service: Service,
    handle: Arc<Handle>,
    external: std::path::PathBuf,
}

impl Fixture {
    async fn new(url: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let external = root.path().join("external");
        fs::create_dir(&workspace).unwrap();
        fs::create_dir(&external).unwrap();
        let config = root.path().join("config.toml");
        fs::write(
            &config,
            format!(
                r#"
[providers.test]
base_url="{url}"
api_key="test-only"
auto_refresh_models=false
models=[{{id="test"}}]
"#
            ),
        )
        .unwrap();
        let options = crate::WorkspaceOptions {
            model: Some("test::test".into()),
            initial_directory: workspace.clone(),
            auth_directory: root.path().join("auth"),
            config,
            database: root.path().join("sessions.db"),
            allow_mcp_start: false,
            home: Some(root.path().into()),
            config_home: None,
            instruction_dir: None,
            editor_command: "vi".into(),
        };
        let setup = crate::workspace::setup::prepare_workspace(&options, &workspace, None)
            .await
            .unwrap();
        // An existing external capability allows the tool to reach its separate write approval.
        setup
            .resources
            .workspace
            .policy()
            .record_grant(orbit_runtime::policy::FilesystemGrant {
                environment: orbit_runtime::capability::EnvironmentId::Local,
                path: orbit_runtime::filesystem::ResolvedPath::resolve(&external, None).unwrap(),
                read: true,
                write: true,
            });
        let handle = Arc::new(Handle::spawn(setup, None));
        let service = Service {
            options,
            sessions: Mutex::new([(handle.info.id.clone(), handle.clone())].into()),
            opening: Mutex::new(()),
            last_activity: Mutex::new(tokio::time::Instant::now()),
            stop: tokio_util::sync::CancellationToken::new(),
            closing: AtomicBool::new(false),
        };
        Self {
            _root: root,
            service,
            handle,
            external,
        }
    }

    async fn request(&self, request: Request) -> Result<Response> {
        self.service.dispatch(request, &self.service.options).await
    }

    async fn mode(&self, mode: Mode) -> SessionInfo {
        let Response::Profile(info) = self
            .request(Request::SetMode {
                session: self.handle.info.id.clone(),
                mode,
            })
            .await
            .unwrap()
        else {
            panic!("missing mode confirmation")
        };
        *info
    }

    async fn page(&self, after: usize) -> protocol::Page {
        let Response::Page(page) = self
            .request(Request::Poll {
                session: self.handle.info.id.clone(),
                after,
            })
            .await
            .unwrap()
        else {
            panic!("missing session page")
        };
        page
    }

    async fn wait(&self, predicate: impl Fn(&protocol::Page) -> bool) -> protocol::Page {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let page = self.page(0).await;
                if predicate(&page) {
                    return page;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("session failed to reach expected state")
    }
}

#[tokio::test]
async fn permission_changes_release_only_eligible_waiters_and_reach_all_observers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = Fixture::new(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
    let inside = fixture.service.options.initial_directory.join("inside.txt");
    let outside = fixture.external.join("outside.txt");
    let model = tokio::spawn(write_model(listener, outside.clone()));
    let info = fixture.mode(Mode::Ask).await;
    assert_eq!((info.mode, info.mode_revision), (Mode::Ask, 1));
    let observer_a = fixture.page(0).await.next;
    let observer_b = fixture.page(0).await.next;
    fixture
        .request(Request::Start {
            session: info.id.clone(),
            request_id: "permissions".into(),
            prompt: "write the files".into(),
            images: vec![],
            compact: false,
            model: None,
        })
        .await
        .unwrap();
    let pending = fixture.wait(|page| !page.approvals.is_empty()).await;
    assert!(pending.timing.unwrap().paused);
    assert_eq!(fixture.handle.current_info().await.mode, Mode::Ask);
    assert!(!inside.exists());
    let first = pending.approvals[0].id;
    fixture.mode(Mode::Auto).await;
    let pending = fixture
        .wait(|page| page.approvals.iter().any(|a| a.id != first))
        .await;
    assert_eq!(fs::read_to_string(&inside).unwrap(), "inside");
    assert!(!outside.exists());
    assert!(pending.timing.unwrap().paused);
    // Reapplying Auto leaves the outside write waiting; Yolo releases that same request.
    fixture.mode(Mode::Auto).await;
    assert_eq!(
        fixture.page(0).await.approvals[0].id,
        pending.approvals[0].id
    );
    let confirmed = fixture.mode(Mode::Yolo).await;
    fixture.wait(|page| !page.busy).await;
    model.await.unwrap();
    assert_eq!(fs::read_to_string(&outside).unwrap(), "outside");
    for cursor in [observer_a, observer_b] {
        let page = fixture.page(cursor).await;
        let profile = page
            .updates
            .iter()
            .rev()
            .find_map(|update| match update {
                protocol::Update::Profile(info) => Some(info),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            (profile.mode, profile.mode_revision),
            (confirmed.mode, confirmed.mode_revision)
        );
        assert!(page.approvals.is_empty());
    }
    let store = orbit_store::store::Store::open(&fixture.service.options.database)
        .await
        .unwrap();
    assert_eq!(store.tui_mode(&info.id).await.unwrap(), "YOLO");
    let Response::Opened(reopened) = fixture
        .request(Request::Open {
            resume: Some(info.id),
            model: None,
            configuration: None,
            policy: Default::default(),
        })
        .await
        .unwrap()
    else {
        panic!("missing reopened session")
    };
    assert_eq!(reopened.mode, Mode::Yolo);
}

#[tokio::test]
async fn failed_permission_persistence_never_publishes_or_confirms_the_change() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let before = fixture.mode(Mode::Ask).await;
    let cursor = fixture.page(0).await.next;
    let db = rusqlite::Connection::open(&fixture.service.options.database).unwrap();
    db.execute_batch("CREATE TRIGGER refuse_mode BEFORE INSERT ON tui_session_state BEGIN SELECT RAISE(ABORT, 'test storage failure'); END;").unwrap();
    assert!(
        fixture
            .request(Request::SetMode {
                session: before.id.clone(),
                mode: Mode::Yolo
            })
            .await
            .is_err()
    );
    let after = fixture.handle.current_info().await;
    assert_eq!(
        (after.mode, after.mode_revision),
        (before.mode, before.mode_revision)
    );
    assert!(fixture.page(cursor).await.updates.is_empty());
    db.execute_batch("DROP TRIGGER refuse_mode;").unwrap();
    assert_eq!(
        fixture.mode(Mode::Auto).await.mode_revision,
        before.mode_revision + 1
    );
}

/// Only the external model is simulated; dispatch, actor, policy, tool writes and store are real.
async fn write_model(listener: tokio::net::TcpListener, outside: std::path::PathBuf) {
    let mut step = 0;
    while step < 3 {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            header.push(socket.read_u8().await.unwrap());
        }
        let header = String::from_utf8(header).unwrap();
        let length: usize = header
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse().unwrap())
            })
            .unwrap();
        let mut body = vec![0; length];
        socket.read_exact(&mut body).await.unwrap();
        let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let (kind, body) = if request["stream"] == true {
            let delta = match step {
                0 | 1 => {
                    let args = if step == 0 {
                        json!({"path":"inside.txt","content":"inside"})
                    } else {
                        json!({"path":outside,"content":"outside"})
                    };
                    json!({"tool_calls":[{"index":0,"id":format!("write-{step}"),"type":"function","function":{"name":"write_file","arguments":args.to_string()}}]})
                }
                _ => json!({"content":"done"}),
            };
            let finish = if step < 2 { "tool_calls" } else { "stop" };
            step += 1;
            (
                "text/event-stream",
                format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"choices":[{"index":0,"delta":delta}]}),
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":finish}]})
                ),
            )
        } else {
            ("application/json", json!({"choices":[{"message":{"role":"assistant","content":"test"},"finish_reason":"stop"}]}).to_string())
        };
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    }
}
