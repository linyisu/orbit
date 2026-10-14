use super::*;
use orbit_protocol::input_queue::{Action, Snapshot, State};

#[tokio::test]
async fn actual_dispatch_mutates_waiting_inputs_and_preserves_them_after_cancellation() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
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
        entries
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>(),
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
    assert!(
        error
            .to_string()
            .contains("工作区已切换")
    );
}

#[tokio::test]
async fn image_input_is_uploaded_queued_and_authorized_for_withdrawal() {
    use base64::Engine;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let fixture = Fixture::with_vision(
        &format!("http://{}/v1", listener.local_addr().unwrap()),
        true,
    )
    .await;
    let session = fixture.handle.info.id.clone();
    fixture
        .request(Request::Start {
            session: session.clone(),
            request_id: "image-queue".into(),
            prompt: "wait".into(),
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
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let bytes = png.into_inner();
    let Response::InputQueued { message_id } = fixture
        .request(Request::SteerImages {
            session: session.clone(),
            message: String::new(),
            images: vec![orbit_protocol::ImageUpload {
                name: "queued.png".into(),
                data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            }],
        })
        .await
        .unwrap()
    else {
        panic!("queue receipt expected")
    };
    let queue = |action| Request::InputQueue {
        session: session.clone(),
        expected_workspace: fixture.handle.info.workspace.clone(),
        action,
    };
    let Response::InputQueue(Snapshot::List { entries, run, .. }) = fixture
        .request(queue(Action::List { after: None }))
        .await
        .unwrap()
    else {
        panic!("queue expected")
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, message_id);
    assert_eq!(entries[0].images.len(), 1);
    assert!(entries[0].content.is_empty());
    assert_eq!(entries[0].state, State::Queued);
    let Response::File(data) = fixture
        .request(Request::ReadFile {
            session: session.clone(),
            path: entries[0].images[0].path.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("image must be readable")
    };
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap(),
        bytes
    );
    fixture
        .request(queue(Action::Remove {
            id: message_id,
            expected: String::new(),
            run,
        }))
        .await
        .unwrap();
    let Response::InputQueue(Snapshot::List { entries, .. }) = fixture
        .request(queue(Action::List { after: None }))
        .await
        .unwrap()
    else {
        panic!("queue expected")
    };
    assert!(entries.is_empty());
    fixture
        .request(Request::Cancel { session })
        .await
        .unwrap();
}

#[tokio::test]
async fn file_input_is_queued_without_vision_and_can_be_withdrawn() {
    use base64::Engine;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let fixture = Fixture::with_vision(
        &format!("http://{}/v1", listener.local_addr().unwrap()),
        false,
    )
    .await;
    let session = fixture.handle.info.id.clone();
    fixture
        .request(Request::Start {
            session: session.clone(),
            request_id: "image-queue".into(),
            prompt: "wait".into(),
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
    let bytes = b"generated queue material\n".to_vec();
    let Response::InputQueued { message_id } = fixture
        .request(Request::SubmitAttachments {
            session: session.clone(),
            request_id: "queued-file".into(),
            prompt: String::new(),
            steer: true,
            images: vec![],
            files: vec![orbit_protocol::FileUpload {
                name: "queued.txt".into(),
                data: base64::engine::general_purpose::STANDARD.encode(&bytes),
            }],
        })
        .await
        .unwrap()
    else {
        panic!("queue receipt expected")
    };
    let queue = |action| Request::InputQueue {
        session: session.clone(),
        expected_workspace: fixture.handle.info.workspace.clone(),
        action,
    };
    let Response::InputQueue(Snapshot::List { entries, run, .. }) = fixture
        .request(queue(Action::List { after: None }))
        .await
        .unwrap()
    else {
        panic!("queue expected")
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, message_id);
    assert_eq!(entries[0].files.len(), 1);
    assert!(entries[0].content.is_empty());
    assert_eq!(entries[0].state, State::Queued);
    let Response::File(data) = fixture
        .request(Request::ReadFile {
            session: session.clone(),
            path: entries[0].files[0].path.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("file must be readable")
    };
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .unwrap(),
        bytes
    );
    fixture
        .request(queue(Action::Remove {
            id: message_id,
            expected: String::new(),
            run,
        }))
        .await
        .unwrap();
    let Response::InputQueue(Snapshot::List { entries, .. }) = fixture
        .request(queue(Action::List { after: None }))
        .await
        .unwrap()
    else {
        panic!("queue expected")
    };
    assert!(entries.is_empty());
    fixture
        .request(Request::Cancel { session })
        .await
        .unwrap();
}
