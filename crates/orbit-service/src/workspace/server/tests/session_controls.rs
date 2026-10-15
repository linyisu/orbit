use super::*;
use orbit_protocol::admin::Action;

#[tokio::test]
async fn compaction_accepts_empty_prompts_without_allowing_empty_chat_requests() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let session = fixture.handle.info.id.clone();
    let request = |prompt: &str, compact| Request::Start {
        session: session.clone(),
        request_id: orbit_protocol::request_id(),
        prompt: prompt.into(),
        images: vec![],
        compact,
        model: None,
    };
    assert!(
        fixture
            .request(request("  ", false))
            .await
            .is_err()
    );
    for prompt in ["", " \n ", "/compact"] {
        fixture
            .request(request(prompt, true))
            .await
            .unwrap();
        let page = fixture.wait(|page| !page.busy).await;
        assert!(page.updates.iter().any(|update| {
            matches!(update, protocol::Update::Started { prompt, presentation, .. }
                if prompt == "/compact" && presentation.compact)
        }));
        let snapshot = fixture
            .handle
            .store
            .load(&session)
            .await
            .unwrap();
        assert_eq!(snapshot.runs.last().unwrap().prompt, "/compact");
    }
}

#[tokio::test]
async fn saved_session_rename_keeps_history_and_rejects_another_writer() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let store = &fixture.handle.store;
    let mut writer = store
        .create(fixture.external.display().to_string(), "test::test".into())
        .await
        .unwrap();
    let id = writer.id().to_owned();
    writer
        .begin("saved conversation".into(), "test::test".into())
        .await
        .unwrap();
    writer
        .finish(
            &orbit_store::types::RunCompletion::Completed {
                history: vec![orbit_model::types::ChatMessage::user("saved conversation")],
            },
            vec![],
        )
        .await
        .unwrap();
    let rename = |title: &str| Request::Admin {
        action: Action::RenameSession {
            id: id.clone(),
            title: title.into(),
        },
    };
    assert!(
        fixture
            .request(rename("writer still owns this session"))
            .await
            .is_err()
    );
    drop(writer);
    let before = store.load(&id).await.unwrap();
    fixture
        .request(rename("跨项目的历史会话"))
        .await
        .unwrap();
    let after = store.load(&id).await.unwrap();
    assert_eq!(after.session.id, before.session.id);
    assert_eq!(after.session.project_id, before.session.project_id);
    assert_eq!(after.session.workspace, before.session.workspace);
    assert_eq!(after.session.title, "跨项目的历史会话");
    assert_eq!(after.runs.len(), before.runs.len());
    assert_eq!(after.history.len(), before.history.len());
    for title in [" ".to_owned(), "中".repeat(121)] {
        assert!(
            fixture
                .request(rename(&title))
                .await
                .is_err()
        );
    }
    assert_eq!(store.session(&id).await.unwrap().title, "跨项目的历史会话");
}

#[tokio::test]
async fn running_session_can_be_renamed_but_delete_preserves_its_actor_and_history() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let fixture = Fixture::new(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
    let id = fixture.handle.info.id.clone();
    fixture
        .request(Request::Start {
            session: id.clone(),
            request_id: "session-controls-busy".into(),
            prompt: "keep running until canceled".into(),
            images: vec![],
            compact: false,
            model: None,
        })
        .await
        .unwrap();
    let (_socket, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    fixture
        .request(Request::Admin {
            action: Action::RenameSession {
                id: id.clone(),
                title: "运行中的会话".into(),
            },
        })
        .await
        .unwrap();
    assert_eq!(
        fixture
            .handle
            .current_info()
            .await
            .title,
        "运行中的会话"
    );
    let delete = || Request::Admin {
        action: Action::DeleteSession { id: id.clone() },
    };
    let error = fixture
        .request(delete())
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("会话正在运行")
    );
    assert!(
        fixture
            .service
            .sessions
            .lock()
            .await
            .contains_key(&id)
    );
    assert!(fixture.page(0).await.busy);
    assert_eq!(
        fixture
            .handle
            .store
            .session(&id)
            .await
            .unwrap()
            .title,
        "运行中的会话"
    );
    fixture
        .request(Request::Cancel {
            session: id.clone(),
        })
        .await
        .unwrap();
    fixture.wait(|page| !page.busy).await;
    fixture.request(delete()).await.unwrap();
    assert!(
        !fixture
            .service
            .sessions
            .lock()
            .await
            .contains_key(&id)
    );
    assert!(
        fixture
            .handle
            .store
            .session(&id)
            .await
            .is_err()
    );
}
