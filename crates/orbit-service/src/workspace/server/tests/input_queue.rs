use super::*;
use orbit_protocol::input_queue::{Action, Snapshot, State};

#[tokio::test]
async fn actual_dispatch_mutates_waiting_inputs_and_delivers_them_after_cancellation() {
    use orbit_agent::events::AgentEvent;
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
    let (_socket, _) = streaming_request(&listener).await;
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
    let page = fixture
        .wait(|page| {
            page.updates
                .iter()
                .any(|update| match update {
                    protocol::Update::Agent(event) => matches!(
                        event.as_ref(),
                        AgentEvent::InputReceived { message_id, content, .. }
                            if *message_id == ids[0] && content == "updated"
                    ),
                    _ => false,
                })
        })
        .await;
    assert!(page.busy);
    let Response::InputQueue(Snapshot::List {
        entries,
        run: next_run,
        ..
    }) = fixture
        .request(queue(Action::List { after: None }))
        .await
        .unwrap()
    else {
        panic!("continued queue expected")
    };
    assert_ne!(next_run, run);
    assert!(entries.is_empty());
    assert!(
        fixture
            .request(queue(Action::Remove {
                id: ids[0],
                expected: "updated".into(),
                run,
            }))
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
    let snapshot = fixture
        .handle
        .store
        .load(&session)
        .await
        .unwrap();
    assert_eq!(snapshot.runs.len(), 2);
    assert_eq!(
        snapshot
            .history
            .iter()
            .filter(|message| matches!(
                message,
                orbit_model::types::ChatMessage::User { content, .. } if content == "updated"
            ))
            .count(),
        1
    );
    assert!(
        !snapshot
            .history
            .iter()
            .any(|message| matches!(
                message, orbit_model::types::ChatMessage::User { content, .. }
                    if content == "original" || content == "remove me" || content.is_empty()
            ))
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
    let (_socket, _) = streaming_request(&listener).await;
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
            allow_missing: false,
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
    let (_socket, _) = streaming_request(&listener).await;
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
            allow_missing: false,
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

#[tokio::test]
async fn stopping_delivers_queued_files_and_messages_once_in_order() {
    use base64::Engine;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let fixture = Fixture::new(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
    let session = fixture.handle.info.id.clone();
    fixture
        .request(Request::Start {
            session: session.clone(),
            request_id: "generated-stop".into(),
            prompt: "Generate a summary".into(),
            images: vec![],
            compact: false,
            model: None,
        })
        .await
        .unwrap();
    let (_initial, _) = streaming_request(&listener).await;
    let source = "synthetic measurement = 73\n";
    fixture
        .request(Request::SubmitAttachments {
            session: session.clone(),
            request_id: "generated-file".into(),
            prompt: String::new(),
            images: vec![],
            files: vec![orbit_protocol::FileUpload {
                name: "measurement.txt".into(),
                data: base64::engine::general_purpose::STANDARD.encode(source),
            }],
            steer: true,
        })
        .await
        .unwrap();
    fixture
        .request(Request::Steer {
            session: session.clone(),
            message: "Compare the measurements".into(),
        })
        .await
        .unwrap();
    fixture
        .request(Request::Cancel {
            session: session.clone(),
        })
        .await
        .unwrap();
    let (_continued, request) = streaming_request(&listener).await;
    let users = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .map(|message| message["content"].as_str().unwrap())
        .filter(|content| {
            *content == "Generate a summary"
                || content.contains(source)
                || *content == "Compare the measurements"
        })
        .collect::<Vec<_>>();
    assert_eq!(users.len(), 3);
    assert_eq!(users[0], "Generate a summary");
    assert!(users[1].contains(source));
    assert!(users[1].contains("measurement.txt"));
    assert_eq!(users[2], "Compare the measurements");
    fixture
        .request(Request::Cancel {
            session: session.clone(),
        })
        .await
        .unwrap();
    fixture.wait(|page| !page.busy).await;
    assert!(
        fixture
            .handle
            .store
            .pending_agent_messages(&session)
            .await
            .unwrap()
            .is_empty()
    );
    let snapshot = fixture
        .handle
        .store
        .load(&session)
        .await
        .unwrap();
    assert_eq!(snapshot.runs.len(), 2);
    assert_eq!(snapshot.runs[1].status, "cancelled");
    assert!(
        fixture
            .page(0)
            .await
            .updates
            .iter()
            .any(|update| matches!(
                update, protocol::Update::Started { prompt, presentation }
                    if prompt.is_empty() && presentation.queued_input
            ))
    );
    assert_eq!(
        snapshot
            .history
            .iter()
            .filter(|message| matches!(message, orbit_model::types::ChatMessage::User { .. }))
            .count(),
        3
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn model_failure_keeps_queued_input_without_automatically_retrying() {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let fixture = Fixture::new(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
    let session = fixture.handle.info.id.clone();
    fixture
        .request(Request::Start {
            session: session.clone(),
            request_id: "generated-failure".into(),
            prompt: "Inspect generated material".into(),
            images: vec![],
            compact: false,
            model: None,
        })
        .await
        .unwrap();
    let (mut socket, _) = streaming_request(&listener).await;
    fixture
        .request(Request::Steer {
            session: session.clone(),
            message: "Pending synthetic instruction".into(),
        })
        .await
        .unwrap();
    let body = serde_json::json!({
        "error": { "message": "Synthetic invalid request", "type": "invalid_request_error" }
    })
    .to_string();
    socket
        .write_all(
            format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
            Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    fixture.wait(|page| !page.busy).await;
    fixture
        .request(Request::Cancel {
            session: session.clone(),
        })
        .await
        .unwrap();
    let queue = fixture
        .handle
        .store
        .pending_agent_messages(&session)
        .await
        .unwrap();
    assert_eq!(queue.len(), 1);
    assert_eq!(queue[0].content, "Pending synthetic instruction");
    let snapshot = fixture
        .handle
        .store
        .load(&session)
        .await
        .unwrap();
    assert_eq!(snapshot.runs.len(), 1);
    assert_eq!(snapshot.runs[0].status, "failed");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
}
