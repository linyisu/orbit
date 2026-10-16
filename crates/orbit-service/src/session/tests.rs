use super::*;
use orbit_agent::events::AgentEvent;
use std::time::Duration;

#[tokio::test]
async fn generated_title_follows_its_session_and_manual_rename_wins() {
    let tmp = tempfile::tempdir().unwrap();
    let mut current = SessionState::open_in(
        tmp.path().join("sessions.db"),
        None,
        "model".into(),
        tmp.path(),
    )
    .await
    .unwrap();
    current
        .begin("first question".into(), "model".into())
        .await
        .unwrap();
    current
        .finish(orbit_store::types::RunCompletion::Cancelled)
        .await
        .unwrap();
    let original_id = current.id().to_owned();
    current.title_task = Some(crate::session::title::TitleTask::ready("解析器修复").await);
    let next = current
        .prepare_new("model".into())
        .await
        .unwrap();

    let mut parked = current.swap_resume(next);
    assert!(parked.poll_title().await.unwrap());
    assert_eq!(parked.snapshot.session.title, "解析器修复");
    assert!(
        current
            .snapshot
            .session
            .title
            .is_empty()
    );
    assert_eq!(
        current
            .store
            .session(&original_id)
            .await
            .unwrap()
            .title,
        "解析器修复"
    );
    assert!(parked.snapshot.history.is_empty());

    current.title_task = Some(crate::session::title::TitleTask::ready("自动生成标题").await);
    current
        .rename("手动标题".into())
        .await
        .unwrap();
    assert!(!current.poll_metadata().await.unwrap());
    assert!(
        current
            .store
            .session(current.id())
            .await
            .is_err()
    );
    current
        .begin("next question".into(), "model".into())
        .await
        .unwrap();
    current
        .finish(orbit_store::types::RunCompletion::Cancelled)
        .await
        .unwrap();
    assert_eq!(
        current
            .store
            .session(current.id())
            .await
            .unwrap()
            .title,
        "手动标题"
    );
}

#[tokio::test]
async fn prepared_resume_is_atomic_and_releases_lease_when_cancelled() {
    let tmp = tempfile::tempdir().unwrap();
    let mut current = SessionState::open(tmp.path().join("sessions.db"), None, "model".into())
        .await
        .unwrap();
    current
        .begin("question".into(), "model".into())
        .await
        .unwrap();
    current
        .finish(orbit_store::types::RunCompletion::Cancelled)
        .await
        .unwrap();
    let target = current.id().to_owned();
    current
        .new_session("model".into())
        .await
        .unwrap();
    current
        .begin("another question".into(), "model".into())
        .await
        .unwrap();
    current
        .finish(orbit_store::types::RunCompletion::Cancelled)
        .await
        .unwrap();
    let original = current.id().to_owned();
    let lease = current
        .store
        .acquire(&target)
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        SessionState::prepare_resume(
            current.store.clone(),
            current
                .snapshot
                .session
                .project_id
                .clone(),
            target.clone(),
        ),
    )
    .await
    .unwrap();
    let error = match result {
        Err(e) => e,
        Ok(_) => panic!("locked session was opened"),
    };

    assert!(
        error
            .to_string()
            .contains("写入锁正在被占用")
    );
    assert_eq!(current.id(), original);

    drop(lease);
    let prepared = SessionState::prepare_resume(
        current.store.clone(),
        current
            .snapshot
            .session
            .project_id
            .clone(),
        target.clone(),
    )
    .await
    .unwrap();

    assert_eq!(current.id(), original);
    assert!(
        current
            .store
            .acquire(&target)
            .await
            .is_err()
    );

    drop(prepared);

    // Cancelling a prepared switch releases the destination without
    // releasing the writer for the conversation still visible in the UI.
    assert!(
        current
            .store
            .acquire(&target)
            .await
            .is_ok()
    );
    assert!(
        current
            .store
            .acquire(&original)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn imports_images_into_the_orbit_asset_directory_by_content_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let state = SessionState::open(tmp.path().join("sessions.db"), None, "model".into())
        .await
        .unwrap();
    let source = tmp.path().join("capture.png");
    image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]))
        .save(&source)
        .unwrap();
    let first = state.import_image(&source).unwrap();
    let second = state.import_image(&source).unwrap();

    assert_eq!(first, second);

    let Attachment::Image {
        path, mime_type, ..
    } = first
    else {
        panic!("uploaded image must be persistent")
    };

    assert_eq!(mime_type, "image/png");
    assert!(path.starts_with(tmp.path().join("sessions.attachments")));

    let clipboard = state
        .import_clipboard_image(1, 1, vec![10, 20, 30, 255])
        .unwrap();
    let Attachment::Image {
        name,
        path,
        mime_type,
        ..
    } = clipboard
    else {
        panic!("uploaded image must be persistent")
    };

    assert!(name.starts_with("clipboard-") && name.ends_with(".png"));
    assert_eq!(mime_type, "image/png");
    assert!(path.starts_with(tmp.path().join("sessions.attachments")));
}

#[tokio::test]
async fn restored_completed_run_keeps_its_persisted_elapsed_time() {
    let tmp = tempfile::tempdir().unwrap();
    let database = tmp.path().join("sessions.db");
    let mut state = SessionState::open(database.clone(), None, "model".into())
        .await
        .unwrap();
    let id = state.id().to_owned();
    state
        .begin("question".into(), "model".into())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    state
        .finish(orbit_store::types::RunCompletion::Completed {
            history: vec![
                ChatMessage::user("question"),
                ChatMessage::assistant("answer"),
            ],
        })
        .await
        .unwrap();
    drop(state);

    let restored = SessionState::open(database, Some(id), "model".into())
        .await
        .unwrap();

    assert!(
        restored.snapshot.runs[0]
            .elapsed_ms
            .is_some_and(|ms| ms >= 5)
    );
}

#[tokio::test]
async fn resumed_session_keeps_its_selected_model_profile() {
    let tmp = tempfile::tempdir().unwrap();
    let database = tmp.path().join("sessions.db");
    let mut state = SessionState::open(database.clone(), None, "qwen::default".into())
        .await
        .unwrap();
    let id = state.id().to_owned();
    state
        .set_model_profile("deepseek::flash".into(), Some(ReasoningEffort::High))
        .await
        .unwrap();
    state
        .begin("question".into(), "deepseek::flash".into())
        .await
        .unwrap();
    state
        .finish(orbit_store::types::RunCompletion::Cancelled)
        .await
        .unwrap();
    drop(state);

    let restored = SessionState::open(database, Some(id), "qwen::default".into())
        .await
        .unwrap();

    assert_eq!(
        restored.saved_model_profile(),
        ("deepseek::flash", Some(ReasoningEffort::High))
    );
}

#[tokio::test]
async fn renamed_session_keeps_its_identity_and_title_after_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let database = tmp.path().join("sessions.db");
    let mut state = SessionState::open(database.clone(), None, "model".into())
        .await
        .unwrap();
    let id = state.id().to_owned();
    state
        .rename("解析器重构".into())
        .await
        .unwrap();

    assert_eq!(state.snapshot.session.title, "解析器重构");
    assert!(state.store.session(&id).await.is_err());
    state
        .append_context(ChatMessage::system("draft context"))
        .await
        .unwrap();
    assert!(state.store.session(&id).await.is_err());
    state
        .begin("start conversation".into(), "model".into())
        .await
        .unwrap();
    state
        .finish(orbit_store::types::RunCompletion::Cancelled)
        .await
        .unwrap();
    drop(state);

    let restored = SessionState::open(database, Some(id.clone()), "model".into())
        .await
        .unwrap();

    assert_eq!(restored.id(), id);
    assert_eq!(restored.snapshot.session.title, "解析器重构");
    assert_eq!(
        restored.snapshot.history,
        vec![ChatMessage::system("draft context")]
    );
}

#[tokio::test]
async fn switching_restores_context_preserves_old_session_and_checks_scope_and_locks() {
    let tmp = tempfile::tempdir().unwrap();
    let mut state = SessionState::open(tmp.path().join("sessions.db"), None, "model".into())
        .await
        .unwrap();
    let first = state.id().to_owned();
    let command_output = state.command_output.clone();
    assert!(command_output.lease().is_none());
    let history = vec![
        ChatMessage::user("first question"),
        ChatMessage::assistant("first answer"),
    ];
    state
        .begin("first question".into(), "model".into())
        .await
        .unwrap();

    assert!(state.resume("missing").await.is_err());
    let log = command_output
        .create_log()
        .await
        .unwrap();
    let first_command_output = command_output
        .lease()
        .unwrap()
        .path()
        .to_owned();
    drop(log);

    state
        .finish(orbit_store::types::RunCompletion::Completed {
            history: history.clone(),
        })
        .await
        .unwrap();
    assert!(command_output.lease().is_some());
    assert!(first_command_output.exists());
    state.resume(&first).await.unwrap();

    assert_eq!(state.snapshot.history, history);

    state
        .new_session("model".into())
        .await
        .unwrap();
    let second = state.id().to_owned();

    assert_ne!(first, second);
    assert!(command_output.lease().is_some());
    assert!(state.snapshot.history.is_empty());

    let foreign_path = tmp.path().join("foreign");
    std::fs::create_dir(&foreign_path).unwrap();
    let foreign = state
        .store
        .create(
            foreign_path
                .to_string_lossy()
                .into_owned(),
            "model".into(),
        )
        .await
        .unwrap();
    assert!(
        state
            .resume(foreign.id())
            .await
            .is_err()
    );
    assert_eq!(
        state
            .resumable()
            .await
            .unwrap()
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        vec![first.as_str()]
    );

    let locked = state
        .store
        .acquire(&first)
        .await
        .unwrap();

    assert!(state.resume(&first).await.is_err());
    assert_eq!(state.id(), second);
    assert!(state.resume("missing").await.is_err());
    assert_eq!(state.id(), second);

    drop(locked);
    state.resume(&first).await.unwrap();

    assert!(command_output.lease().is_some());
    assert_eq!(state.snapshot.history, history);
    assert_eq!(state.snapshot.runs.len(), 1);
    assert!(state.store.load(&second).await.is_err());
    assert!(
        state
            .store
            .acquire(&second)
            .await
            .is_err()
    );

    drop(state);
    let reopened = SessionState::open(tmp.path().join("sessions.db"), Some(first), "model".into())
        .await
        .unwrap();

    assert!(
        reopened
            .command_output
            .lease()
            .is_some()
    );
    assert!(
        reopened
            .command_output
            .create_log()
            .await
            .is_err()
    );
}

#[tokio::test]
async fn failed_cancelled_and_dropped_tasks_preserve_session_command_logs() {
    let temporary = tempfile::tempdir().unwrap();
    let database = temporary.path().join("sessions.db");
    let mut session = SessionState::open(database.clone(), None, "model".into())
        .await
        .unwrap();
    for completion in [RunCompletion::Failed, RunCompletion::Cancelled] {
        session
            .begin("run a command".into(), "model".into())
            .await
            .unwrap();
        let log = session
            .command_output
            .create_log()
            .await
            .unwrap();
        let directory = session
            .command_output
            .lease()
            .unwrap()
            .path()
            .to_owned();
        drop(log);
        session
            .finish(completion)
            .await
            .unwrap();
        assert!(session.command_output.lease().is_some());
        assert!(directory.exists());
    }
    assert!(
        database
            .with_extension("command-output")
            .exists()
    );
    session
        .begin("interrupted".into(), "model".into())
        .await
        .unwrap();
    let outputs = session.command_output.clone();
    let log = outputs.create_log().await.unwrap();
    let directory = outputs
        .lease()
        .unwrap()
        .path()
        .to_owned();
    drop(log);
    drop(session);
    assert!(outputs.lease().is_some());
    assert!(directory.exists());
}

#[tokio::test]
async fn calibration_restores_per_profile_without_cross_provider_contamination() {
    let tmp = tempfile::tempdir().unwrap();
    let mut session = SessionState::open(tmp.path().join("sessions.db"), None, "a::model".into())
        .await
        .unwrap();
    let id = session.id().to_owned();
    session
        .begin("hello".into(), "a::model".into())
        .await
        .unwrap();
    let journal = session.execution_journal().unwrap();

    for (profile, actual) in [("a::model", 200), ("b::model", 500)] {
        journal.record(AgentEvent::RequestMetrics {
            profile: profile.into(),
            estimated_input: 100,
            retries: Some(0),
            elapsed_ms: 10,
            usage: Some(orbit_model::types::Usage {
                prompt_tokens: Some(actual),
                completion_tokens: Some(1),
                total_tokens: None,
                ..Default::default()
            }),
        });
    }

    session
        .finish(orbit_store::types::RunCompletion::Completed {
            history: vec![ChatMessage::user("hello"), ChatMessage::assistant("hi")],
        })
        .await
        .unwrap();
    drop(session);
    let session = SessionState::open(tmp.path().join("sessions.db"), Some(id), "a::model".into())
        .await
        .unwrap();

    assert_eq!(
        session
            .calibration("a::model")
            .estimate(100),
        200
    );
    assert_eq!(
        session
            .calibration("b::model")
            .estimate(100),
        500
    );
}

#[tokio::test]
async fn test_session_switch_and_resume_clears_session_grants() {
    let tmp = tempfile::tempdir().unwrap();
    let ws_dir = tempfile::tempdir().unwrap();
    let workspace = orbit_runtime::filesystem::Workspace::new(ws_dir.path()).unwrap();
    let mut session = SessionState::open(tmp.path().join("sessions.db"), None, "a::model".into())
        .await
        .unwrap();
    session.audit_workspace = Some(workspace.clone());

    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("file.txt");
    std::fs::write(&file, "data").unwrap();
    let res = orbit_runtime::filesystem::ResolvedPath::resolve(&file, None).unwrap();
    workspace
        .policy()
        .record_grant(orbit_runtime::policy::FilesystemGrant::read_only(
            orbit_runtime::capability::EnvironmentId::Local,
            res.clone(),
        ));

    assert_eq!(
        workspace
            .policy()
            .session_grants()
            .read()
            .unwrap()
            .grants()
            .len(),
        1
    );

    session
        .new_session("a::model".into())
        .await
        .unwrap();

    assert_eq!(
        workspace
            .policy()
            .session_grants()
            .read()
            .unwrap()
            .grants()
            .len(),
        0
    );

    workspace
        .policy()
        .record_grant(orbit_runtime::policy::FilesystemGrant::read_only(
            orbit_runtime::capability::EnvironmentId::Local,
            res,
        ));

    assert_eq!(
        workspace
            .policy()
            .session_grants()
            .read()
            .unwrap()
            .grants()
            .len(),
        1
    );

    let writer2 = session
        .store
        .create(
            session
                .snapshot
                .session
                .workspace
                .clone(),
            "a::model".into(),
        )
        .await
        .unwrap();
    let second_id = writer2.id().to_owned();
    drop(writer2);

    let prepared = SessionState::prepare_resume(
        session.store.clone(),
        session
            .store
            .session(&second_id)
            .await
            .unwrap()
            .project_id,
        second_id,
    )
    .await
    .unwrap();

    session.install_resume(prepared);

    assert_eq!(
        workspace
            .policy()
            .session_grants()
            .read()
            .unwrap()
            .grants()
            .len(),
        0
    );
}

#[tokio::test]
async fn vision_to_text_to_vision_roundtrip_preserves_original_history() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("orbit.db");

    let mut session = SessionState::open(db_path.clone(), None, "test::vision-model".into())
        .await
        .unwrap();
    session
        .begin("look at image".into(), "test::vision-model".into())
        .await
        .unwrap();
    let history = vec![
        ChatMessage::user("look at image"),
        ChatMessage::user_with_attachments(
            "analyze this",
            vec![orbit_model::types::Attachment::Image {
                lease: None,
                name: "photo.png".into(),
                mime_type: "image/png".into(),
                path: "/path/to/photo.png".into(),
            }],
        ),
        ChatMessage::assistant("I see a mountain in photo.png"),
    ];

    session
        .finish(orbit_store::types::RunCompletion::Completed {
            history: history.clone(),
        })
        .await
        .unwrap();
    let id = session.id().to_owned();
    drop(session);

    let mut session_text =
        SessionState::open(db_path.clone(), Some(id.clone()), "test::text-model".into())
            .await
            .unwrap();
    assert_eq!(session_text.snapshot.history, history);
    let projected = orbit_agent::model_messages(&session_text.snapshot.history, false);

    assert_eq!(projected.len(), 3);

    let ChatMessage::User {
        attachments,
        content,
    } = &projected[1]
    else {
        panic!()
    };

    assert!(attachments.is_empty());
    assert!(
        content.contains("[Image omitted: current model does not support image input: photo.png]")
    );

    session_text
        .begin("what was that mountain?".into(), "test::text-model".into())
        .await
        .unwrap();
    let mut history_turn2 = session_text.snapshot.history.clone();
    history_turn2.push(ChatMessage::user("what was that mountain?"));
    history_turn2.push(ChatMessage::assistant("It was Mount Fuji."));
    session_text
        .finish(orbit_store::types::RunCompletion::Completed {
            history: history_turn2.clone(),
        })
        .await
        .unwrap();
    drop(session_text);

    let session_vision = SessionState::open(
        db_path.clone(),
        Some(id.clone()),
        "test::vision-model".into(),
    )
    .await
    .unwrap();

    assert_eq!(session_vision.snapshot.history, history_turn2);
    assert_eq!(
        orbit_agent::model_messages(&session_vision.snapshot.history, true),
        history_turn2
    );
}

struct FailFirstFlush(std::sync::atomic::AtomicBool);

impl orbit_agent::checkpoint::ExecutionJournal for FailFirstFlush {
    fn record(&self, _event: orbit_agent::events::AgentEvent) {}

    fn flush(&self) -> orbit_agent::checkpoint::CheckpointFuture<'_> {
        Box::pin(async {
            if self
                .0
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                Err(orbit_agent::error::AgentError::Checkpoint(
                    "injected flush failure".into(),
                ))
            } else {
                Ok(())
            }
        })
    }

    fn save<'a>(
        &'a self,
        _checkpoint: orbit_agent::checkpoint::ExecutionCheckpoint,
        _event: Option<orbit_agent::events::AgentEvent>,
    ) -> orbit_agent::checkpoint::CheckpointFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn failed_terminal_flush_retries_receipt_before_a_new_run() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = SessionState::open_in(
        directory.path().join("sessions.db"),
        None,
        "model".into(),
        directory.path(),
    )
    .await
    .unwrap();
    session
        .begin("first request".into(), "model".into())
        .await
        .unwrap();
    let RunState::Running { journal, .. } = &mut session.run else {
        panic!("running")
    };
    *journal = Some(std::sync::Arc::new(FailFirstFlush(true.into())));
    let history = vec![
        ChatMessage::user("first request"),
        ChatMessage::assistant("first result"),
    ];
    assert!(
        session
            .finish(RunCompletion::Completed {
                history: history.clone()
            })
            .await
            .is_err()
    );
    assert!(matches!(session.run, RunState::Finalizing { .. }));
    assert!(session.command_output.lease().is_none());
    session
        .begin("second request".into(), "model".into())
        .await
        .unwrap();
    let snapshot = session
        .store
        .load(session.id())
        .await
        .unwrap();
    assert_eq!(snapshot.runs.len(), 2);
    assert_eq!(snapshot.runs[0].status, "completed");
    assert_eq!(snapshot.history, history);
    session
        .finish(RunCompletion::Cancelled)
        .await
        .unwrap();
}

#[tokio::test]
async fn failed_terminal_transaction_preserves_completion_until_retry() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sessions.db");
    let mut session =
        SessionState::open_in(database.clone(), None, "model".into(), directory.path())
            .await
            .unwrap();
    session
        .begin("initial request".into(), "model".into())
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER reject_completion BEFORE UPDATE OF status ON runs \
         BEGIN SELECT RAISE(ABORT, 'injected transaction failure'); END;",
        )
        .unwrap();
    assert!(
        session
            .finish(RunCompletion::Completed {
                history: vec![ChatMessage::assistant("saved result")]
            })
            .await
            .is_err()
    );
    assert!(matches!(session.run, RunState::Finalizing { .. }));
    assert_eq!(
        session
            .store
            .load(session.id())
            .await
            .unwrap()
            .runs[0]
            .status,
        "running"
    );
    let elapsed_ms = session
        .writer
        .as_mut()
        .unwrap()
        .freeze_elapsed()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    connection
        .execute_batch("DROP TRIGGER reject_completion;")
        .unwrap();
    session
        .begin("later request".into(), "model".into())
        .await
        .unwrap();
    let snapshot = session
        .store
        .load(session.id())
        .await
        .unwrap();
    assert_eq!(snapshot.runs.len(), 2);
    assert_eq!(snapshot.runs[0].status, "completed");
    assert_eq!(
        snapshot.runs[0].elapsed_ms,
        Some(u64::try_from(elapsed_ms).unwrap())
    );
    assert_eq!(
        snapshot.history,
        vec![ChatMessage::assistant("saved result")]
    );
    session
        .finish(RunCompletion::Cancelled)
        .await
        .unwrap();
}

#[tokio::test]
async fn committed_terminal_receipt_only_retries_its_failed_projection_refresh() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("sessions.db");
    let mut session =
        SessionState::open_in(database.clone(), None, "model".into(), directory.path())
            .await
            .unwrap();
    session
        .begin("first request".into(), "model".into())
        .await
        .unwrap();
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TRIGGER invalidate_projection AFTER UPDATE OF status ON runs \
         WHEN NEW.status = 'failed' BEGIN \
         UPDATE sessions SET history = 'invalid json' WHERE id = NEW.session_id; END;",
        )
        .unwrap();
    assert!(
        session
            .finish(RunCompletion::Failed)
            .await
            .is_err()
    );
    assert!(matches!(session.run, RunState::Refreshing));
    assert_eq!(session.active_run_id(), None);
    connection
        .execute_batch("DROP TRIGGER invalidate_projection; UPDATE sessions SET history = '[]';")
        .unwrap();
    session
        .begin("second request".into(), "model".into())
        .await
        .unwrap();
    let snapshot = session
        .store
        .load(session.id())
        .await
        .unwrap();
    assert_eq!(snapshot.runs.len(), 2);
    assert_eq!(snapshot.runs[0].status, "failed");
    session
        .finish(RunCompletion::Cancelled)
        .await
        .unwrap();
}

#[tokio::test]
async fn idle_metadata_poll_releases_a_transient_finalization_lease() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = SessionState::open_in(
        directory.path().join("sessions.db"),
        None,
        "model".into(),
        directory.path(),
    )
    .await
    .unwrap();
    session.enable_shared();
    session
        .begin("request".into(), "model".into())
        .await
        .unwrap();
    let RunState::Running { journal, .. } = &mut session.run else {
        panic!("running")
    };
    *journal = Some(std::sync::Arc::new(FailFirstFlush(true.into())));
    assert!(
        session
            .finish(RunCompletion::Cancelled)
            .await
            .is_err()
    );
    assert!(session.metadata_pending());
    assert!(session.writer.is_some());
    assert!(session.poll_metadata().await.unwrap());
    assert!(!session.metadata_pending());
    assert!(session.writer.is_none());
    assert!(!session.poll_metadata().await.unwrap());
    assert_eq!(session.snapshot.runs[0].status, "cancelled");
    assert!(
        session
            .store
            .acquire(session.id())
            .await
            .is_ok()
    );
}
