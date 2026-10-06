use super::*;
use orbit_agent::events::AgentEvent;

#[test]
fn replay_preserves_compaction_and_queued_input_semantics() {
    let compact = orbit_store::types::Run {
        prompt: "/compact".into(),
        status: "completed".into(),
        elapsed_ms: Some(1234),
        events: vec![],
    };
    assert!(presentation(&compact).compact);
    let queued = orbit_store::types::Run {
        prompt: "queued".into(),
        events: vec![
            AgentEvent::InputReceived {
                message_id: 1,
                content: "queued".into(),
            },
            AgentEvent::TurnStarted { turn: 1 },
        ],
        ..compact
    };
    assert!(presentation(&queued).queued_input);
    assert!(!presentation(&queued).compact);
}

#[test]
fn idle_and_live_pages_carry_service_clock_without_changing_the_event_cursor() {
    let mut view = View {
        updates: vec![],
        busy: true,
        model: "test".into(),
        approvals: BTreeMap::new(),
        clock: Some(Clock::new()),
    };
    let page = view.page(0).unwrap();
    assert_eq!(page.next, 0);
    assert!(page.timing.is_some());
    let clock = view.clock.as_mut().unwrap();
    clock.pause();
    assert!(view.page(0).unwrap().timing.unwrap().paused);
    view.clock.as_mut().unwrap().resume();
    assert!(!view.page(0).unwrap().timing.unwrap().paused);
    view.clock = None;
    view.busy = false;
    assert!(view.page(0).unwrap().timing.is_none());
}

#[tokio::test]
async fn image_only_submission_reaches_the_service_and_its_attachment_is_presented() {
    use base64::Engine;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("config.toml"),
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
    let setup = crate::workspace::setup::prepare_workspace(&options, root, None)
        .await
        .unwrap();
    let handle = Handle::spawn(setup, None);
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(1, 1)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    handle
        .command(Command::Start {
            request_id: "image-only".into(),
            prompt: String::new(),
            images: vec![orbit_protocol::ImageUpload {
                name: "one.png".into(),
                data: base64::engine::general_purpose::STANDARD.encode(png.into_inner()),
            }],
            compact: false,
            mode: Mode::Ask,
            model: None,
        })
        .await
        .unwrap();
    assert!(handle.view.lock().await.page(0).unwrap().updates.iter().any(|update| matches!(update,Update::Started{prompt,presentation} if prompt.is_empty() && presentation.images==["one.png"])));
    handle.command(Command::Cancel).await.unwrap();
}
