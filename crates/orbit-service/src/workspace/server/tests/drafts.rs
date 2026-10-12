use super::*;

#[tokio::test]
async fn global_history_resolves_a_moved_project_before_resume_without_writing_on_browse() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let store = &fixture.handle.store;
    let old = &fixture.external;
    let moved = fixture._root.path().join("moved-project");
    let mut writer = store
        .create(old.display().to_string(), "test::test".into())
        .await
        .unwrap();
    let id = writer.id().to_owned();
    writer
        .begin("remember this".into(), "test::test".into())
        .await
        .unwrap();
    writer
        .finish(
            "completed",
            vec![],
            Some(vec![orbit_model::types::ChatMessage::user("remember this")]),
        )
        .await
        .unwrap();
    drop(writer);
    let original = store.session(&id).await.unwrap();
    fs::rename(old, &moved).unwrap();

    // Merely selecting the new directory must locate history without creating
    // a session or persisting a project-path change.
    let mut options = fixture.service.options.clone();
    options.initial_directory = moved.clone();
    let Response::Sessions(history) = fixture
        .service
        .dispatch(Request::AllSessions, &options, 0)
        .await
        .unwrap()
    else {
        panic!("missing global history")
    };
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, id);
    assert_eq!(history[0].project_id, original.project_id);
    assert_eq!(history[0].workspace, moved.display().to_string());
    assert_eq!(
        store.session(&id).await.unwrap().workspace,
        original.workspace
    );

    let wrong_directory = fixture
        .request(Request::Open {
            model: None,
            policy: Default::default(),
            resume: Some(id.clone()),
            configuration: None,
        })
        .await
        .unwrap_err();
    assert!(
        wrong_directory
            .to_string()
            .contains("session belongs to workspace")
    );
    assert_eq!(
        store.session(&id).await.unwrap().project_id,
        original.project_id
    );

    // The GUI reconnects to the path returned by the sidebar listing.
    options.initial_directory = history[0].workspace.clone().into();
    let Response::Opened(info) = fixture
        .service
        .dispatch(
            Request::Open {
                model: None,
                policy: Default::default(),
                resume: Some(id.clone()),
                configuration: None,
            },
            &options,
            0,
        )
        .await
        .unwrap()
    else {
        panic!("failed to resume moved project")
    };
    assert_eq!(info.id, id);
    assert_eq!(info.workspace, moved.display().to_string());
    let snapshot = store.load(&id).await.unwrap();
    assert_eq!(snapshot.runs.len(), 1);
    assert_eq!(snapshot.history.len(), 1);

    // Other workspaces also see the corrected location after a real resume.
    let Response::Sessions(history) = fixture.request(Request::AllSessions).await.unwrap() else {
        panic!("missing refreshed history")
    };
    assert_eq!(history[0].workspace, info.workspace);

    // Reusing the original path is a new project, not the migrated one.
    fs::create_dir(old).unwrap();
    options.initial_directory = old.clone();
    let Response::Sessions(local) = fixture
        .service
        .dispatch(Request::Sessions, &options, 0)
        .await
        .unwrap()
    else {
        panic!("missing project history")
    };
    assert!(local.is_empty());
}

#[tokio::test]
async fn global_history_lists_other_directories_without_changing_project_scoped_history() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let store = &fixture.handle.store;
    let mut ids = Vec::new();
    for directory in [
        &fixture.service.options.initial_directory,
        &fixture.external,
    ] {
        let mut writer = store
            .create(directory.display().to_string(), "test::test".into())
            .await
            .unwrap();
        ids.push(writer.id().to_owned());
        writer
            .begin("saved conversation".into(), "test::test".into())
            .await
            .unwrap();
        writer
            .finish("completed", vec![], Some(vec![]))
            .await
            .unwrap();
    }
    let Response::Sessions(global) = fixture.request(Request::AllSessions).await.unwrap() else {
        panic!("missing global history")
    };
    assert_eq!(global.len(), 2);
    assert!(
        ids.iter()
            .all(|id| global.iter().any(|session| &session.id == id))
    );
    let Response::Sessions(local) = fixture.request(Request::Sessions).await.unwrap() else {
        panic!("missing project history")
    };
    assert_eq!(local.len(), 1);
    assert_eq!(local[0].id, ids[0]);
    let mut remote_options = fixture.service.options.clone();
    remote_options.initial_directory = fixture.external.clone();
    let Response::Opened(info) = fixture
        .service
        .dispatch(
            Request::Open {
                model: None,
                policy: Default::default(),
                resume: Some(ids[1].clone()),
                configuration: None,
            },
            &remote_options,
            0,
        )
        .await
        .unwrap()
    else {
        panic!("failed to resume other project")
    };
    assert_eq!(info.workspace, fixture.external.display().to_string());
    assert_eq!(info.id, ids[1]);
}

#[tokio::test]
async fn resumed_transcript_pages_join_live_updates_without_reordering_or_losing_reconnect_history()
{
    use orbit_agent::events::AgentEvent;
    use orbit_model::types::StreamEvent;
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let store = &fixture.handle.store;
    let mut writer = store
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
    let id = writer.id().to_owned();
    for run in 0..2 {
        writer
            .begin(format!("question {run}"), "test::test".into())
            .await
            .unwrap();
        let journal = writer.execution_journal().unwrap();
        for event in 0..150 {
            journal.record(AgentEvent::ReasoningFinished { elapsed_ms: event });
            journal.record(AgentEvent::Model(StreamEvent::TextDelta(format!(
                "{run}/{event}"
            ))));
        }
        journal.flush().await.unwrap();
        writer
            .finish(
                "completed",
                vec![],
                Some(vec![orbit_model::types::ChatMessage::user(format!(
                    "question {run}"
                ))]),
            )
            .await
            .unwrap();
    }
    drop(writer);
    fixture
        .request(Request::Open {
            model: None,
            policy: Default::default(),
            resume: Some(id.clone()),
            configuration: None,
        })
        .await
        .unwrap();
    let handle = fixture.service.session(&id).await.unwrap();
    let mut after = 0;
    let mut received = Vec::new();
    loop {
        let page = handle.page(0, after).await.unwrap();
        assert!(page.next >= after && page.updates.len() <= protocol::PAGE_EVENTS);
        after = page.next;
        received.extend(page.updates);
        if !page.more {
            break;
        }
    }
    assert_eq!(
        received
            .iter()
            .filter(|event| matches!(event, protocol::Update::Started { .. }))
            .count(),
        2
    );
    assert_eq!(
        received
            .iter()
            .filter(|event| matches!(event, protocol::Update::Agent(_)))
            .count(),
        600
    );
    let initial = after;
    handle
        .command(session::Command::SetMode { mode: Mode::Yolo })
        .await
        .unwrap();
    let tail = handle.page(0, after).await.unwrap();
    assert_eq!(tail.next, initial + 1);
    assert!(
        matches!(&tail.updates[..], [protocol::Update::Profile(info)] if info.mode == Mode::Yolo)
    );
    let mut replay = Vec::new();
    let mut cursor = 0;
    while cursor < initial {
        let page = handle.page(1, cursor).await.unwrap();
        cursor = page.next;
        replay.extend(page.updates);
    }
    assert_eq!(
        serde_json::to_value(&replay[..received.len()]).unwrap(),
        serde_json::to_value(received).unwrap()
    );
    assert!(handle.page(1, tail.next + 1).await.is_err());
}

#[tokio::test]
async fn draft_preferences_stay_in_memory_until_first_message_and_survive_resume() {
    use orbit_protocol::{Inspection, SessionAction, plan};
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let id = fixture.handle.info.id.clone();
    let store = &fixture.handle.store;
    assert!(store.list().await.unwrap().is_empty());
    assert!(store.git_baseline(&id).await.unwrap().is_none());
    assert!(store.workspace_baseline(&id).await.unwrap().is_none());
    fixture.mode(Mode::Yolo).await;
    fixture
        .request(Request::Plan {
            session: id.clone(),
            action: plan::Action::Enter,
        })
        .await
        .unwrap();
    fixture
        .request(Request::SetModel {
            session: id.clone(),
            model: "test::test".into(),
            effort: None,
        })
        .await
        .unwrap();
    fixture
        .request(Request::ListSessions {
            session: id.clone(),
        })
        .await
        .unwrap();
    fixture
        .request(Request::Complete {
            session: id.clone(),
            input: "/resume ".into(),
        })
        .await
        .unwrap();
    fixture
        .handle
        .action(SessionAction::Inspect {
            section: Inspection::Context,
        })
        .await
        .unwrap();
    fixture
        .handle
        .action(SessionAction::Inspect {
            section: Inspection::Diagnostics,
        })
        .await
        .unwrap();
    fixture
        .handle
        .action(SessionAction::ChangeDirectory {
            path: fixture.external.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    assert!(store.list().await.unwrap().is_empty());
    assert!(store.projects().await.unwrap().is_empty());
    assert_no_domain_records(store);
    assert_eq!(fixture.handle.current_info().await.mode, Mode::Yolo);
    assert!(
        fixture
            .handle
            .command(session::Command::Start {
                request_id: "empty".into(),
                prompt: "  ".into(),
                images: vec![],
                compact: false,
                model: None,
            })
            .await
            .is_err()
    );
    assert!(store.list().await.unwrap().is_empty());
    fixture
        .handle
        .command(session::Command::Start {
            request_id: "first".into(),
            prompt: "hello".into(),
            images: vec![],
            compact: false,
            model: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.handle.running().await {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    // A failed model connection is still a real conversation and must remain resumable.
    assert_eq!(store.list_resumable().await.unwrap().len(), 1);
    let snapshot = store.load(&id).await.unwrap();
    assert_eq!(snapshot.runs.len(), 1);
    assert_eq!(snapshot.runs[0].prompt, "hello");
    assert_eq!(
        snapshot.session.workspace,
        fixture.external.to_string_lossy()
    );
    assert_eq!(store.tui_mode(&id).await.unwrap(), "YOLO");
    assert_eq!(
        store.planning(&id).await.unwrap().mode,
        plan::WorkMode::Plan
    );
    fixture
        .handle
        .command(session::Command::Close)
        .await
        .unwrap();
    let mut options = fixture.service.options.clone();
    options.initial_directory = fixture.external.clone();
    let resumed = crate::workspace::setup::prepare_service(&options, Some(id.clone()))
        .await
        .unwrap();
    assert_eq!(resumed.session.id(), id);
    assert_eq!(resumed.resources.mode, Mode::Yolo);
    assert_eq!(
        resumed.resources.planning.snapshot().await.mode,
        plan::WorkMode::Plan
    );
}

#[tokio::test]
async fn last_client_discards_drafts_but_retains_running_sessions_until_finish() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let Response::Opened(info) = fixture
        .request(Request::Open {
            model: Some("test::test".into()),
            policy: Default::default(),
            resume: None,
            configuration: None,
        })
        .await
        .unwrap()
    else {
        panic!("missing draft")
    };
    fixture
        .request(Request::Open {
            model: None,
            policy: Default::default(),
            resume: Some(info.id.clone()),
            configuration: None,
        })
        .await
        .unwrap();
    fixture
        .service
        .release_client(vec![info.id.clone()], 0)
        .await;
    assert!(fixture.service.session(&info.id).await.is_ok());
    fixture
        .service
        .release_client(vec![info.id.clone()], 0)
        .await;
    assert!(fixture.service.session(&info.id).await.is_err());
    assert!(fixture.handle.store.list().await.unwrap().is_empty());

    let Response::Opened(started) = fixture
        .request(Request::Open {
            model: Some("test::test".into()),
            policy: Default::default(),
            resume: None,
            configuration: None,
        })
        .await
        .unwrap()
    else {
        panic!("missing draft")
    };
    let handle = fixture.service.session(&started.id).await.unwrap();
    handle
        .command(session::Command::Start {
            request_id: "first".into(),
            prompt: "hello".into(),
            images: vec![],
            compact: false,
            model: None,
        })
        .await
        .unwrap();
    fixture
        .service
        .release_client(vec![started.id.clone()], 0)
        .await;
    assert!(fixture.service.session(&started.id).await.is_ok());
    assert_eq!(fixture.handle.store.list().await.unwrap().len(), 1);
    handle.command(session::Command::Cancel).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            fixture.service.collect_idle_sessions().await;
            if fixture.service.session(&started.id).await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        fixture.handle.store.list_resumable().await.unwrap().len(),
        1
    );
    // Retirement loses only the runtime cache, never durable request receipts.
    fixture
        .request(Request::Open {
            resume: Some(started.id.clone()),
            model: None,
            configuration: None,
            policy: Default::default(),
        })
        .await
        .unwrap();
    fixture
        .request(Request::Start {
            session: started.id.clone(),
            request_id: "first".into(),
            prompt: "hello".into(),
            images: vec![],
            compact: false,
            model: None,
        })
        .await
        .unwrap();
    assert_eq!(
        fixture
            .handle
            .store
            .load(&started.id)
            .await
            .unwrap()
            .runs
            .len(),
        1
    );
    fixture.service.release_client(vec![started.id], 0).await;
}

#[tokio::test]
async fn closing_idle_persisted_sessions_releases_capacity_and_preserves_resume() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let store = &fixture.handle.store;
    let mut ids = Vec::new();
    for index in 0..MAX_SESSIONS + 2 {
        let mut writer = store
            .create(
                fixture
                    .service
                    .options
                    .initial_directory
                    .to_string_lossy()
                    .into_owned(),
                "test::test".into(),
            )
            .await
            .unwrap();
        let id = writer.id().to_owned();
        writer
            .begin(format!("question {index}"), "test::test".into())
            .await
            .unwrap();
        writer
            .finish(
                "completed",
                vec![],
                Some(vec![orbit_model::types::ChatMessage::user(format!(
                    "question {index}"
                ))]),
            )
            .await
            .unwrap();
        drop(writer);
        let Response::Opened(info) = fixture
            .request(Request::Open {
                model: None,
                policy: Default::default(),
                resume: Some(id.clone()),
                configuration: None,
            })
            .await
            .unwrap()
        else {
            panic!("missing restored session")
        };
        assert_eq!(info.id, id);
        fixture.service.release_client(vec![id.clone()], 0).await;
        assert!(fixture.service.session(&id).await.is_err());
        let snapshot = store.load(&id).await.unwrap();
        assert_eq!(snapshot.runs[0].prompt, format!("question {index}"));
        ids.push(id);
    }
    // Opening a second viewer retains the actor until both viewers disconnect.
    let id = ids[0].clone();
    for _ in 0..2 {
        fixture
            .request(Request::Open {
                model: None,
                policy: Default::default(),
                resume: Some(id.clone()),
                configuration: None,
            })
            .await
            .unwrap();
    }
    fixture.service.release_client(vec![id.clone()], 0).await;
    assert!(fixture.service.session(&id).await.is_ok());
    fixture.service.release_client(vec![id.clone()], 1).await;
    assert!(fixture.service.session(&id).await.is_err());
    assert_eq!(
        store.list_resumable().await.unwrap().len(),
        MAX_SESSIONS + 2
    );
}

#[tokio::test]
async fn btw_is_ephemeral_and_last_viewer_close_or_disconnect_discards_it() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let id = fixture.handle.info.id.clone();
    let request = |viewer, request| {
        fixture
            .service
            .dispatch(request, &fixture.service.options, viewer)
    };
    let Response::Questions(initial) = request(
        10,
        Request::Ask {
            session: id.clone(),
            question: "independent question".into(),
            follow_up: None,
        },
    )
    .await
    .unwrap() else {
        panic!("missing side answer")
    };
    assert_eq!(initial.records.unwrap().len(), 1);
    assert!(fixture.handle.store.list().await.unwrap().is_empty());
    let Response::Questions(shared) = request(
        20,
        Request::OpenQuestions {
            session: id.clone(),
        },
    )
    .await
    .unwrap() else {
        panic!("missing shared answer")
    };
    assert_eq!(shared.records.unwrap()[0].question, "independent question");

    // Closing repeatedly, or disconnecting a non-viewer, must not release another viewer.
    for viewer in [10, 10, 30] {
        request(
            viewer,
            Request::CloseQuestions {
                session: id.clone(),
            },
        )
        .await
        .unwrap();
    }
    assert!(
        request(
            10,
            Request::ReadQuestions {
                session: id.clone(),
                after: None
            }
        )
        .await
        .is_err()
    );
    let Response::Questions(shared) = request(
        20,
        Request::ReadQuestions {
            session: id.clone(),
            after: None,
        },
    )
    .await
    .unwrap() else {
        panic!("missing shared answer")
    };
    assert_eq!(shared.records.unwrap().len(), 1);
    // A disconnect follows the same release path even when the main session stays open.
    fixture.service.release_client(vec![], 20).await;
    assert!(
        request(
            20,
            Request::OpenQuestions {
                session: id.clone()
            }
        )
        .await
        .is_err()
    );
    assert!(!fixture.handle.questions.running().await);
    assert!(fixture.handle.store.list().await.unwrap().is_empty());
    assert!(
        request(
            20,
            Request::Ask {
                session: id.clone(),
                question: "stale follow-up".into(),
                follow_up: Some(0)
            }
        )
        .await
        .is_err()
    );
    let Response::Questions(fresh) = request(
        20,
        Request::Ask {
            session: id.clone(),
            question: "fresh question".into(),
            follow_up: None,
        },
    )
    .await
    .unwrap() else {
        panic!("missing fresh question")
    };
    assert_eq!(fresh.records.unwrap().len(), 1);
    assert!(fresh.revision > shared.revision);
    request(
        20,
        Request::CloseQuestions {
            session: id.clone(),
        },
    )
    .await
    .unwrap();
    assert!(
        request(20, Request::OpenQuestions { session: id })
            .await
            .is_err()
    );
    assert_no_domain_records(&fixture.handle.store);
}

fn assert_no_domain_records(store: &orbit_store::store::Store) {
    let db = rusqlite::Connection::open(store.path()).unwrap();
    let tables = db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name <> 'seaql_migrations'").unwrap()
        .query_map([], |row| row.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    for table in tables {
        let count: i64 = db
            .query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "draft wrote records to {table}");
    }
}

#[tokio::test]
async fn explicit_directory_replacement_recovers_history_after_filesystem_identity_changes() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let store = &fixture.handle.store;
    let old = &fixture.external;
    let moved = fixture._root.path().join("Workspace");
    // A cross-filesystem move (or copy then removing the source) creates a new
    // directory identity; rename-only coverage cannot catch this workflow.
    fs::create_dir(&moved).unwrap();
    let mut writer = store
        .create(old.display().to_string(), "test::test".into())
        .await
        .unwrap();
    let id = writer.id().to_owned();
    writer
        .begin("remember this".into(), "test::test".into())
        .await
        .unwrap();
    writer
        .finish(
            "completed",
            vec![],
            Some(vec![orbit_model::types::ChatMessage::user("remember this")]),
        )
        .await
        .unwrap();
    drop(writer);
    let original = store.session(&id).await.unwrap();
    fs::remove_dir_all(old).unwrap();
    let mut options = fixture.service.options.clone();
    options.initial_directory = moved.clone();
    let open = || Request::Open {
        model: None,
        policy: Default::default(),
        resume: Some(id.clone()),
        configuration: None,
    };
    let error = fixture
        .service
        .dispatch(open(), &options, 0)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("session belongs to workspace"));
    assert_eq!(
        store.session(&id).await.unwrap().workspace,
        original.workspace
    );
    fixture
        .service
        .dispatch(
            Request::Admin {
                action: orbit_protocol::admin::Action::LinkProject {
                    id: original.project_id.clone(),
                },
            },
            &options,
            0,
        )
        .await
        .unwrap();
    let Response::Sessions(history) = fixture
        .service
        .dispatch(Request::AllSessions, &options, 0)
        .await
        .unwrap()
    else {
        panic!("missing history")
    };
    assert_eq!(history[0].id, id);
    assert_eq!(history[0].project_id, original.project_id);
    assert_eq!(history[0].workspace, moved.display().to_string());
    let Response::Opened(info) = fixture.service.dispatch(open(), &options, 0).await.unwrap()
    else {
        panic!("failed to resume relocated session")
    };
    assert_eq!(info.id, id);
    assert_eq!(info.workspace, moved.display().to_string());
    let snapshot = store.load(&id).await.unwrap();
    assert_eq!(snapshot.runs.len(), 1);
    assert_eq!(snapshot.history.len(), 1);
    assert_eq!(snapshot.session.project_id, original.project_id);
}
