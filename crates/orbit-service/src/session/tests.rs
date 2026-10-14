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
        .finish("cancelled", None)
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
    assert!(!current.poll_title().await.unwrap());
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
        .finish("cancelled", None)
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
        .finish("cancelled", None)
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
        .finish("cancelled", None)
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
        .finish(
            "completed",
            Some(vec![
                ChatMessage::user("question"),
                ChatMessage::assistant("answer"),
            ]),
        )
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
        .finish("cancelled", None)
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
        .finish("cancelled", None)
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
        .finish("completed", Some(history.clone()))
        .await
        .unwrap();
    assert!(command_output.lease().is_none());
    assert!(!first_command_output.exists());
    state.resume(&first).await.unwrap();

    assert_eq!(state.snapshot.history, history);

    state
        .new_session("model".into())
        .await
        .unwrap();
    let second = state.id().to_owned();

    assert_ne!(first, second);
    assert!(command_output.lease().is_none());
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

    assert!(command_output.lease().is_none());
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
            .is_none()
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
async fn failed_cancelled_and_dropped_tasks_release_temporary_command_logs() {
    let temporary = tempfile::tempdir().unwrap();
    let database = temporary.path().join("sessions.db");
    let mut session = SessionState::open(database.clone(), None, "model".into())
        .await
        .unwrap();
    for status in ["failed", "cancelled"] {
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
            .finish(status, None)
            .await
            .unwrap();
        assert!(session.command_output.lease().is_none());
        assert!(!directory.exists());
    }
    assert!(
        !database
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
    assert!(outputs.lease().is_none());
    assert!(!directory.exists());
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
        .finish(
            "completed",
            Some(vec![
                ChatMessage::user("hello"),
                ChatMessage::assistant("hi"),
            ]),
        )
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
async fn test_resume_preserves_attachments_losslessly_and_projects_for_text_only_model() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("orbit.db");
    let mut session = SessionState::open(db_path.clone(), None, "test::model".into())
        .await
        .unwrap();

    session
        .begin("hi".into(), "test::model".into())
        .await
        .unwrap();
    session
        .finish(
            "completed",
            Some(vec![
                ChatMessage::user("hi"),
                ChatMessage::user_with_attachments(
                    "check this",
                    vec![orbit_model::types::Attachment::Image {
                        lease: None,
                        name: "pic.png".into(),
                        mime_type: "image/png".into(),
                        path: "/path/to/pic.png".into(),
                    }],
                ),
            ]),
        )
        .await
        .unwrap();

    let id = session.id().to_owned();
    drop(session);

    let resumed = SessionState::open(db_path, Some(id), "test::model".into())
        .await
        .unwrap();

    assert_eq!(resumed.snapshot.history.len(), 2);

    let ChatMessage::User {
        content,
        attachments,
    } = &resumed.snapshot.history[1]
    else {
        panic!("expected user message");
    };

    assert_eq!(
        attachments.len(),
        1,
        "persisted history must retain attachments losslessly"
    );
    assert_eq!(content, "check this");

    let projected = orbit_agent::model_messages(&resumed.snapshot.history, false);
    let ChatMessage::User {
        content: proj_content,
        attachments: proj_attachments,
    } = &projected[1]
    else {
        panic!("expected projected user message");
    };

    assert!(
        proj_attachments.is_empty(),
        "projected request attachments must be emptied"
    );
    assert!(
        proj_content
            .contains("[Image omitted: current model does not support image input: pic.png]")
    );
}

#[tokio::test]
async fn test_full_roundtrip_vision_to_text_to_vision_persists_attachments_losslessly() {
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
        .finish("completed", Some(history))
        .await
        .unwrap();
    let id = session.id().to_owned();
    drop(session);

    let mut session_text =
        SessionState::open(db_path.clone(), Some(id.clone()), "test::text-model".into())
            .await
            .unwrap();
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
        .finish("completed", Some(history_turn2))
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

    assert_eq!(session_vision.snapshot.history.len(), 5);

    let ChatMessage::User {
        attachments: final_attachments,
        ..
    } = &session_vision.snapshot.history[1]
    else {
        panic!()
    };

    assert_eq!(
        final_attachments.len(),
        1,
        "persisted history must still retain image attachment"
    );

    let projected_vision = orbit_agent::model_messages(&session_vision.snapshot.history, true);
    let ChatMessage::User {
        attachments: vis_attachments,
        ..
    } = &projected_vision[1]
    else {
        panic!()
    };

    assert_eq!(
        vis_attachments.len(),
        1,
        "vision projection must include the raw image attachment"
    );
}

#[tokio::test]
#[ignore = "requires ORBIT_RECOVERY_DB and ORBIT_RECOVERY_SESSION"]
async fn test_real_session_recovery_validation() {
    let real_db = match std::env::var("ORBIT_RECOVERY_DB") {
        Ok(v) if !v.is_empty() => std::path::PathBuf::from(v),
        _ => return,
    };

    let session_id = match std::env::var("ORBIT_RECOVERY_SESSION") {
        Ok(v) if !v.is_empty() => v,
        _ => return,
    };

    if !real_db.exists() {
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let copy_db = tmp.path().join("orbit_copy.db");
    std::fs::copy(&real_db, &copy_db).unwrap();

    for suffix in ["-wal", "-shm", "-journal"] {
        let mut src = real_db.as_os_str().to_os_string();
        src.push(suffix);
        let src = std::path::PathBuf::from(src);

        if src.exists() {
            let mut dst = copy_db.as_os_str().to_os_string();
            dst.push(suffix);
            let _ = std::fs::copy(&src, std::path::PathBuf::from(dst));
        }
    }

    let store = orbit_store::store::Store::open(copy_db)
        .await
        .unwrap();
    let saved = store
        .session(&session_id)
        .await
        .unwrap();
    let prepared =
        SessionState::prepare_resume(store.clone(), saved.project_id.clone(), session_id)
            .await
            .unwrap();

    assert!(!prepared.snapshot.history.is_empty());

    let projected_text = orbit_agent::model_messages(&prepared.snapshot.history, false);

    for msg in &projected_text {
        if let ChatMessage::User { attachments, .. } = msg {
            assert!(
                attachments.is_empty(),
                "text-only model must receive 0 attachments"
            );
        }
    }

    let projected_vision = orbit_agent::model_messages(&prepared.snapshot.history, true);

    assert_eq!(projected_vision.len(), prepared.snapshot.history.len());
}
