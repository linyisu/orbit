use super::*;
use clap::Parser;
use orbit_protocol::input_queue::{Action, Snapshot, State};

async fn request(backend: &Backend, request: Request) -> Result<Response, String> {
    let (result, receiver) = tokio::sync::oneshot::channel();
    backend
        .send(Command::Settings { request, result })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), receiver)
        .await
        .unwrap()
        .unwrap()
}

#[test]
fn queue_transport_uses_row_cursors_exact_content_and_run_receipts() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory
            .path()
            .join(".fixture-input-queue"),
        "",
    )
    .unwrap();
    let mut options = Options::parse_from(["orbit-gui"]);
    options.config = directory.path().join("unused.toml");
    options.workspace = Some(directory.path().into());
    options.server = Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into());
    options.database = Some(directory.path().join("unused.db"));
    options.auth_dir = Some(directory.path().join("auth"));
    options.model = Some("fixture::local".into());
    options.resume = Some("demo".into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (backend, mut events) = Backend::start(options, &runtime);
    runtime.block_on(async {
        let Event::Response(Response::Opened(info)) = until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        })
        .await
        else {
            unreachable!()
        };
        let command = |action| Request::InputQueue {
            session: info.id.clone(),
            expected_workspace: info.workspace.clone(),
            action,
        };
        let Response::InputQueue(Snapshot::List { entries, next, run }) =
            request(&backend, command(Action::List { after: None }))
                .await
                .unwrap()
        else {
            panic!("queue expected")
        };
        assert_eq!(entries.len(), 32);
        assert_eq!(next, Some(224));
        assert_eq!(run, Some(71));
        assert_eq!(entries[1].state, State::Claimed);
        let first = &entries[0];
        let edit = |expected: String, run| {
            command(Action::Edit {
                id: first.id,
                expected,
                content: "updated queue draft".into(),
                run,
            })
        };
        assert!(
            request(&backend, edit(first.content.clone(), Some(72)))
                .await
                .unwrap_err()
                .contains("运行已变化")
        );
        assert!(
            request(&backend, edit("stale content".into(), run))
                .await
                .unwrap_err()
                .contains("内容已变化")
        );
        std::fs::write(
            directory
                .path()
                .join(".fixture-input-queue-fail-once"),
            "",
        )
        .unwrap();
        assert!(
            request(&backend, edit(first.content.clone(), run))
                .await
                .unwrap_err()
                .contains("草稿应保留")
        );
        assert!(matches!(
            request(&backend, edit(first.content.clone(), run))
                .await
                .unwrap(),
            Response::InputQueue(Snapshot::Edited { entry })
                if entry.content == "updated queue draft"
        ));
        assert!(
            request(
                &backend,
                command(Action::Remove {
                    id: first.id,
                    expected: first.content.clone(),
                    run
                })
            )
            .await
            .unwrap_err()
            .contains("内容已变化")
        );
        assert!(matches!(
            request(
                &backend,
                command(Action::Remove {
                    id: first.id,
                    expected: "updated queue draft".into(),
                    run,
                }),
            )
            .await
            .unwrap(),
            Response::InputQueue(Snapshot::Removed { id }) if id == first.id
        ));
        let Response::InputQueue(Snapshot::List {
            entries: older,
            next,
            ..
        }) = request(&backend, command(Action::List { after: next }))
            .await
            .unwrap()
        else {
            panic!("older queue expected")
        };
        assert_eq!(older.len(), 3);
        assert_eq!(older[0].id, 231);
        assert!(next.is_none());
        std::fs::write(
            directory
                .path()
                .join(".fixture-input-queue-unknown-once"),
            "",
        )
        .unwrap();
        assert!(
            request(
                &backend,
                command(Action::Edit {
                    id: older[0].id,
                    expected: older[0].content.clone(),
                    content: "saved despite lost receipt".into(),
                    run
                })
            )
            .await
            .unwrap_err()
            .contains("回执已丢失")
        );
        let Response::InputQueue(Snapshot::List {
            entries: reconciled,
            ..
        }) = request(&backend, command(Action::List { after: Some(224) }))
            .await
            .unwrap()
        else {
            panic!("reconciliation expected")
        };
        assert_eq!(reconciled[0].content, "saved despite lost receipt");
        let claimed = &entries[1];
        assert!(
            request(
                &backend,
                command(Action::Remove {
                    id: claimed.id,
                    expected: claimed.content.clone(),
                    run
                })
            )
            .await
            .unwrap_err()
            .contains("已被取走")
        );
        assert!(
            request(
                &backend,
                Request::InputQueue {
                    session: info.id.clone(),
                    expected_workspace: "previous-workspace".into(),
                    action: Action::List { after: None }
                }
            )
            .await
            .unwrap_err()
            .contains("工作区已切换")
        );
        assert!(
            request(
                &backend,
                Request::InputQueue {
                    session: "other-session".into(),
                    expected_workspace: info.workspace.clone(),
                    action: Action::List { after: None }
                }
            )
            .await
            .unwrap_err()
            .contains("不属于当前会话")
        );
    });
}

#[test]
fn acknowledged_steers_are_visible_until_the_agent_consumes_or_the_user_removes_them() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory
            .path()
            .join(".fixture-input-queue-live"),
        "",
    )
    .unwrap();
    let mut options = Options::parse_from(["orbit-gui"]);
    options.config = directory.path().join("unused.toml");
    options.workspace = Some(directory.path().into());
    options.server = Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into());
    options.database = Some(directory.path().join("unused.db"));
    options.auth_dir = Some(directory.path().join("auth"));
    options.model = Some("fixture::local".into());
    options.resume = Some("demo".into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (backend, mut events) = Backend::start(options, &runtime);
    runtime.block_on(async {
        let Event::Response(Response::Opened(info)) = until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        })
        .await
        else {
            unreachable!()
        };
        let command = |action| Request::InputQueue {
            session: info.id.clone(),
            expected_workspace: info.workspace.clone(),
            action,
        };
        let mut first = None;
        for (id, text) in [
            ("first", "先检查现状\n再执行修改"),
            ("second", "暂时不要提交"),
        ] {
            backend
                .send(Command::Send {
                    session: info.id.clone(),
                    id: id.into(),
                    prompt: text.into(),
                    paths: vec![],
                    steer: true,
                })
                .unwrap();
            until(
                &mut events,
                |event| matches!(event, Event::Sent { id: received } if received == id),
            )
            .await;
            let Response::InputQueue(Snapshot::List { entries, run, .. }) =
                request(&backend, command(Action::List { after: None }))
                    .await
                    .unwrap()
            else {
                panic!("queue expected")
            };
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.content == text && entry.state == State::Queued)
            );
            first = Some((entries[0].clone(), run));
        }
        let (first, run) = first.unwrap();
        request(
            &backend,
            command(Action::Remove {
                id: first.id,
                expected: first.content,
                run,
            }),
        )
        .await
        .unwrap();
        let Response::InputQueue(Snapshot::List { entries, .. }) =
            request(&backend, command(Action::List { after: None }))
                .await
                .unwrap()
        else {
            panic!("queue expected")
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].content, "暂时不要提交");
        std::fs::write(
            directory
                .path()
                .join(".fixture-input-queue-consume-all"),
            "",
        )
        .unwrap();
        backend
            .send(Command::Request(Request::Poll {
                session: info.id.clone(),
                after: 0,
            }))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page))
                    if page.updates.iter().any(|update| {
                        matches!(
                            update,
                            orbit_protocol::Update::Agent(event)
                                if matches!(
                                    event.as_ref(),
                                    orbit_client::protocol::events::AgentEvent::InputReceived {
                                        content,
                                        ..
                                    } if content == "暂时不要提交"
                                )
                        )
                    })
            )
        })
        .await;
        let Response::InputQueue(Snapshot::List { entries, .. }) =
            request(&backend, command(Action::List { after: None }))
                .await
                .unwrap()
        else {
            panic!("queue expected")
        };
        assert!(entries.is_empty());
    });
}

#[test]
fn image_only_steer_uploads_and_downloads_a_durable_local_preview() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory
            .path()
            .join(".fixture-input-queue-live"),
        "",
    )
    .unwrap();
    let image = directory.path().join("input.png");
    image::RgbaImage::new(12, 8)
        .save(&image)
        .unwrap();
    let mut options = Options::parse_from(["orbit-gui"]);
    options.config = directory.path().join("unused.toml");
    options.workspace = Some(directory.path().into());
    options.preferences = Some(directory.path().join("gui.json"));
    options.server = Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into());
    options.database = Some(directory.path().join("unused.db"));
    options.auth_dir = Some(directory.path().join("auth"));
    options.model = Some("fixture::local".into());
    options.resume = Some("demo".into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (backend, mut events) = Backend::start(options, &runtime);
    let preview = runtime.block_on(async {
        let Event::Response(Response::Opened(info)) = until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        })
        .await
        else {
            unreachable!()
        };
        backend
            .send(Command::Send {
                session: info.id.clone(),
                id: "image-steer".into(),
                prompt: String::new(),
                paths: vec![(
                    image.clone(),
                    image
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                )],
                steer: true,
            })
            .unwrap();
        until(
            &mut events,
            |event| matches!(event, Event::Sent { id } if id == "image-steer"),
        )
        .await;
        let command = |action| Request::InputQueue {
            session: info.id.clone(),
            expected_workspace: info.workspace.clone(),
            action,
        };
        let Response::InputQueue(Snapshot::List { entries, run, .. }) =
            request(&backend, command(Action::List { after: None }))
                .await
                .unwrap()
        else {
            panic!("queue expected")
        };
        assert_eq!(entries.len(), 1);
        assert!(entries[0].content.is_empty());
        assert_eq!(entries[0].images.len(), 1);
        let preview = std::path::PathBuf::from(&entries[0].images[0].path);
        assert!(preview.is_file());
        assert_eq!(image::image_dimensions(&preview).unwrap(), (12, 8));
        request(
            &backend,
            command(Action::Remove {
                id: entries[0].id,
                expected: String::new(),
                run,
            }),
        )
        .await
        .unwrap();
        preview
    });
    drop(backend);
    drop(runtime);
    assert!(preview.is_file(), "withdrawn draft must survive disconnect");
}

#[test]
fn file_only_steer_preserves_content_and_original_name_in_the_local_queue() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory
            .path()
            .join(".fixture-input-queue-live"),
        "",
    )
    .unwrap();
    let image = directory.path().join("input.txt");
    std::fs::write(&image, b"  generated file payload\ntrailing whitespace  ").unwrap();
    let mut options = Options::parse_from(["orbit-gui"]);
    options.config = directory.path().join("unused.toml");
    options.workspace = Some(directory.path().into());
    options.preferences = Some(directory.path().join("gui.json"));
    options.server = Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into());
    options.database = Some(directory.path().join("unused.db"));
    options.auth_dir = Some(directory.path().join("auth"));
    options.model = Some("fixture::local".into());
    options.resume = Some("demo".into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (backend, mut events) = Backend::start(options, &runtime);
    let preview = runtime.block_on(async {
        let Event::Response(Response::Opened(info)) = until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        })
        .await
        else {
            unreachable!()
        };
        backend
            .send(Command::Send {
                session: info.id.clone(),
                id: "file-steer".into(),
                prompt: String::new(),
                paths: vec![(
                    image.clone(),
                    image
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                )],
                steer: true,
            })
            .unwrap();
        until(
            &mut events,
            |event| matches!(event, Event::Sent { id } if id == "file-steer"),
        )
        .await;
        let command = |action| Request::InputQueue {
            session: info.id.clone(),
            expected_workspace: info.workspace.clone(),
            action,
        };
        let Response::InputQueue(Snapshot::List { entries, run, .. }) =
            request(&backend, command(Action::List { after: None }))
                .await
                .unwrap()
        else {
            panic!("queue expected")
        };
        assert_eq!(entries.len(), 1);
        assert!(entries[0].content.is_empty());
        assert_eq!(entries[0].files.len(), 1);
        let preview = std::path::PathBuf::from(&entries[0].files[0].path);
        assert!(preview.is_file());
        assert_eq!(
            std::fs::read(&preview).unwrap(),
            std::fs::read(&image).unwrap()
        );
        assert_eq!(entries[0].files[0].name, "input.txt");
        request(
            &backend,
            command(Action::Remove {
                id: entries[0].id,
                expected: String::new(),
                run,
            }),
        )
        .await
        .unwrap();
        preview
    });
    drop(backend);
    drop(runtime);
    assert!(preview.is_file(), "withdrawn draft must survive disconnect");
}
