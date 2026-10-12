use super::*;
use orbit_protocol::input_queue::{Action, Snapshot, State};

#[tokio::test]
async fn actual_dispatch_mutates_waiting_inputs_and_preserves_them_after_cancellation() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let fixture = Fixture::new(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
    let session = fixture.handle.info.id.clone();
    let workspace = fixture.handle.info.workspace.clone();
    fixture
        .request(Request::Start {
            session: session.clone(),
            request_id: "queue-native-start".into(),
            prompt: "initial".into(),
            images: vec![],
            compact: false,
            model: None,
        })
        .await
        .unwrap();
    // Leave the real model request waiting, so steering stays in the real inbox.
    let (_socket, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ids = Vec::new();
    for message in ["original", "remove me"] {
        let Response::InputQueued { message_id } = fixture
            .request(Request::Steer {
                session: session.clone(),
                message: message.into(),
            })
            .await
            .unwrap()
        else {
            panic!("queue receipt expected")
        };
        ids.push(message_id);
    }
    let queue = |action| Request::InputQueue {
        session: session.clone(),
        expected_workspace: workspace.clone(),
        action,
    };
    let Response::InputQueue(Snapshot::List { entries, run, .. }) = fixture
        .request(queue(Action::List { after: None }))
        .await
        .unwrap()
    else {
        panic!("queue list expected")
    };
    assert!(run.is_some());
    assert_eq!(
        entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
        ids
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry.state == State::Queued && entry.editable())
    );
    let Response::InputQueue(Snapshot::Edited { entry }) = fixture
        .request(queue(Action::Edit {
            id: ids[0],
            expected: "original".into(),
            content: "updated".into(),
            run,
        }))
        .await
        .unwrap()
    else {
        panic!("edit receipt expected")
    };
    assert_eq!((entry.id, entry.content.as_str()), (ids[0], "updated"));
    assert!(
        fixture
            .request(queue(Action::Edit {
                id: ids[0],
                expected: "original".into(),
                content: "stale".into(),
                run,
            }))
            .await
            .is_err()
    );
    fixture
        .request(queue(Action::Remove {
            id: ids[1],
            expected: "remove me".into(),
            run,
        }))
        .await
        .unwrap();
    assert!(
        fixture
            .request(Request::InputQueue {
                session: session.clone(),
                expected_workspace: "/stale-workspace".into(),
                action: Action::Remove {
                    id: ids[0],
                    expected: "updated".into(),
                    run
                },
            })
            .await
            .is_err()
    );
    assert!(
        fixture
            .request(Request::InputQueue {
                session: "not-an-open-session".into(),
                expected_workspace: workspace.clone(),
                action: Action::List { after: None },
            })
            .await
            .is_err()
    );
    fixture
        .request(Request::Cancel {
            session: session.clone(),
        })
        .await
        .unwrap();
    fixture.wait(|page| !page.busy).await;
    let Response::InputQueue(Snapshot::List {
        entries,
        run: idle_run,
        ..
    }) = fixture
        .request(queue(Action::List { after: None }))
        .await
        .unwrap()
    else {
        panic!("idle queue list expected")
    };
    assert_eq!(idle_run, None);
    assert_eq!(entries.len(), 1);
    assert_eq!(
        (entries[0].id, entries[0].content.as_str(), entries[0].state),
        (ids[0], "updated", State::Idle)
    );
    assert!(
        fixture
            .request(queue(Action::Remove {
                id: ids[0],
                expected: "updated".into(),
                run
            }))
            .await
            .is_err(),
        "a receipt sampled from the previous run must not mutate an idle queue"
    );
    fixture
        .request(queue(Action::Remove {
            id: ids[0],
            expected: "updated".into(),
            run: None,
        }))
        .await
        .unwrap();
    assert!(
        fixture
            .handle
            .store
            .input_queue(&session)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
}

#[tokio::test]
async fn actor_rechecks_workspace_even_when_dispatch_is_bypassed() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let error = fixture
        .handle
        .input_queue(Action::List { after: None }, "/old-workspace".into())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("工作区已切换"));
}
