use super::*;
use orbit_agent::events::AgentEvent;

#[test]
fn replay_preserves_compaction_and_queued_input_semantics() {
    let compact = orbit_store::types::Run {
        images: Vec::new(),
        prompt: "/compact".into(),
        status: "completed".into(),
        elapsed_ms: Some(1234),
        events: vec![],
    };
    assert!(presentation(&compact).compact);
    let queued = orbit_store::types::Run {
        images: Vec::new(),
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
        user_inputs: Vec::new(),
        updates: vec![],
        busy: true,
        model: "test".into(),
        approvals: BTreeMap::new(),
        clock: Some(Clock::new()),
        progress: Vec::new(),
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
    let page = handle.view.lock().await.page(0).unwrap();
    let images = page
        .updates
        .iter()
        .find_map(|update| match update {
            Update::Started {
                prompt,
                presentation,
            } if prompt.is_empty() => Some(presentation.image_assets.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].name, "one.png");
    assert!(handle.has_image(&images[0].path).await);
    assert!(!handle.has_image("/another/session/image.png").await);
    let assets = orbit_tools::image_assets::ImageAssets::for_database(&options.database);
    assert!(
        assets
            .read(Path::new(&images[0].path))
            .unwrap()
            .starts_with(b"\x89PNG")
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let snapshot = handle.store.load(&handle.info.id).await.unwrap();
            if snapshot
                .runs
                .first()
                .is_some_and(|run| run.images == images)
            {
                assert_eq!(super::presentation(&snapshot.runs[0]).image_assets, images);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    handle.command(Command::Cancel).await.unwrap();
}

#[tokio::test]
async fn remembered_file_grant_survives_reopen_and_matches_changed_content() {
    use orbit_tools::approval::{ApprovalPreview, DeletePreview, WritePreview};
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let session =
        crate::session::SessionState::open_in(root.join("sessions.db"), None, "test".into(), root)
            .await
            .unwrap();
    let id = session.id().to_owned();
    let first = ApprovalPreview::Write(WritePreview {
        path: "file.txt".into(),
        before: None,
        after: "first".into(),
    });
    let key = approval_key(&first, root).unwrap();
    session.store.grant_tui_file(&id, &key).await.unwrap();
    drop(session);
    std::fs::write(root.join("file.txt"), "first").unwrap();
    let store = orbit_store::store::Store::open(root.join("sessions.db"))
        .await
        .unwrap();
    let granted = store.tui_file_grants(&id).await.unwrap();
    for preview in [
        ApprovalPreview::Write(WritePreview {
            path: "file.txt".into(),
            before: Some("first".into()),
            after: "second".into(),
        }),
        ApprovalPreview::Delete(DeletePreview {
            path: "file.txt".into(),
            before: "second".into(),
        }),
    ] {
        assert!(granted.contains(&approval_key(&preview, root).unwrap()));
    }
    let other = ApprovalPreview::Write(WritePreview {
        path: "other.txt".into(),
        before: None,
        after: "first".into(),
    });
    assert!(!granted.contains(&approval_key(&other, root).unwrap()));
}

#[tokio::test]
async fn switching_to_a_smaller_context_starts_compaction_in_the_service() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(root.join("config.toml"),r#"
auto_compact=true
[providers.test]
base_url="http://127.0.0.1:1/v1"
api_key="test-only"
auto_refresh_models=false
models=[{id="large",input_context_tokens=64000,max_output_tokens=512},{id="small",input_context_tokens=8192,max_output_tokens=512}]
"#).unwrap();
    let options = crate::WorkspaceOptions {
        model: Some("test::large".into()),
        initial_directory: root.into(),
        auth_directory: root.join("auth"),
        config: root.join("config.toml"),
        database: root.join("sessions.db"),
        allow_mcp_start: false,
        home: None,
        config_home: None,
        instruction_dir: None,
        editor_command: "vi".into(),
    };
    let mut setup = crate::workspace::setup::prepare_workspace(&options, root, None)
        .await
        .unwrap();
    setup
        .session
        .append_context(orbit_model::types::ChatMessage::user(
            "earlier context ".repeat(6000),
        ))
        .await
        .unwrap();
    assert!(setup.resources.budget.auto_compact);
    let handle = Handle::spawn(setup, None);
    handle
        .command(Command::SetModel {
            model: "test::small".into(),
            effort: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3),async {
        loop {
            if handle.view.lock().await.page(0).unwrap().updates.iter().any(|update|matches!(update,Update::Started{presentation,..} if presentation.compact)) { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    handle.command(Command::Cancel).await.unwrap();
}

#[test]
fn progress_snapshots_are_bounded_and_do_not_hide_ledger_events() {
    let mut view = View {
        user_inputs: Vec::new(),
        updates: vec![],
        busy: true,
        model: "test".into(),
        approvals: BTreeMap::new(),
        clock: Some(Clock::new()),
        progress: vec![orbit_protocol::ToolProgress {
            call_id: "large-output".into(),
            stdout: Some("x".repeat(PAGE_BYTES)),
            elapsed_ms: None,
        }],
    };
    view.push(Update::Started {
        prompt: "test".into(),
        presentation: Default::default(),
    });
    let page = view.page(0).unwrap();
    assert_eq!(page.updates.len(), 1);
    assert!(page.tool_progress.is_empty());
    view.progress[0].stdout = Some("bounded output".into());
    let first = view.page(0).unwrap();
    let second = view.page(first.next).unwrap();
    assert_eq!(
        serde_json::to_value(&first.tool_progress).unwrap(),
        serde_json::to_value(&second.tool_progress).unwrap()
    );
    assert_eq!(second.next, first.next);
    assert!(second.updates.is_empty());
    assert!(second.timing.is_some());
}

#[tokio::test]
async fn user_input_shared_session_pages_wait_for_explicit_answer_in_yolo() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("config.toml"),
        r#"
[providers.test]
base_url="http://127.0.0.1:1/v1"
api_key="test-only"
auto_refresh_models=false
models=[{id="test"}]
"#,
    )
    .unwrap();
    let options = crate::WorkspaceOptions {
        model: Some("test::test".into()),
        initial_directory: root.into(),
        auth_directory: root.join("auth"),
        config: root.join("config.toml"),
        database: root.join("db"),
        allow_mcp_start: false,
        home: Some(root.into()),
        config_home: None,
        instruction_dir: None,
        editor_command: "vi".into(),
    };
    let setup = crate::workspace::setup::prepare_service(&options, None)
        .await
        .unwrap();
    let tools = setup.resources.tools.clone();
    let handle = Handle::spawn(setup, None);
    handle
        .command(Command::SetMode { mode: Mode::Yolo })
        .await
        .unwrap();
    handle.command(Command::EnableUserInput).await.unwrap();
    let call = orbit_model::types::ToolCall::function(
        "ask",
        "ask_user",
        serde_json::json!({"questions":[{"id":"q","title":"说明","prompt":"需要什么？","kind":"text"}]}),
    );
    let task = tokio::spawn(async move {
        tools
            .execute(call, tokio_util::sync::CancellationToken::new())
            .await
            .unwrap()
    });
    let record = tokio::time::timeout(std::time::Duration::from_secs(3), async {
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
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!task.is_finished(), "YOLO must still await user answers");
    let second = handle.view.lock().await.page(0).unwrap();
    assert_eq!(second.user_inputs[0].id, record.id);
    let answers = vec![orbit_protocol::user_input::Answer {
        question_id: "q".into(),
        text: "用户回答".into(),
        ..Default::default()
    }];
    handle
        .command(Command::AnswerUserInput {
            id: record.id.clone(),
            answers: Some(answers.clone()),
        })
        .await
        .unwrap();
    let output = task.await.unwrap();
    assert!(output.content.contains("用户回答"));
    handle
        .command(Command::AnswerUserInput {
            id: record.id,
            answers: Some(answers),
        })
        .await
        .unwrap();
    handle.command(Command::Close).await.unwrap();
}
