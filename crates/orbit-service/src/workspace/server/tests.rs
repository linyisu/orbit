mod permissions;

use super::*;

#[tokio::test]
async fn sent_images_are_readable_only_through_the_owning_session() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let config = root.path().join("config.toml");
    fs::write(
        &config,
        r#"
[providers.test]
base_url="http://127.0.0.1:1/v1"
api_key="test-only"
auto_refresh_models=false
models=[{id="test",supports_vision=true}]
"#,
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
        handles.push(Arc::new(Handle::spawn(setup, None)));
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
                &service.options
            )
            .await
            .is_err()
    );
    owner.command(session::Command::Cancel).await.unwrap();
}
