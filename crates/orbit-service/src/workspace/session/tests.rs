use super::*;
use orbit_agent::events::AgentEvent;
#[cfg(test)]
use orbit_config::test_support::{TestConfig, TestModel, TestProvider};

mod tool_selection_eval;

#[test]
fn replay_preserves_compaction_and_queued_input_semantics() {
    let compact = orbit_store::types::Run {
        files: Vec::new(),
        images: Vec::new(),
        prompt: "/compact".into(),
        status: "completed".into(),
        elapsed_ms: Some(1_234),
        events: vec![],
    };
    assert!(presentation(&compact).compact);
    let queued = orbit_store::types::Run {
        files: Vec::new(),
        images: Vec::new(),
        prompt: "queued".into(),
        events: vec![
            AgentEvent::InputReceived {
                attachments: vec![],
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
        published: 0,
        profile: None,
        images: Default::default(),
        user_inputs: Vec::new(),
        planning: Default::default(),
        updates: Default::default(),
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
    assert!(
        view.page(0)
            .unwrap()
            .timing
            .unwrap()
            .paused
    );
    view.clock.as_mut().unwrap().resume();
    assert!(
        !view
            .page(0)
            .unwrap()
            .timing
            .unwrap()
            .paused
    );
    view.clock = None;
    view.busy = false;
    assert!(view.page(0).unwrap().timing.is_none());
}

#[test]
fn dense_streams_coalesce_without_mutating_published_cursors_or_crossing_barriers() {
    use orbit_model::types::StreamEvent;
    let mut view = View {
        published: 0,
        profile: None,
        images: Default::default(),
        updates: Default::default(),
        busy: true,
        model: "test".into(),
        approvals: BTreeMap::new(),
        clock: None,
        progress: vec![],
        user_inputs: vec![],
        planning: Default::default(),
    };
    let text = |value: &str| Update::agent(AgentEvent::Model(StreamEvent::TextDelta(value.into())));
    for _ in 0..100_000 {
        view.push(text("中\"\n"));
    }
    let fast = view.page(0).unwrap();
    assert!(fast.updates.len() < 16);
    for (update, size) in view.updates.cached() {
        assert_eq!(
            *size,
            serde_json::to_vec(update)
                .unwrap()
                .len()
        );
    }
    let barrier = AgentEvent::InputReceived {
        attachments: vec![],
        message_id: 42,
        content: "steering".into(),
    };
    view.push(text("after publication"));
    view.push(Update::agent(barrier));
    view.push(text("after barrier"));
    let suffix = view.page(fast.next).unwrap();
    assert_eq!(suffix.updates.len(), 3);
    assert!(
        matches!(&suffix.updates[1], Update::Agent(event) if matches!(
            event.as_ref(),
            AgentEvent::InputReceived { message_id: 42, .. }
        ))
    );
    let slow = view.page(0).unwrap();
    let concatenate = |updates: &[Update]| {
        updates
            .iter()
            .filter_map(|update| match update {
                Update::Agent(event) => match event.as_ref() {
                    AgentEvent::Model(StreamEvent::TextDelta(text)) => Some(text.as_str()),
                    _ => None,
                },
                _ => None,
            })
            .collect::<String>()
    };
    assert_eq!(
        concatenate(&slow.updates),
        concatenate(&fast.updates) + &concatenate(&suffix.updates)
    );
    assert_eq!(concatenate(&fast.updates), "中\"\n".repeat(100_000));
    assert!(
        view.page(suffix.next)
            .unwrap()
            .updates
            .is_empty()
    );
}

#[tokio::test]
async fn image_only_submission_reaches_the_service_and_its_attachment_is_presented() {
    use base64::Engine;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("config.toml"),
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
    let handle = Handle::spawn(setup, None)
        .await
        .unwrap();
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(1, 1)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    handle
        .command(Command::Start {
            files: Vec::new(),
            request_id: "image-only".into(),
            prompt: String::new(),
            images: vec![orbit_protocol::ImageUpload {
                name: "one.png".into(),
                data: base64::engine::general_purpose::STANDARD.encode(png.into_inner()),
            }],
            compact: false,
            model: None,
        })
        .await
        .unwrap();
    let page = handle
        .view
        .lock()
        .await
        .page(0)
        .unwrap();
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
    assert!(
        !handle
            .has_image("/another/session/image.png")
            .await
    );
    let assets = orbit_tools::image_assets::ImageAssets::for_database(&options.database);
    assert!(
        assets
            .read(Path::new(&images[0].path))
            .unwrap()
            .starts_with(b"\x89PNG")
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let snapshot = handle
                .store
                .load(&handle.info.id)
                .await
                .unwrap();
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
    handle
        .command(Command::Cancel)
        .await
        .unwrap();
}

#[tokio::test]
async fn remembered_file_grant_survives_reopen_and_matches_changed_content() {
    use orbit_tools::approval::{ApprovalPreview, DeletePreview, WritePreview};
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut session =
        crate::session::SessionState::open_in(root.join("sessions.db"), None, "test".into(), root)
            .await
            .unwrap();
    session
        .begin("edit file".into(), "test".into())
        .await
        .unwrap();
    session
        .finish(orbit_store::types::RunCompletion::Cancelled)
        .await
        .unwrap();
    let id = session.id().to_owned();
    let first = ApprovalPreview::Write(WritePreview {
        path: "file.txt".into(),
        before: None,
        after: "first".into(),
    });
    let key = approval_key(&first, root).unwrap();
    session
        .store
        .grant_tui_file(&id, &key)
        .await
        .unwrap();
    drop(session);
    std::fs::write(root.join("file.txt"), "first").unwrap();
    let store = orbit_store::store::Store::open(root.join("sessions.db"))
        .await
        .unwrap();
    let granted = store
        .tui_file_grants(&id)
        .await
        .unwrap();
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

async fn smaller_context_setup(root: &Path, auto_compact: bool) -> crate::PreparedWorkspace {
    std::fs::create_dir(root.join(".git")).unwrap();
    std::fs::write(
        root.join("config.toml"),
        format!(
            "auto_compact={auto_compact}\n{}",
            TestConfig::new()
                .provider(
                    TestProvider::new("test")
                        .endpoint("http://127.0.0.1:1/v1")
                        .key("test-only")
                        .models([
                            TestModel::new("large")
                                .input_context(128_000)
                                .output(512),
                            TestModel::new("small")
                                .input_context(32_768)
                                .output(512)
                        ])
                )
                .providers_toml()
                .unwrap()
        ),
    )
    .unwrap();
    let options = crate::WorkspaceOptions {
        model: Some("test::large".into()),
        initial_directory: root.into(),
        auth_directory: root.join("auth"),
        config: root.join("config.toml"),
        database: root.join("sessions.db"),
        allow_mcp_start: false,
        home: Some(root.into()),
        config_home: Some(root.into()),
        instruction_dir: Some(root.join("instructions")),
        editor_command: "vi".into(),
    };
    let mut setup = crate::workspace::setup::prepare_workspace(&options, root, None)
        .await
        .unwrap();
    for message in [
        orbit_model::types::ChatMessage::user("Inspect the generated subsystem."),
        orbit_model::types::ChatMessage::assistant("archived evidence ".repeat(6_000)),
        orbit_model::types::ChatMessage::user("Continue from the recorded findings."),
    ] {
        setup
            .session
            .append_context(message)
            .await
            .unwrap();
    }
    setup
}

#[tokio::test]
async fn switching_to_a_smaller_context_starts_compaction_in_the_service() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let setup = smaller_context_setup(root, true).await;
    assert!(setup.resources.budget.auto_compact);
    let handle = Handle::spawn(setup, None)
        .await
        .unwrap();
    handle
        .command(Command::SetModel {
            model: "test::small".into(),
            effort: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let updates = handle
                .view
                .lock()
                .await
                .page(0)
                .unwrap()
                .updates;
            let started = updates.iter().any(|update| {
                matches!(
                    update,
                    Update::Started { presentation, .. } if presentation.compact
                )
            });
            let announced = updates.iter().any(|update| {
                matches!(
                    update,
                    Update::Agent(event)
                        if matches!(event.as_ref(), AgentEvent::CompactionStarted)
                )
            });
            if started && announced {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    handle
        .command(Command::Cancel)
        .await
        .unwrap();
}

#[tokio::test]
async fn manual_compaction_after_a_smaller_model_switch_preserves_history_and_fits_the_budget() {
    use orbit_agent::{memory::TaskSummary, model::MockModel};
    use orbit_model::types::{ChatCompletion, ChatMessage};

    let directory = tempfile::tempdir().unwrap();
    let mut setup = smaller_context_setup(directory.path(), false).await;
    let final_answer = ChatMessage::assistant("generated completion report ".repeat(4_000));
    setup
        .session
        .append_context(final_answer.clone())
        .await
        .unwrap();
    let summary = serde_json::to_string(&TaskSummary {
        goal: "Continue the generated investigation using archived evidence.".into(),
        constraints: vec![],
        decisions: vec![],
        completed: vec![],
        remaining: vec![],
        verification: vec![],
        uncertainty: vec![],
    })
    .unwrap();
    let backend = MockModel::new((0..256).map(|_| ChatCompletion {
        id: None,
        model: None,
        message: ChatMessage::assistant(summary.clone()),
        finish_reason: Some("stop".into()),
        usage: None,
    }));
    let compaction = &mut setup.resources.budget.compaction;
    compaction.backend = Some(Arc::new(backend.clone()));
    compaction.settings.max_input_tokens = 2_048;
    compaction.settings.max_output_tokens = 512;
    let store = setup.session.store.clone();
    let original = setup.session.snapshot.history.clone();
    let tools = setup.resources.tools.definitions();
    assert!(
        orbit_agent::context::estimate_tokens(&original, &tools)
            < setup
                .resources
                .budget
                .input_context_tokens
    );
    let handle = Handle::spawn(setup, None)
        .await
        .unwrap();
    handle
        .command(Command::SetModel {
            model: "test::small".into(),
            effort: None,
        })
        .await
        .unwrap();
    let info = handle.current_info().await;
    assert!(orbit_agent::context::estimate_tokens(&original, &tools) > info.input_context_tokens);
    assert!(
        orbit_agent::context::estimate_tokens(&[final_answer], &[]) > info.input_context_tokens
    );
    handle
        .command(Command::Start {
            request_id: orbit_protocol::request_id(),
            prompt: String::new(),
            images: vec![],
            files: vec![],
            compact: true,
            model: None,
        })
        .await
        .unwrap();
    let page = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let page = handle
                .view
                .lock()
                .await
                .page(0)
                .unwrap();
            if !page.busy {
                break page;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        page.updates
            .iter()
            .any(|update| matches!(update,
                Update::Agent(event) if matches!(event.as_ref(), AgentEvent::ContextCompacted {
                    before_tokens, after_tokens, ..
                } if after_tokens < before_tokens)
            )),
        "{:#?}",
        page.updates
    );
    let snapshot = store.load(&info.id).await.unwrap();
    let run = snapshot.runs.last().unwrap();
    assert_eq!(run.prompt, "/compact");
    assert_eq!(run.status, "completed");
    assert!(presentation(run).compact);
    assert!(
        orbit_agent::context::estimate_tokens(&snapshot.history, &tools)
            <= info.input_context_tokens
    );
    assert!(backend.requests().len() > 1);
    for request in backend.requests() {
        assert!(orbit_agent::context::estimate_tokens(&request.messages, &[]) <= 2_048);
    }
    let archived = store
        .load_context_archive(&info.id, 1)
        .await
        .unwrap();
    assert!(
        original
            .iter()
            .all(|message| archived.contains(message))
    );
    handle
        .command(Command::Close)
        .await
        .unwrap();
}

#[tokio::test]
async fn oversized_tool_catalog_reports_the_budget_and_preserves_history() {
    let directory = tempfile::tempdir().unwrap();
    let mut setup = smaller_context_setup(directory.path(), false).await;
    setup
        .resources
        .tools
        .register(orbit_agent::tool::ClosureTool::new(
            "generated_catalog",
            "generated tool specification ".repeat(5_000),
            serde_json::json!({"type": "object"}),
            |_, _| async { Ok("unused".into()) },
        ));
    for (model, actual) in [("test::small", 400), ("test::large", 100)] {
        setup
            .session
            .snapshot
            .calibration
            .entry(model.into())
            .or_default()
            .observe(200, Some(actual));
    }
    let original = setup.session.snapshot.history.clone();
    let estimated =
        orbit_agent::context::estimate_tokens(&original, &setup.resources.tools.definitions());
    let store = setup.session.store.clone();
    let handle = Handle::spawn(setup, None)
        .await
        .unwrap();
    handle
        .command(Command::SetModel {
            model: "test::small".into(),
            effort: None,
        })
        .await
        .unwrap();
    let info = handle.current_info().await;
    assert_eq!(info.context_used_tokens, Some(estimated * 2));
    handle
        .command(Command::Start {
            request_id: orbit_protocol::request_id(),
            prompt: String::new(),
            images: vec![],
            files: vec![],
            compact: true,
            model: None,
        })
        .await
        .unwrap();
    let page = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let page = handle
                .view
                .lock()
                .await
                .page(0)
                .unwrap();
            if !page.busy {
                break page;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        page.updates
            .iter()
            .any(|update| matches!(update,
                Update::Agent(event) if matches!(event.as_ref(),
                    AgentEvent::Failed { error }
                        if error.contains("tool definitions:") && error.contains("input budget is")
                )
            ))
    );
    let snapshot = store.load(&info.id).await.unwrap();
    assert_eq!(snapshot.history, original);
    assert_eq!(snapshot.runs.last().unwrap().status, "failed");
    handle
        .command(Command::SetModel {
            model: "test::large".into(),
            effort: None,
        })
        .await
        .unwrap();
    let refreshed = handle.current_info().await;
    assert_eq!(refreshed.context_used_tokens, Some(estimated.div_ceil(2)));
    assert!(refreshed.context_used_tokens.unwrap() < refreshed.input_context_tokens);
    let preserved = store.load(&info.id).await.unwrap();
    assert_eq!(preserved.history, original);
    assert_eq!(preserved.runs.len(), snapshot.runs.len());
    assert_eq!(preserved.runs.last().unwrap().status, "failed");
    handle
        .command(Command::Close)
        .await
        .unwrap();
}

#[test]
fn progress_snapshots_are_bounded_and_do_not_hide_ledger_events() {
    let mut view = View {
        published: 0,
        profile: None,
        images: Default::default(),
        user_inputs: Vec::new(),
        planning: Default::default(),
        updates: Default::default(),
        busy: true,
        model: "test".into(),
        approvals: BTreeMap::new(),
        clock: Some(Clock::new()),
        progress: vec![orbit_protocol::ToolProgress {
            call_id: "large-output".into(),
            stdout: Some("x".repeat(PAGE_BYTES)),
            stderr: None,
            command: None,
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
    assert_eq!(first.tool_progress.len(), 1);
    assert_eq!(
        first.tool_progress[0].stdout.as_deref(),
        Some("bounded output")
    );
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
        TestConfig::new()
            .provider(
                TestProvider::new("test")
                    .endpoint("http://127.0.0.1:1/v1")
                    .key("test-only")
                    .models([TestModel::new("test")]),
            )
            .providers_toml()
            .unwrap(),
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
    let mut setup = crate::workspace::setup::prepare_service(&options, None)
        .await
        .unwrap();
    setup.start_session().await.unwrap();
    let tools = setup.resources.tools.clone();
    let handle = Handle::spawn(setup, None)
        .await
        .unwrap();
    handle
        .command(Command::SetMode { mode: Mode::Yolo })
        .await
        .unwrap();
    handle
        .command(Command::EnableUserInput)
        .await
        .unwrap();
    let call = orbit_model::types::ToolCall::function(
        "ask",
        "AskUser",
        serde_json::json!({
            "questions": [{
                "id": "q",
                "title": "说明",
                "prompt": "需要什么？",
                "kind": "text"
            }]
        }),
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
    let second = handle
        .view
        .lock()
        .await
        .page(0)
        .unwrap();
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
    handle
        .command(Command::Close)
        .await
        .unwrap();
}

#[tokio::test]
async fn planning_tool_review_and_client_decisions_share_authoritative_session_state() {
    use orbit_model::types::ToolCall;
    use orbit_protocol::plan::{Action, Status, WorkMode};
    use tokio_util::sync::CancellationToken;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("config.toml"),
        TestConfig::new()
            .provider(
                TestProvider::new("test")
                    .endpoint("http://127.0.0.1:1/v1")
                    .key("test")
                    .models([TestModel::new("test")]),
            )
            .providers_toml()
            .unwrap(),
    )
    .unwrap();
    let options = crate::WorkspaceOptions {
        model: Some("test::test".into()),
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
    setup.start_session().await.unwrap();
    let tools = setup.resources.tools.clone();
    let handle = Handle::spawn(setup, None)
        .await
        .unwrap();
    handle
        .command(Command::Plan {
            action: Action::Enable,
        })
        .await
        .unwrap();
    tools
        .execute(
            ToolCall::function("enter", "EnterPlan", serde_json::json!({})),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while handle
            .view
            .lock()
            .await
            .page(0)
            .unwrap()
            .planning
            .mode
            != WorkMode::Plan
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let generation = handle
        .view
        .lock()
        .await
        .page(0)
        .unwrap()
        .planning
        .generation;
    assert!(
        tools
            .execute(
                ToolCall::function(
                    "write",
                    "WriteFile",
                    serde_json::json!({
                        "path": "should-not-exist",
                        "content": "forbidden"
                    })
                ),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert!(!root.join("should-not-exist").exists());
    let result = tools
        .execute(
            ToolCall::function(
                "draft",
                "UpdatePlan",
                serde_json::json!({
                    "generation": generation,
                    "title": "Feature plan",
                    "markdown": "Investigate, implement and verify.",
                    "steps": ["Implement", "Verify"]
                }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let state: orbit_protocol::plan::State = serde_json::from_str(&result.content).unwrap();
    let submission = tokio::spawn(async move {
        tools
            .execute(
                ToolCall::function(
                    "review",
                    "SubmitPlan",
                    serde_json::json!({
                        "generation": state.generation
                    }),
                ),
                CancellationToken::new(),
            )
            .await
    });
    let generation = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let state = handle
                .view
                .lock()
                .await
                .page(0)
                .unwrap()
                .planning;
            if state
                .plan
                .as_ref()
                .is_some_and(|p| p.status == Status::Review)
            {
                break state.generation;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (first, second) = tokio::join!(
        handle.command(Command::Plan {
            action: Action::Approve { generation }
        }),
        handle.command(Command::Plan {
            action: Action::Approve { generation }
        })
    );
    first.unwrap();
    second.unwrap();
    let result = submission.await.unwrap().unwrap();
    let state: orbit_protocol::plan::State = serde_json::from_str(&result.content).unwrap();
    assert_eq!(state.mode, WorkMode::Build);
    assert_eq!(state.plan.unwrap().task_ids.len(), 2);
    assert_eq!(
        handle
            .view
            .lock()
            .await
            .page(0)
            .unwrap()
            .planning
            .mode,
        WorkMode::Build
    );
    handle
        .command(Command::Close)
        .await
        .unwrap();
}

#[tokio::test]
async fn file_only_submission_is_persisted_and_presented_without_vision() {
    use base64::Engine;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("config.toml"),
        TestConfig::new()
            .provider(
                TestProvider::new("test")
                    .endpoint("http://127.0.0.1:1/v1")
                    .key("test-only")
                    .models([TestModel::new("test").vision(false)]),
            )
            .providers_toml()
            .unwrap(),
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
    let handle = Handle::spawn(setup, None)
        .await
        .unwrap();
    let bytes = b"section = \"synthetic\"\ncount = 7\n";
    handle
        .command(Command::Start {
            files: vec![orbit_protocol::FileUpload {
                name: "report.toml".into(),
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
            }],
            request_id: "file-only".into(),
            prompt: String::new(),
            images: vec![],
            compact: false,
            model: None,
        })
        .await
        .unwrap();
    let page = handle
        .view
        .lock()
        .await
        .page(0)
        .unwrap();
    let images = page
        .updates
        .iter()
        .find_map(|update| match update {
            Update::Started {
                prompt,
                presentation,
            } if prompt.is_empty() => Some(presentation.file_assets.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].name, "report.toml");
    assert!(handle.has_image(&images[0].path).await);
    assert!(
        !handle
            .has_image("/another/session/image.png")
            .await
    );
    let assets = orbit_tools::image_assets::ImageAssets::for_database(&options.database);
    assert!(
        assets
            .read(Path::new(&images[0].path))
            .unwrap()
            == bytes
    );
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let snapshot = handle
                .store
                .load(&handle.info.id)
                .await
                .unwrap();
            if snapshot
                .runs
                .first()
                .is_some_and(|run| run.files == images)
            {
                assert_eq!(super::presentation(&snapshot.runs[0]).file_assets, images);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    handle
        .command(Command::Cancel)
        .await
        .unwrap();
}
