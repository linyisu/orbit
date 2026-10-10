#[cfg(test)]
use orbit_config::test_support::{TestConfig, TestModel, TestProvider};
mod drafts;
mod permissions;
use orbit_protocol::{Mode, SessionInfo};

use super::*;

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
            TestConfig::new()
                .provider(
                    TestProvider::new("test")
                        .endpoint(url)
                        .key("test-only")
                        .models([TestModel::new("test")]),
                )
                .providers_toml()
                .unwrap(),
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
        let handle = Arc::new(Handle::spawn(setup, None).await.unwrap());
        handle.clients.store(1, Ordering::Release);
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
        self.service
            .dispatch(request, &self.service.options, 0)
            .await
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
async fn steering_receipt_confirms_persistence_while_the_model_is_still_waiting() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = Fixture::new(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
    let session = fixture.handle.info.id.clone();
    fixture
        .request(Request::Start {
            session: session.clone(),
            request_id: "steering-receipt".into(),
            prompt: "initial question".into(),
            images: vec![],
            compact: false,
            model: None,
        })
        .await
        .unwrap();
    // Keep the model HTTP request open without returning any response.
    let (_socket, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let response = tokio::time::timeout(
        Duration::from_secs(3),
        fixture.request(Request::Steer {
            session: session.clone(),
            message: "  new instruction  ".into(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let Response::InputQueued { message_id } = response else {
        panic!("missing durable input receipt");
    };
    let queued = fixture
        .handle
        .store
        .pending_agent_messages(&session)
        .await
        .unwrap();
    assert!(
        queued
            .iter()
            .any(|input| input.id == message_id && input.content == "new instruction")
    );
    let page = fixture.page(0).await;
    assert!(page.busy);
    assert!(!page.updates.iter().any(|update| matches!(update, protocol::Update::Agent(event) if matches!(event.as_ref(), orbit_agent::events::AgentEvent::InputReceived { .. }))));
    fixture.request(Request::Cancel { session }).await.unwrap();
    fixture.wait(|page| !page.busy).await;
}

#[tokio::test]
async fn sent_images_are_readable_only_through_the_owning_session() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let config = root.path().join("config.toml");
    fs::write(
        &config,
        TestConfig::new()
            .provider(
                TestProvider::new("test")
                    .endpoint("http://127.0.0.1:1/v1")
                    .key("test-only")
                    .models([TestModel::new("test").vision(true)]),
            )
            .providers_toml()
            .unwrap(),
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
    let mut handles = Vec::new();
    for _ in 0..2 {
        let setup = crate::workspace::setup::prepare_workspace(&options, &workspace, None)
            .await
            .unwrap();
        handles.push(Arc::new(Handle::spawn(setup, None).await.unwrap()));
    }
    let owner = handles[0].clone();
    let other = handles[1].clone();
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let bytes = png.into_inner();
    owner
        .command(session::Command::Start {
            request_id: "image".into(),
            prompt: "look".into(),
            images: vec![orbit_protocol::ImageUpload {
                name: "clipboard.png".into(),
                data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            }],
            compact: false,
            model: None,
        })
        .await
        .unwrap();
    let path = owner
        .view
        .lock()
        .await
        .page(0)
        .unwrap()
        .updates
        .iter()
        .find_map(|update| match update {
            protocol::Update::Started { presentation, .. } => presentation
                .image_assets
                .first()
                .map(|image| image.path.clone()),
            _ => None,
        })
        .unwrap();
    assert!(!Path::new(&path).starts_with(&workspace));
    let service = Service {
        options,
        sessions: Mutex::new(
            handles
                .into_iter()
                .map(|handle| (handle.info.id.clone(), handle))
                .collect(),
        ),
        opening: Mutex::new(()),
        last_activity: Mutex::new(tokio::time::Instant::now()),
        stop: tokio_util::sync::CancellationToken::new(),
        closing: AtomicBool::new(false),
    };
    let response = service
        .dispatch(
            Request::ReadFile {
                session: owner.info.id.clone(),
                path: path.clone(),
            },
            &service.options,
            0,
        )
        .await
        .unwrap();
    assert!(
        matches!(response, Response::File(data) if base64::engine::general_purpose::STANDARD.decode(&data).unwrap() == bytes)
    );
    assert!(
        service
            .dispatch(
                Request::ReadFile {
                    session: other.info.id.clone(),
                    path,
                },
                &service.options,
                0
            )
            .await
            .is_err()
    );
    owner.command(session::Command::Cancel).await.unwrap();
}
