#[allow(dead_code)]
#[path = "../src/backend.rs"]
mod backend;

#[path = "support/files_transport.rs"]
mod files_transport;

#[path = "support/review_transport.rs"]
mod review_transport;

#[path = "support/side_questions_transport.rs"]
mod side_questions_transport;

#[path = "support/user_input_history_transport.rs"]
mod user_input_history_transport;

#[path = "support/subagents_transport.rs"]
mod subagents_transport;

#[path = "support/input_queue_transport.rs"]
mod input_queue_transport;

#[path = "support/tool_output_transport.rs"]
mod tool_output_transport;

#[path = "support/skill_import_transport.rs"]
mod skill_import_transport;

use backend::{Backend, Command, Event, Options};
use orbit_client::protocol::{Request, Response};
use std::time::Duration;
use tokio::sync::mpsc;

#[test]
fn mcp_start_permission_defaults_off_and_is_forwarded_to_each_target() {
    use clap::Parser;
    for target in [vec![], vec!["--ssh", "test-host"], vec!["--wsl", "Ubuntu"]] {
        let mut args = vec!["orbit-gui"];
        args.extend(target);
        let disabled = Options::parse_from(&args);
        assert!(
            !disabled
                .connection_options()
                .unwrap()
                .allow_mcp_start
        );
        args.push("--allow-mcp-start");
        let enabled = Options::parse_from(&args);
        assert!(
            enabled
                .connection_options()
                .unwrap()
                .allow_mcp_start
        );
        // Invocation-only grants must never become permanent via saved tabs.
        let restored: Options =
            serde_json::from_slice(&serde_json::to_vec(&enabled).unwrap()).unwrap();
        assert!(!restored.allow_mcp_start);
    }
}

#[test]
fn remote_options_preserve_target_paths_and_forward_authentication() {
    use clap::Parser;
    let mut options = Options::parse_from([
        "orbit-gui",
        "--ssh",
        "dev-alias",
        "--ssh-user",
        "builder",
        "--ssh-port",
        "2222",
        "--identity",
        "keys/development key",
        "--workspace",
        "~/workspace/项目 'one'",
        "--database",
        "state/orbit.db",
        "--server",
        "/opt/Orbit Service/orbit",
    ]);
    options.resolve_paths().unwrap();
    assert!(options.config.is_absolute());
    assert_eq!(
        options.workspace.as_deref(),
        Some(std::path::Path::new("~/workspace/项目 'one'"))
    );
    assert_eq!(
        options.database.as_deref(),
        Some(std::path::Path::new("state/orbit.db"))
    );
    let connection = options.connection_options().unwrap();
    assert_eq!(connection.workspace, "~/workspace/项目 'one'");
    assert_eq!(connection.ssh.as_deref(), Some("dev-alias"));
    assert_eq!(connection.ssh_user.as_deref(), Some("builder"));
    assert_eq!(connection.ssh_port, Some(2_222));
    assert_eq!(
        connection.server.as_deref(),
        Some("/opt/Orbit Service/orbit")
    );
    assert_eq!(connection.identity.as_deref(), options.identity.as_deref());
    assert!(std::path::Path::new(connection.identity.as_ref().unwrap()).is_absolute());
    let mut other = options.clone();
    other.ssh_user = Some("another-user".into());
    assert!(!options.same_host(&other));
    other = options.clone();
    other.ssh_port = Some(22);
    assert!(!options.same_host(&other));
    other = options.clone();
    other.identity = Some("~/.ssh/other-key".into());
    assert!(!options.same_host(&other));

    let mut wsl = Options::parse_from([
        "orbit-gui",
        "--wsl",
        "Ubuntu 24.04",
        "--wsl-user",
        "linux-user",
    ]);
    wsl.resolve_paths().unwrap();
    let connection = wsl.connection_options().unwrap();
    assert_eq!(connection.workspace, "~");
    assert_eq!(connection.wsl.as_deref(), Some("Ubuntu 24.04"));
    assert_eq!(connection.wsl_user.as_deref(), Some("linux-user"));
}

#[test]
fn remote_relative_databases_are_isolated_by_workspace() {
    use clap::Parser;
    for arguments in [
        ["orbit-gui", "--ssh", "dev"],
        ["orbit-gui", "--wsl", "Ubuntu"],
    ] {
        for database in [
            "state.db",
            "state/orbit.db",
            "./state.db",
            "~other/state.db",
        ] {
            let mut first = Options::parse_from(arguments);
            first.workspace = Some("/workspace/first".into());
            first.database = Some(database.into());
            first.resolve_paths().unwrap();
            let mut second = first.clone();
            assert!(first.same_host(&second));
            second.workspace = Some("/workspace/second".into());
            assert!(!first.same_host(&second), "{arguments:?}: {database}");
            assert!(!second.same_host(&first), "{arguments:?}: {database}");
        }
    }
}

#[test]
fn remote_shared_databases_can_span_workspaces() {
    use clap::Parser;
    for arguments in [
        ["orbit-gui", "--ssh", "dev"],
        ["orbit-gui", "--wsl", "Ubuntu"],
    ] {
        for database in [
            None,
            Some("/state/orbit.db"),
            Some("~/state/orbit.db"),
            Some("~"),
        ] {
            let mut first = Options::parse_from(arguments);
            first.workspace = Some("/workspace/first".into());
            first.database = database.map(Into::into);
            first.resolve_paths().unwrap();
            let mut second = first.clone();
            second.workspace = Some("/workspace/second".into());
            assert!(first.same_host(&second), "{arguments:?}: {database:?}");
            assert!(second.same_host(&first), "{arguments:?}: {database:?}");
        }
    }
}

#[test]
fn saved_remote_targets_restore_new_and_legacy_connection_fields() {
    use clap::Parser;
    let mut options = Options::parse_from(["orbit-gui", "--ssh", "dev"]);
    options.ssh_user = Some("builder".into());
    options.ssh_port = Some(2_222);
    options.identity = Some("~/.ssh/custom".into());
    let mut saved = serde_json::to_value(&options).unwrap();
    let restored: Options = serde_json::from_value(saved.clone()).unwrap();
    assert!(restored.same_host(&options));
    assert_eq!(
        restored
            .connection_options()
            .unwrap()
            .identity
            .as_deref(),
        Some("~/.ssh/custom")
    );
    for key in ["ssh_user", "ssh_port", "identity", "wsl_user"] {
        saved
            .as_object_mut()
            .unwrap()
            .remove(key);
    }
    let legacy: Options = serde_json::from_value(saved).unwrap();
    assert_eq!(legacy.ssh.as_deref(), Some("dev"));
    assert!(
        legacy.ssh_user.is_none()
            && legacy.ssh_port.is_none()
            && legacy.identity.is_none()
            && legacy.wsl_user.is_none()
    );
    assert!(Options::try_parse_from(["orbit-gui", "--ssh-user", "orphan"]).is_err());
    assert!(Options::try_parse_from(["orbit-gui", "--ssh", "dev", "--ssh-port", "0"]).is_err());
    assert!(Options::try_parse_from(["orbit-gui", "--ssh", "dev", "--wsl", "Ubuntu"]).is_err());
}

#[test]
fn connection_progress_precedes_workspace_open_on_controlled_service() {
    use clap::Parser;
    let directory = tempfile::tempdir().unwrap();
    let mut options = Options::parse_from(["orbit-gui"]);
    options.config = directory.path().join("unused.toml");
    options.workspace = Some(directory.path().into());
    options.server = Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into());
    options.database = Some(directory.path().join("unused.db"));
    options.auth_dir = Some(directory.path().join("auth"));
    options.model = Some("fixture::local".into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (_backend, mut events) = Backend::start(options, &runtime);
    runtime.block_on(async {
        let first = events
            .recv()
            .await
            .expect("initial connection event");
        assert!(matches!(first, Event::ConnectionStage(stage) if stage == "准备连接"));
        until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        })
        .await;
        until(
            &mut events,
            |event| matches!(event, Event::Response(Response::Page(page)) if !page.more),
        )
        .await;
    });
}

#[test]
fn first_send_recovery_checks_persistence_without_sending_a_message() {
    use clap::Parser;
    for (resume, recover_draft, expected) in [
        ("demo", true, "demo"),
        ("missing", true, "draft-1"),
        ("missing", false, "missing"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory
                .path()
                .join(".fixture-recovery"),
            "",
        )
        .unwrap();
        let mut options = Options::parse_from(["orbit-gui"]);
        options.config = directory.path().join("unused.toml");
        options.workspace = Some(directory.path().to_path_buf());
        options.server = Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into());
        options.database = Some(directory.path().join("unused.db"));
        options.auth_dir = Some(directory.path().join("auth"));
        options.model = Some("fixture::local".into());
        options.resume = Some(resume.into());
        options.recover_draft = recover_draft;
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
            assert_eq!(info.id, expected);
            until(&mut events, |event| {
                matches!(
                    event,
                    Event::Response(Response::Page(page)) if !page.more
                )
            })
            .await;
        });
        drop(backend);
        let requests = std::fs::read_to_string(
            directory
                .path()
                .join(".fixture-requests"),
        )
        .unwrap();
        for line in requests.lines() {
            let request: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(!matches!(
                request["method"].as_str(),
                Some("start" | "steer")
            ));
        }
    }
}

async fn until(events: &mut mpsc::Receiver<Event>, accept: impl Fn(&Event) -> bool) -> Event {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events
                .recv()
                .await
                .expect("backend dropped its event stream");
            if let Event::Disconnected(error) = &event {
                panic!("fixture disconnected: {error}");
            }
            if accept(&event) {
                return event;
            }
        }
    })
    .await
    .expect("fixture event timed out")
}

#[test]
fn project_tasks_use_typed_snapshots_and_report_dependency_errors() {
    use clap::Parser;
    use orbit_protocol::project::{TaskAction, TaskSnapshot, TaskStatus};
    async fn tasks(
        backend: &Backend,
        events: &mut mpsc::Receiver<Event>,
        workspace: &std::path::Path,
        action: TaskAction,
    ) -> Result<TaskSnapshot, String> {
        backend
            .send(Command::Tasks {
                session: "demo".into(),
                workspace: workspace.to_string_lossy().into_owned(),
                action,
            })
            .unwrap();
        let Event::Tasks { session, result } =
            until(events, |event| matches!(event, Event::Tasks { .. })).await
        else {
            unreachable!()
        };
        assert_eq!(session, "demo");
        result
    }
    let directory = tempfile::tempdir().unwrap();
    let mut options = Options::parse_from(["orbit-gui"]);
    options.config = directory.path().join("unused.toml");
    options.workspace = Some(directory.path().to_path_buf());
    options.server = Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into());
    options.database = Some(directory.path().join("unused.db"));
    options.auth_dir = Some(directory.path().join("auth"));
    options.model = Some("fixture::local".into());
    options.resume = Some("demo".into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (backend, mut events) = Backend::start(options, &runtime);
    runtime.block_on(async {
        until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        })
        .await;
        let error = tasks(
            &backend,
            &mut events,
            &directory
                .path()
                .join("previous-workspace"),
            TaskAction::Create {
                title: "陈旧表单不能创建这个任务".into(),
                details: String::new(),
                priority: 0,
                parent_id: None,
                depends_on: vec![],
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("工作区已切换"));
        assert!(
            tasks(
                &backend,
                &mut events,
                directory.path(),
                TaskAction::List {
                    include_completed: true
                }
            )
            .await
            .unwrap()
            .tasks
            .is_empty()
        );
        let first_snapshot = tasks(
            &backend,
            &mut events,
            directory.path(),
            TaskAction::Create {
                title: "基础任务".into(),
                details: "先完成接口".into(),
                priority: 1,
                parent_id: None,
                depends_on: vec![],
            },
        )
        .await
        .unwrap();
        let first = first_snapshot
            .tasks
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(first_snapshot.changed, Some(first.id));
        let all = tasks(
            &backend,
            &mut events,
            directory.path(),
            TaskAction::Create {
                title: "后续任务".into(),
                details: "依赖接口".into(),
                priority: 2,
                parent_id: None,
                depends_on: vec![first.id],
            },
        )
        .await
        .unwrap();
        let second = all
            .tasks
            .iter()
            .find(|task| task.id != first.id)
            .unwrap()
            .clone();
        assert_eq!(all.changed, Some(second.id));
        assert_eq!(second.blocked_by, [first.id]);
        assert!(
            tasks(
                &backend,
                &mut events,
                directory.path(),
                TaskAction::SetStatus {
                    id: second.id,
                    status: TaskStatus::InProgress
                }
            )
            .await
            .unwrap_err()
            .contains("未完成")
        );
        tasks(
            &backend,
            &mut events,
            directory.path(),
            TaskAction::SetStatus {
                id: first.id,
                status: TaskStatus::Completed,
            },
        )
        .await
        .unwrap();
        let all = tasks(
            &backend,
            &mut events,
            directory.path(),
            TaskAction::Update {
                id: second.id,
                title: Some("继续实现".into()),
                details: Some("保留草稿中的说明".into()),
                status: Some(TaskStatus::InProgress),
                priority: Some(3),
                depends_on: Some(vec![]),
            },
        )
        .await
        .unwrap();
        assert_eq!(all.changed, Some(second.id));
        let task = all
            .tasks
            .iter()
            .find(|task| task.id == second.id)
            .unwrap();
        assert_eq!(task.title, "继续实现");
        assert_eq!(task.details, "保留草稿中的说明");
        assert_eq!(task.priority, 3);
        assert!(task.blocked_by.is_empty());
        assert_eq!(
            tasks(
                &backend,
                &mut events,
                directory.path(),
                TaskAction::List {
                    include_completed: false
                }
            )
            .await
            .unwrap()
            .tasks
            .len(),
            1
        );
    });
}

#[test]
fn transport_retries_sends_and_resolves_approvals() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (backend, mut events) = Backend::start(
        Options {
            config: directory.path().join("unused.toml"),
            workspace: Some(directory.path().to_path_buf()),
            server: Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into()),
            database: Some(directory.path().join("unused.db")),
            auth_dir: Some(directory.path().join("auth")),
            model: Some("fixture::local".into()),
            resume: None,
            allow_mcp_start: false,
            relink_on_resume: false,
            recover_draft: false,
            appearance: None,
            preferences: None,
            ssh: None,
            ssh_user: None,
            ssh_port: None,
            identity: None,
            wsl: None,
            wsl_user: None,
        },
        &runtime,
    );
    runtime.block_on(async {
        let Event::Response(Response::Sessions(sessions)) = until(&mut events, |event| {
            matches!(event, Event::Response(Response::Sessions(_)))
        })
        .await
        else {
            unreachable!()
        };
        assert_eq!(sessions.len(), 3);
        assert!(
            sessions
                .iter()
                .any(|session| session.workspace == "/tmp/orbit-gui-notes")
        );
        let Event::Response(Response::Opened(info)) = until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        })
        .await
        else {
            unreachable!()
        };
        let session = info.id;
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page)) if page.updates.is_empty()
            )
        })
        .await;
        for (input, expected) in [
            ("/cd ", "/cd src/"),
            ("/cd src/", "/cd src/nested/"),
            ("/tasks done ", "/tasks done 42"),
            ("/diff src", "/diff src/main.rs"),
            ("/resume ", "/resume demo"),
        ] {
            backend
                .send(Command::Complete {
                    session: session.clone(),
                    input: input.into(),
                })
                .unwrap();
            let Event::Completions {
                session: returned_session,
                input: returned_input,
                result,
            } = until(&mut events, |event| {
                matches!(event, Event::Completions { .. })
            })
            .await
            else {
                unreachable!()
            };
            assert_eq!(returned_session, session);
            assert_eq!(returned_input, input);
            assert_eq!(result.unwrap()[0].name, expected);
        }
        backend
            .send(Command::Complete {
                session: session.clone(),
                input: "/cd denied".into(),
            })
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Completions {
                    result: Err(error), ..
                } if error == "目录不可访问"
            )
        })
        .await;

        let send = |id: &str, prompt: &str| Command::Send {
            session: session.clone(),
            id: id.into(),
            prompt: prompt.into(),
            paths: vec![],
            steer: false,
        };
        backend
            .send(send("retry-id", "fail"))
            .unwrap();
        until(
            &mut events,
            |event| matches!(event, Event::SendFailed { id, .. } if id == "retry-id"),
        )
        .await;
        backend
            .send(send("retry-id", "fail"))
            .unwrap();
        until(
            &mut events,
            |event| matches!(event, Event::Sent { id } if id == "retry-id"),
        )
        .await;
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page)) if !page.busy && !page.updates.is_empty()
            )
        })
        .await;
        backend
            .send(Command::Request(Request::Usage {
                session: session.clone(),
            }))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Usage {
                    result: Ok(snapshot),
                    ..
                } if snapshot.account.limits.len() == 2
            )
        })
        .await;
        backend
            .send(send("approval-id", "approve"))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page)) if page.approvals.len() == 1
            )
        })
        .await;
        backend
            .send(Command::Request(Request::Approve {
                session: session.clone(),
                approval: 7,
                allow: false,
                remember: false,
            }))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page)) if page.approvals.is_empty()
            )
        })
        .await;
        backend
            .send(Command::Request(Request::Cancel {
                session: session.clone(),
            }))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page)) if !page.busy
            )
        })
        .await;
        // Plan and question changes can arrive without transcript events.
        backend
            .send(Command::Request(Request::Plan {
                session: session.clone(),
                action: orbit_protocol::plan::Action::Enter,
            }))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page))
                    if page.planning.mode == orbit_protocol::plan::WorkMode::Plan
            )
        })
        .await;
        backend
            .send(Command::Request(Request::Plan {
                session: session.clone(),
                action: orbit_protocol::plan::Action::Approve { generation: 1 },
            }))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page))
                    if page
                        .planning
                        .plan
                        .as_ref()
                        .is_some_and(|plan| {
                            plan.status == orbit_protocol::plan::Status::Approved
                        })
            )
        })
        .await;
        backend
            .send(send("question-id", "question"))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page)) if !page.user_inputs.is_empty()
            )
        })
        .await;
        backend
            .send(Command::Request(Request::AnswerUserInput {
                session: session.clone(),
                request: "question-1".into(),
                answers: Some(vec![orbit_protocol::user_input::Answer {
                    question_id: "choice".into(),
                    selected: vec!["full".into()],
                    ..Default::default()
                }]),
            }))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page))
                    if page
                        .user_inputs
                        .first()
                        .is_some_and(|record| {
                            record.status == orbit_protocol::user_input::Status::Answered
                        })
            )
        })
        .await;
        backend
            .send(send("custom-question-id", "questions"))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page))
                    if page
                        .user_inputs
                        .first()
                        .is_some_and(|record| {
                            record.status == orbit_protocol::user_input::Status::Pending
                                && record.form.questions.len() == 2
                        })
            )
        })
        .await;
        let answers = vec![
            orbit_protocol::user_input::Answer {
                question_id: "choice".into(),
                text: "先验证交互".into(),
                ..Default::default()
            },
            orbit_protocol::user_input::Answer {
                question_id: "details".into(),
                selected: vec!["docs".into(), "tests".into()],
                text: "保留已有配置".into(),
                ..Default::default()
            },
        ];
        backend
            .send(Command::Request(Request::AnswerUserInput {
                session: session.clone(),
                request: "question-1".into(),
                answers: Some(answers.clone()),
            }))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page))
                    if page
                        .user_inputs
                        .first()
                        .is_some_and(|record| record.answers == answers)
            )
        })
        .await;
        backend
            .send(send("skip-question-id", "question"))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page))
                    if page
                        .user_inputs
                        .first()
                        .is_some_and(|record| {
                            record.status == orbit_protocol::user_input::Status::Pending
                        })
            )
        })
        .await;
        backend
            .send(Command::Request(Request::AnswerUserInput {
                session: session.clone(),
                request: "question-1".into(),
                answers: None,
            }))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page))
                    if page
                        .user_inputs
                        .first()
                        .is_some_and(|record| {
                            record.status == orbit_protocol::user_input::Status::Declined
                                && record.answers.is_empty()
                        })
            )
        })
        .await;
        backend
            .send(Command::Request(Request::ReadFile {
                session: session.clone(),
                path: "README.md".into(),
            }))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::File { path, bytes } if path == "README.md" && bytes == b"hello gui\n"
            )
        })
        .await;
        backend
            .send(Command::Request(Request::Ask {
                session: session.clone(),
                question: "旁路问题".into(),
                follow_up: None,
            }))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Questions(questions))
                    if questions
                        .records
                        .as_ref()
                        .is_some_and(|records| {
                            records
                                .iter()
                                .any(|record| record.answer == "旁路回答已完成")
                        })
            )
        })
        .await;
        backend
            .send(Command::ReadImage {
                session: session.clone(),
                path: "missing.png".into(),
            })
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Image {
                    path,
                    result: Err(error),
                    ..
                } if path == "missing.png" && error == "图片不存在"
            )
        })
        .await;
        backend
            .send(Command::Request(Request::ReadSubagent {
                session: session.clone(),
                agent: "child".into(),
                after: 0,
            }))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Subagent {
                    agent,
                    reset: true,
                    page,
                } if agent == "child" && page.next == 1
            )
        })
        .await;
        backend
            .send(Command::Request(Request::SendSubagent {
                session: session.clone(),
                agent: "child".into(),
                message: "后续输入".into(),
            }))
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Subagent {
                    reset: false, page, ..
                } if page.next == 2 && page.updates.len() == 1
            )
        })
        .await;
        backend
            .send(Command::CloseSubagent)
            .unwrap();
        backend
            .send(Command::Send {
                session,
                id: "steer-id".into(),
                prompt: "补充指令".into(),
                paths: vec![],
                steer: true,
            })
            .unwrap();
        until(
            &mut events,
            |event| matches!(event, Event::Sent { id } if id == "steer-id"),
        )
        .await;
    });
}

#[test]
fn independent_tasks_keep_running_and_scope_approvals_to_their_connection() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let options = Options {
        config: directory.path().join("unused.toml"),
        workspace: Some(directory.path().to_path_buf()),
        server: Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into()),
        database: Some(directory.path().join("unused.db")),
        auth_dir: Some(directory.path().join("auth")),
        model: Some("fixture::local".into()),
        resume: Some("task-a".into()),
        allow_mcp_start: false,
        relink_on_resume: false,
        recover_draft: false,
        appearance: None,
        preferences: None,
        ssh: None,
        ssh_user: None,
        ssh_port: None,
        identity: None,
        wsl: None,
        wsl_user: None,
    };
    let (first, mut first_events) = Backend::start(options.clone(), &runtime);
    runtime.block_on(async {
        until(&mut first_events, |event| {
            matches!(event, Event::Response(Response::Page(_)))
        })
        .await;
        first
            .send(Command::Send {
                session: "task-a".into(),
                id: "a".into(),
                prompt: "approve".into(),
                paths: vec![],
                steer: false,
            })
            .unwrap();
        until(&mut first_events, |event| {
            matches!(event,
            Event::Response(Response::Page(page)) if page
            .busy
            && page
            .approvals
            .len() == 1)
        })
        .await;
        let mut second_options = options.clone();
        second_options.resume = Some("task-b".into());
        let (second, mut second_events) = Backend::start(second_options, &runtime);
        until(&mut second_events, |event| {
            matches!(event,
            Event::Response(Response::Page(page)) if !page
            .busy
            && page
            .approvals
            .is_empty())
        })
        .await;
        second
            .send(Command::Send {
                session: "task-b".into(),
                id: "b".into(),
                prompt: "finish this task".into(),
                paths: vec![],
                steer: false,
            })
            .unwrap();
        until(&mut second_events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page))
                    if !page.busy
                        && page.updates.iter().any(|update| {
                            matches!(update, orbit_client::protocol::Update::Finished { .. })
                        })
            )
        })
        .await;
        // The first task was neither cancelled nor replaced by opening the second.
        first
            .send(Command::Request(Request::Poll {
                session: "task-a".into(),
                after: 0,
            }))
            .unwrap();
        until(&mut first_events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page)) if page.busy && page.approvals.len() == 1
            )
        })
        .await;
        first
            .send(Command::Request(Request::Approve {
                session: "task-a".into(),
                approval: 7,
                allow: false,
                remember: false,
            }))
            .unwrap();
        until(&mut first_events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page))
                    if !page.busy && page.approvals.is_empty()
            )
        })
        .await;
    });
    let mut other = options.clone();
    other.workspace = Some(directory.path().join("other-project"));
    other.resume = Some("task-b".into());
    assert!(options.same_host(&other));
    other.ssh = Some("different-host".into());
    assert!(!options.same_host(&other));
    other.ssh = None;
    other.database = Some(directory.path().join("another.db"));
    assert!(!options.same_host(&other));
}

#[test]
fn history_pages_drain_without_live_poll_delays_and_keep_cursor_order() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (_backend, mut events) = Backend::start(
        Options {
            config: directory.path().join("unused.toml"),
            workspace: Some(directory.path().to_path_buf()),
            server: Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into()),
            database: Some(directory.path().join("unused.db")),
            auth_dir: Some(directory.path().join("auth")),
            model: Some("fixture::local".into()),
            resume: Some("paged".into()),
            allow_mcp_start: false,
            relink_on_resume: false,
            recover_draft: false,
            appearance: None,
            preferences: None,
            ssh: None,
            ssh_user: None,
            ssh_port: None,
            identity: None,
            wsl: None,
            wsl_user: None,
        },
        &runtime,
    );
    runtime.block_on(async {
        until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        })
        .await;
        let started = std::time::Instant::now();
        for cursor in 1..=100 {
            let Event::Response(Response::Page(page)) = until(&mut events, |event| {
                matches!(event, Event::Response(Response::Page(_)))
            })
            .await
            else {
                unreachable!()
            };
            assert_eq!(page.next, cursor);
            assert_eq!(page.updates.len(), 1);
            assert_eq!(page.more, cursor < 100);
            let orbit_protocol::Update::Agent(event) = &page.updates[0] else {
                panic!("missing replay event")
            };
            assert!(matches!(
                event.as_ref(),
                orbit_agent::events::AgentEvent::Model(
                    orbit_model::types::StreamEvent::TextDelta(text),
                ) if text == &format!("{}\n", cursor - 1)
            ));
        }
        // Old behavior spent at least 7.9 seconds sleeping between these pages.
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "history took {:?}",
            started.elapsed()
        );
    });
}

#[test]
fn replacement_directory_links_before_resume_and_preserves_ownership_checks() {
    for (mode, explicit, succeeds, should_link) in [
        ("moved", true, true, true),
        ("moved", false, false, false),
        ("reject", true, false, true),
        ("linked", true, true, false),
    ] {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory
                .path()
                .join(".fixture-relocation"),
            mode,
        )
        .unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let (_backend, mut events) = Backend::start(
            Options {
                config: directory.path().join("unused.toml"),
                workspace: Some(directory.path().to_path_buf()),
                server: Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into()),
                database: Some(directory.path().join("unused.db")),
                auth_dir: Some(directory.path().join("auth")),
                model: Some("fixture::local".into()),
                resume: Some("demo".into()),
                allow_mcp_start: false,
                relink_on_resume: explicit,
                recover_draft: false,
                appearance: None,
                preferences: None,
                ssh: None,
                ssh_user: None,
                ssh_port: None,
                identity: None,
                wsl: None,
                wsl_user: None,
            },
            &runtime,
        );
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    match events
                        .recv()
                        .await
                        .expect("backend stopped")
                    {
                        Event::Response(Response::Sessions(sessions)) if succeeds => {
                            assert!(
                                sessions
                                    .iter()
                                    .filter(|s| s.project_id == "fixture")
                                    .all(|s| s.workspace == directory.path().to_string_lossy())
                            );
                        }
                        Event::Response(Response::Opened(info)) => {
                            assert!(succeeds);
                            assert_eq!(info.id, "demo");
                            assert_eq!(info.workspace, directory.path().to_string_lossy());
                            break;
                        }
                        Event::Disconnected(error) => {
                            assert!(!succeeds, "{error}");
                            assert!(
                                error.contains(if explicit {
                                    "another project's conversations"
                                } else {
                                    "session belongs to workspace"
                                }),
                                "{error}"
                            );
                            break;
                        }
                        _ => {}
                    }
                }
            })
            .await
            .unwrap();
        });
        let requests: Vec<serde_json::Value> = std::fs::read_to_string(
            directory
                .path()
                .join(".fixture-requests"),
        )
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
        let link = requests
            .iter()
            .position(|r| r["method"] == "admin");
        let open = requests
            .iter()
            .position(|r| r["method"] == "open");
        assert_eq!(link.is_some(), should_link);
        if should_link && succeeds {
            assert!(link.unwrap() < open.unwrap());
        }
        if mode == "reject" {
            assert!(open.is_none());
        }
    }
}

#[test]
fn session_controls_refresh_saved_catalog_and_reject_busy_deletion() {
    use clap::Parser;
    use orbit_protocol::admin::Action;

    async fn request(backend: &Backend, request: Request) -> Result<Response, String> {
        let (result, response) = tokio::sync::oneshot::channel();
        backend
            .send(Command::Settings { request, result })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), response)
            .await
            .unwrap()
            .unwrap()
    }

    let directory = tempfile::tempdir().unwrap();
    let mut options = Options::parse_from(["orbit-gui"]);
    options.config = directory.path().join("unused.toml");
    options.workspace = Some(directory.path().into());
    options.server = Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into());
    options.database = Some(directory.path().join("unused.db"));
    options.auth_dir = Some(directory.path().join("auth"));
    options.model = Some("fixture::local".into());
    options.resume = Some("demo".into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (backend, mut events) = Backend::start(options.clone(), &runtime);
    runtime.block_on(async {
        until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        })
        .await;
        request(
            &backend,
            Request::Admin {
                action: Action::RenameSession {
                    id: "navigation".into(),
                    title: "保存的历史会话".into(),
                },
            },
        )
        .await
        .unwrap();
        let Response::Sessions(sessions) = request(&backend, Request::AllSessions)
            .await
            .unwrap()
        else {
            panic!("missing saved catalog");
        };
        assert_eq!(
            sessions
                .iter()
                .find(|session| session.id == "navigation")
                .unwrap()
                .title,
            "保存的历史会话"
        );
        assert_eq!(
            sessions
                .iter()
                .find(|session| session.id == "notes")
                .unwrap()
                .title,
            "整理开发笔记"
        );
        assert!(
            request(
                &backend,
                Request::Admin {
                    action: Action::RenameSession {
                        id: "navigation".into(),
                        title: " ".into()
                    },
                }
            )
            .await
            .unwrap_err()
            .contains("1–120")
        );

        backend
            .send(Command::Send {
                session: "demo".into(),
                id: "session-controls-approval".into(),
                prompt: "approve".into(),
                paths: vec![],
                steer: false,
            })
            .unwrap();
        until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page)) if !page.approvals.is_empty()
            )
        })
        .await;
        assert!(
            request(
                &backend,
                Request::Admin {
                    action: Action::DeleteSession { id: "demo".into() },
                }
            )
            .await
            .unwrap_err()
            .contains("会话正在运行")
        );
        request(
            &backend,
            Request::Admin {
                action: Action::RenameSession {
                    id: "demo".into(),
                    title: "运行中重命名".into(),
                },
            },
        )
        .await
        .unwrap();
        let Response::Sessions(sessions) = request(&backend, Request::Sessions)
            .await
            .unwrap()
        else {
            panic!("missing project catalog");
        };
        assert_eq!(sessions[0].title, "运行中重命名");
        request(
            &backend,
            Request::Cancel {
                session: "demo".into(),
            },
        )
        .await
        .unwrap();
        request(
            &backend,
            Request::Admin {
                action: Action::DeleteSession {
                    id: "navigation".into(),
                },
            },
        )
        .await
        .unwrap();
        let Response::Sessions(sessions) = request(&backend, Request::AllSessions)
            .await
            .unwrap()
        else {
            panic!("missing refreshed catalog");
        };
        assert!(
            !sessions
                .iter()
                .any(|session| session.id == "navigation")
        );
        assert!(
            sessions
                .iter()
                .any(|session| session.id == "demo")
        );
    });
    drop(backend);

    // Separate workspace clients read the same test service catalog state.
    let (_backend, mut events) = Backend::start(options, &runtime);
    runtime.block_on(async {
        let Event::Response(Response::Sessions(sessions)) = until(&mut events, |event| {
            matches!(event, Event::Response(Response::Sessions(_)))
        })
        .await
        else {
            panic!("missing reopened catalog");
        };
        assert!(
            !sessions
                .iter()
                .any(|session| session.id == "navigation")
        );
        assert_eq!(
            sessions
                .iter()
                .find(|session| session.id == "demo")
                .unwrap()
                .title,
            "运行中重命名"
        );
    });
}

#[test]
fn settings_resources_round_trip_preserves_memory_category_and_workspace_scope() {
    use clap::Parser;
    use orbit_protocol::{
        project::{MemoryAction, MemoryCategory},
        resources::{Action, Snapshot},
    };

    async fn resources(
        backend: &Backend,
        workspace: &std::path::Path,
        action: Action,
    ) -> Result<Snapshot, String> {
        let (result, receive) = tokio::sync::oneshot::channel();
        backend
            .send(Command::Settings {
                request: Request::Resources {
                    session: "demo".into(),
                    expected_workspace: Some(workspace.to_string_lossy().into_owned()),
                    action,
                },
                result,
            })
            .unwrap();
        match tokio::time::timeout(Duration::from_secs(10), receive)
            .await
            .unwrap()
            .unwrap()?
        {
            Response::Resources(snapshot) => Ok(snapshot),
            other => panic!("expected settings snapshot, got {other:?}"),
        }
    }

    let directory = tempfile::tempdir().unwrap();
    let mut options = Options::parse_from(["orbit-gui"]);
    options.config = directory.path().join("unused.toml");
    options.workspace = Some(directory.path().to_path_buf());
    options.server = Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into());
    options.database = Some(directory.path().join("unused.db"));
    options.auth_dir = Some(directory.path().join("auth"));
    options.model = Some("fixture::local".into());
    options.resume = Some("demo".into());
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (backend, mut events) = Backend::start(options, &runtime);
    runtime.block_on(async {
        until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        })
        .await;
        let content = "---\ndescription: Review code\n---\nInspect the diff.\n";
        let Snapshot::Skills(skills) = resources(
            &backend,
            directory.path(),
            Action::SaveSkill {
                name: "review".into(),
                original: None,
                content: content.into(),
            },
        )
        .await
        .unwrap() else {
            panic!("skills expected")
        };
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "review");
        let Snapshot::Skill {
            content: saved,
            editable,
            ..
        } = resources(
            &backend,
            directory.path(),
            Action::ReadSkill {
                name: "review".into(),
            },
        )
        .await
        .unwrap()
        else {
            panic!("skill expected")
        };
        assert_eq!(saved, content);
        assert!(editable);
        assert!(
            resources(
                &backend,
                &directory.path().join("previous"),
                Action::SaveSkill {
                    name: "stale".into(),
                    original: None,
                    content: content.into(),
                }
            )
            .await
            .unwrap_err()
            .contains("工作区已切换")
        );
        let Snapshot::Skills(skills) = resources(&backend, directory.path(), Action::Skills)
            .await
            .unwrap()
        else {
            panic!("skills expected")
        };
        assert_eq!(skills.len(), 1);
        for category in MemoryCategory::ALL {
            let Snapshot::Memories(memories) = resources(
                &backend,
                directory.path(),
                Action::Memory(MemoryAction::Add {
                    category,
                    content: format!("saved {}", category.as_str()),
                }),
            )
            .await
            .unwrap() else {
                panic!("memories expected")
            };
            assert_eq!(memories.last().unwrap().category, category);
            assert_eq!(memories.last().unwrap().session_id, "demo");
        }
        let Snapshot::Memories(memories) = resources(&backend, directory.path(), Action::Memories)
            .await
            .unwrap()
        else {
            panic!("memories expected")
        };
        assert_eq!(memories.len(), 4);
        let id = memories[0].id;
        assert!(
            resources(
                &backend,
                &directory.path().join("previous"),
                Action::Memory(MemoryAction::Forget { id })
            )
            .await
            .is_err()
        );
        let Snapshot::Memories(memories) = resources(
            &backend,
            directory.path(),
            Action::Memory(MemoryAction::Forget { id }),
        )
        .await
        .unwrap() else {
            panic!("memories expected")
        };
        assert_eq!(memories.len(), 3);
        assert!(
            memories
                .iter()
                .all(|memory| memory.id != id)
        );
        let Snapshot::Mcp(servers) = resources(&backend, directory.path(), Action::Mcp)
            .await
            .unwrap()
        else {
            panic!("MCP expected")
        };
        assert!(servers[0].connected);
        assert_eq!(servers[0].tools[0].name, "mcp__fixture__echo");
        assert_eq!(servers[0].tools[0].description, "Echo input");
        let Snapshot::Mcp(reloaded) = resources(
            &backend,
            directory.path(),
            Action::ReloadMcp {
                config: serde_json::json!({
                    "servers": {}
                }),
                allow_start: true,
            },
        )
        .await
        .unwrap() else {
            panic!("MCP reload expected")
        };
        assert!(reloaded[0].connected);
        std::fs::write(
            directory
                .path()
                .join(".fixture-hold-mcp-reload"),
            "hold",
        )
        .unwrap();
        let (result, pending) = tokio::sync::oneshot::channel();
        backend
            .send(Command::Settings {
                request: Request::Resources {
                    session: "demo".into(),
                    expected_workspace: Some(directory.path().display().to_string()),
                    action: Action::ReloadMcp {
                        config: serde_json::json!({
                            "servers": {}
                        }),
                        allow_start: true,
                    },
                },
                result,
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !directory
                .path()
                .join(".fixture-mcp-reload-started")
                .exists()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("reload must start on its own connection");
        // A slow handshake must leave the main GUI connection available.
        tokio::time::timeout(
            Duration::from_secs(1),
            resources(&backend, directory.path(), Action::Mcp),
        )
        .await
        .expect("MCP reconnect blocked the main connection")
        .unwrap();
        std::fs::write(
            directory
                .path()
                .join(".fixture-release-mcp-reload"),
            "ready",
        )
        .unwrap();
        assert!(matches!(
            pending.await.unwrap().unwrap(),
            Response::Resources(Snapshot::Mcp(_))
        ));
        assert!(
            resources(&backend, &directory.path().join("previous"), Action::Mcp)
                .await
                .is_err()
        );
    });
}
