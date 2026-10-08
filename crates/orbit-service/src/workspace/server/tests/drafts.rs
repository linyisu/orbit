use super::*;

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
    // Opening and configuring a draft must not create any domain records.
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
    drop(db);
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
async fn last_client_releases_only_unstarted_drafts() {
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
    fixture.service.release_client(vec![info.id.clone()]).await;
    assert!(fixture.service.session(&info.id).await.is_ok());
    fixture.service.release_client(vec![info.id.clone()]).await;
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
        .release_client(vec![started.id.clone()])
        .await;
    assert!(fixture.service.session(&started.id).await.is_ok());
    assert_eq!(fixture.handle.store.list().await.unwrap().len(), 1);
    handle.command(session::Command::Cancel).await.unwrap();
}
