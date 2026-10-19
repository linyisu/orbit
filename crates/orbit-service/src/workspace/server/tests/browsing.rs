use super::*;

#[tokio::test]
async fn remote_browsing_retains_restrictive_policy_through_execution_preparation() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let source = fs::read_to_string(&fixture.service.options.config).unwrap();
    let configuration = orbit_protocol::Configuration {
        content: format!("disabled_capabilities = [\"files\"]\n{source}"),
        credentials: Default::default(),
    };
    let Response::Opened(info) = fixture
        .request(Request::Browse {
            resume: None,
            configuration: Some(Ok(configuration)),
        })
        .await
        .unwrap()
    else {
        panic!("missing browser")
    };
    let read = || Request::ReadFile {
        session: info.id.clone(),
        path: "generated.txt".into(),
        allow_missing: true,
    };
    assert!(
        fixture
            .request(read())
            .await
            .unwrap_err()
            .to_string()
            .contains("文件功能已禁用")
    );
    let installed_path = fixture
        .service
        .browsing(&info.id)
        .await
        .unwrap()
        .options(&fixture.service.options)
        .unwrap()
        .config
        .clone();
    fixture
        .request(Request::Prepare {
            session: info.id.clone(),
            model: Some("test::test".into()),
            configuration: None,
        })
        .await
        .unwrap();
    let handle = fixture
        .service
        .session(&info.id)
        .await
        .unwrap();
    assert!(
        handle
            .policy
            .disabled(orbit_protocol::Capability::Files)
    );
    assert!(installed_path.exists());
    assert!(
        fixture
            .request(Request::Files {
                session: info.id,
                expected_workspace: None,
                action: orbit_protocol::files::Action::List {
                    path: ".".into(),
                    offset: 0,
                    include_hidden: false
                },
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn failed_remote_policy_capture_preserves_history_and_rejects_execution_until_repaired() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    for configuration in [
        Some(Err(())),
        Some(Ok(orbit_protocol::Configuration {
            content: "invalid = [".into(),
            credentials: Default::default(),
        })),
    ] {
        let Response::Opened(info) = fixture
            .request(Request::Browse {
                resume: None,
                configuration,
            })
            .await
            .unwrap()
        else {
            panic!("missing browser")
        };
        assert!(matches!(
            fixture
                .request(Request::Poll {
                    session: info.id.clone(),
                    after: 0
                })
                .await
                .unwrap(),
            Response::Page(_)
        ));
        assert!(
            fixture
                .request(Request::ReadFile {
                    session: info.id.clone(),
                    path: "generated.txt".into(),
                    allow_missing: true
                })
                .await
                .is_err()
        );
        assert!(
            fixture
                .request(Request::Prepare {
                    session: info.id.clone(),
                    model: Some("test::test".into()),
                    configuration: None
                })
                .await
                .is_err()
        );
        let source = fs::read_to_string(&fixture.service.options.config).unwrap();
        fixture
            .request(Request::Prepare {
                session: info.id,
                model: Some("test::test".into()),
                configuration: Some(orbit_protocol::Configuration {
                    content: source,
                    credentials: Default::default(),
                }),
            })
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn browsing_keeps_session_administration_and_archived_views_consistent() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    fs::remove_file(&fixture.service.options.config).unwrap();
    let Response::Opened(info) = fixture
        .request(Request::Browse {
            resume: None,
            configuration: None,
        })
        .await
        .unwrap()
    else {
        panic!("missing draft");
    };
    fixture
        .service
        .browsing(&info.id)
        .await
        .unwrap()
        .session
        .persist()
        .await
        .unwrap();
    fixture
        .request(Request::Admin {
            action: orbit_protocol::admin::Action::RenameSession {
                id: info.id.clone(),
                title: "Generated archive".into(),
            },
        })
        .await
        .unwrap();
    let Response::Opened(renamed) = fixture
        .request(Request::Browse {
            configuration: None,
            resume: Some(info.id.clone()),
        })
        .await
        .unwrap()
    else {
        panic!("missing renamed conversation");
    };
    assert_eq!(renamed.title, "Generated archive");
    let children = fixture
        .request(Request::Subagents {
            session: info.id.clone(),
            expected_workspace: info.workspace.clone(),
            action: orbit_protocol::subagents::Action::List { offset: 0 },
        })
        .await
        .unwrap();
    assert!(matches!(children,
        Response::Subagents(orbit_protocol::subagents::Snapshot::List { agents, .. }) if agents.is_empty()
    ));
    for section in [
        orbit_protocol::Inspection::Diagnostics,
        orbit_protocol::Inspection::Changes,
        orbit_protocol::Inspection::Status,
        orbit_protocol::Inspection::Mcp,
    ] {
        assert!(matches!(
            fixture
                .request(Request::SessionAction {
                    session: info.id.clone(),
                    action: orbit_protocol::SessionAction::Inspect { section },
                })
                .await
                .unwrap(),
            Response::Report(_)
        ));
    }
    fixture
        .request(Request::Admin {
            action: orbit_protocol::admin::Action::DeleteSession {
                id: info.id.clone(),
            },
        })
        .await
        .unwrap();
    assert!(
        fixture
            .request(Request::Poll {
                session: info.id.clone(),
                after: 0
            })
            .await
            .is_err()
    );
    assert!(
        fixture
            .request(Request::Browse {
                configuration: None,
                resume: Some(info.id)
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn shutdown_waits_for_browsing_operations_and_rejects_new_sessions() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let Response::Opened(info) = fixture
        .request(Request::Browse {
            resume: None,
            configuration: None,
        })
        .await
        .unwrap()
    else {
        panic!("missing draft");
    };
    let operation = fixture
        .service
        .browsing(&info.id)
        .await
        .unwrap();
    assert!(
        fixture
            .request(Request::Shutdown)
            .await
            .is_err()
    );
    drop(operation);
    assert!(matches!(
        fixture
            .request(Request::Shutdown)
            .await
            .unwrap(),
        Response::Ok
    ));
    assert!(
        fixture
            .request(Request::Browse {
                resume: None,
                configuration: None
            })
            .await
            .is_err()
    );
    assert!(
        fixture
            .request(Request::Prepare {
                session: info.id,
                model: Some("test::test".into()),
                configuration: None,
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn session_extensions_and_memory_views_work_before_model_setup() {
    use orbit_protocol::resources::{Action, Snapshot};
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    fs::remove_file(&fixture.service.options.config).unwrap();
    let Response::Opened(info) = fixture
        .request(Request::Browse {
            resume: None,
            configuration: None,
        })
        .await
        .unwrap()
    else {
        panic!("missing draft");
    };
    let request = |action| Request::Resources {
        session: info.id.clone(),
        expected_workspace: Some(info.workspace.clone()),
        action,
    };
    let checkpoint = fixture
        .request(Request::Review {
            session: info.id.clone(),
            expected_workspace: info.workspace.clone(),
            action: orbit_protocol::review::Action::List {
                kind: orbit_protocol::review::Kind::Checkpoint,
                offset: 0,
            },
        })
        .await
        .unwrap();
    assert!(matches!(checkpoint,
        Response::Review(orbit_protocol::review::Snapshot::List { entries, .. })
            if entries.is_empty()
    ));
    for action in [
        Action::Skills,
        Action::Memories,
        Action::Mcp,
        Action::AppliedExtensions,
    ] {
        assert!(matches!(
            fixture
                .request(request(action))
                .await
                .unwrap(),
            Response::Resources(_)
        ));
    }
    let bundle = super::super::super::library::test_bundle(
        "generated-skill",
        "---\ndescription: Generated skill\n---\nPreserve public identifiers.",
    );
    let mut invalid = bundle.clone();
    invalid.skill_roots = vec!["../outside".into()];
    assert!(
        fixture
            .request(request(Action::ApplyExtension {
                bundle: Box::new(invalid)
            }))
            .await
            .is_err()
    );
    assert!(
        fixture
            .handle
            .store
            .session(&info.id)
            .await
            .is_err()
    );
    let applied = fixture
        .request(request(Action::ApplyExtension {
            bundle: Box::new(bundle),
        }))
        .await
        .unwrap();
    assert!(matches!(applied,
        Response::Resources(Snapshot::Extensions(items)) if items.len() == 1
    ));
    let skills = fixture
        .request(request(Action::Skills))
        .await
        .unwrap();
    assert!(matches!(skills,
        Response::Resources(Snapshot::Skills(items)) if items.len() == 1
    ));
    let skill = fixture
        .request(request(Action::ReadSkill {
            name: "generated-skill".into(),
        }))
        .await
        .unwrap();
    assert!(matches!(skill,
        Response::Resources(Snapshot::Skill { content, .. })
            if content.contains("Preserve public identifiers.")
    ));
    assert!(matches!(
        fixture
            .request(Request::SessionAction {
                session: info.id.clone(),
                action: orbit_protocol::SessionAction::LoadSkill {
                    name: "generated-skill".into()
                },
            })
            .await
            .unwrap(),
        Response::Report(_)
    ));
    assert!(
        fixture
            .handle
            .store
            .session(&info.id)
            .await
            .is_ok()
    );
    fs::write(
        &fixture.service.options.config,
        "disabled_capabilities = ['all']\n[providers]\n",
    )
    .unwrap();
    for action in [
        Action::Skills,
        Action::Memories,
        Action::Mcp,
        Action::AppliedExtensions,
    ] {
        assert!(
            fixture
                .request(request(action))
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn terminal_directory_changes_and_task_reports_do_not_require_a_model() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let config = &fixture.service.options.config;
    let source = fs::read_to_string(config).unwrap();
    fs::write(config, "[providers]\n").unwrap();
    let chosen = fixture
        ._root
        .path()
        .join("generated directory");
    fs::create_dir(&chosen).unwrap();
    fs::write(chosen.join("marker.txt"), "chosen directory").unwrap();
    let Response::Opened(info) = fixture
        .request(Request::Browse {
            resume: None,
            configuration: None,
        })
        .await
        .unwrap()
    else {
        panic!("missing conversation");
    };
    let Response::Profile(moved) = fixture
        .request(Request::SessionAction {
            session: info.id.clone(),
            action: orbit_protocol::SessionAction::ChangeDirectory {
                path: format!("\"{}\"", chosen.display()),
            },
        })
        .await
        .unwrap()
    else {
        panic!("missing directory profile");
    };
    assert_eq!(moved.id, info.id);
    assert_eq!(
        moved.workspace,
        chosen
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
    );
    assert!(matches!(
        fixture
            .request(Request::ReadFile {
                session: info.id.clone(),
                path: "marker.txt".into(),
                allow_missing: false,
            })
            .await
            .unwrap(),
        Response::File(_)
    ));
    assert!(matches!(
        fixture
            .request(Request::SessionAction {
                session: info.id.clone(),
                action: orbit_protocol::SessionAction::Task {
                    task: orbit_protocol::project::TaskAction::List {
                        include_completed: true
                    },
                },
            })
            .await
            .unwrap(),
        Response::Report(_)
    ));
    let Response::Tasks(tasks) = fixture
        .request(Request::Tasks {
            session: info.id.clone(),
            expected_workspace: Some(moved.workspace.clone()),
            action: orbit_protocol::project::TaskAction::List {
                include_completed: true,
            },
        })
        .await
        .unwrap()
    else {
        panic!("missing desktop task snapshot");
    };
    assert!(tasks.tasks.is_empty());
    fs::write(config, source).unwrap();
    let Response::Profile(ready) = fixture
        .request(Request::Prepare {
            session: info.id.clone(),
            model: Some("test::test".into()),
            configuration: None,
        })
        .await
        .unwrap()
    else {
        panic!("missing execution profile");
    };
    assert_eq!(ready.workspace, moved.workspace);
    assert_eq!(
        fixture
            .service
            .session(&info.id)
            .await
            .unwrap()
            .workspace
            .read()
            .unwrap()
            .root(),
        chosen.canonicalize().unwrap()
    );
}

#[tokio::test]
async fn files_remain_available_before_model_setup_with_the_same_write_and_path_boundaries() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let workspace = &fixture
        .service
        .options
        .initial_directory;
    fs::write(workspace.join("generated.txt"), "before").unwrap();
    fs::write(fixture.external.join("outside.txt"), "private").unwrap();
    fs::remove_file(&fixture.service.options.config).unwrap();
    let Response::Opened(info) = fixture
        .request(Request::Browse {
            resume: None,
            configuration: None,
        })
        .await
        .unwrap()
    else {
        panic!("conversation metadata missing");
    };
    let file = || Request::ReadFile {
        session: info.id.clone(),
        path: "generated.txt".into(),
        allow_missing: false,
    };
    assert!(matches!(
        fixture.request(file()).await.unwrap(),
        Response::File(_)
    ));
    assert!(
        fixture
            .request(Request::ReadFile {
                session: info.id.clone(),
                path: fixture
                    .external
                    .join("outside.txt")
                    .display()
                    .to_string(),
                allow_missing: false,
            })
            .await
            .is_err()
    );
    assert!(matches!(
        fixture
            .request(Request::Files {
                session: info.id.clone(),
                expected_workspace: Some(info.workspace.clone()),
                action: orbit_protocol::files::Action::List {
                    path: ".".into(),
                    offset: 0,
                    include_hidden: false
                },
            })
            .await
            .unwrap(),
        Response::Files(_)
    ));
    let save = || Request::Files {
        session: info.id.clone(),
        expected_workspace: Some(info.workspace.clone()),
        action: orbit_protocol::files::Action::Save {
            path: "generated.txt".into(),
            original: "before".into(),
            content: "after".into(),
        },
    };
    fixture
        .request(Request::Plan {
            session: info.id.clone(),
            action: orbit_protocol::plan::Action::Enter,
        })
        .await
        .unwrap();
    assert!(fixture.request(save()).await.is_err());
    assert_eq!(
        fs::read_to_string(workspace.join("generated.txt")).unwrap(),
        "before"
    );
    fixture
        .request(Request::Plan {
            session: info.id.clone(),
            action: orbit_protocol::plan::Action::Leave,
        })
        .await
        .unwrap();
    fixture.request(save()).await.unwrap();
    assert_eq!(
        fs::read_to_string(workspace.join("generated.txt")).unwrap(),
        "after"
    );
    assert!(fixture.request(save()).await.is_err());
    assert!(
        fixture
            .service
            .session(&info.id)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn browsing_survives_missing_and_invalid_models_and_prepares_after_configuration_is_fixed() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let config = &fixture.service.options.config;
    let source = fs::read_to_string(config).unwrap();
    fs::remove_file(config).unwrap();
    let Response::Opened(info) = fixture
        .request(Request::Browse {
            resume: None,
            configuration: None,
        })
        .await
        .unwrap()
    else {
        panic!("missing conversation metadata");
    };
    assert_eq!(info.phase, orbit_protocol::SessionPhase::Browsing);
    assert!(!info.persisted);
    assert!(
        fixture
            .handle
            .store
            .list_resumable()
            .await
            .unwrap()
            .is_empty()
    );
    for content in [None, Some("invalid = [")] {
        if let Some(content) = content {
            fs::write(config, content).unwrap();
        }
        assert!(
            fixture
                .request(Request::Prepare {
                    session: info.id.clone(),
                    model: Some("test::test".into()),
                    configuration: None,
                })
                .await
                .is_err()
        );
        let Response::Page(page) = fixture
            .request(Request::Poll {
                session: info.id.clone(),
                after: 0,
            })
            .await
            .unwrap()
        else {
            panic!("history must remain readable after failed preparation");
        };
        assert!(!page.busy);
        assert!(page.updates.is_empty());
    }
    fs::write(config, source).unwrap();
    fixture
        .request(Request::SetMode {
            session: info.id.clone(),
            mode: Mode::Ask,
        })
        .await
        .unwrap();
    fixture
        .request(Request::Plan {
            session: info.id.clone(),
            action: orbit_protocol::plan::Action::Enter,
        })
        .await
        .unwrap();
    let Response::Profile(ready) = fixture
        .request(Request::Prepare {
            session: info.id.clone(),
            model: Some("test::test".into()),
            configuration: None,
        })
        .await
        .unwrap()
    else {
        panic!("execution profile missing");
    };
    assert_eq!(ready.id, info.id);
    assert_eq!(ready.phase, orbit_protocol::SessionPhase::Ready);
    assert_eq!(ready.mode, Mode::Ask);
    assert!(!ready.persisted);
    let handle = fixture
        .service
        .session(&info.id)
        .await
        .unwrap();
    let page = handle.page(0, 0).await.unwrap();
    assert_eq!(page.planning.mode, orbit_protocol::plan::WorkMode::Plan);
    assert!(!page.busy);
    assert!(
        fixture
            .handle
            .store
            .list_resumable()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn saved_transcript_is_readable_without_model_or_execution_directory_authorization() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let mut writer = fixture
        .handle
        .store
        .create(
            fixture.external.display().to_string(),
            "removed::generated-model".into(),
        )
        .await
        .unwrap();
    let id = writer.id().to_owned();
    writer
        .begin(
            "generated archived question".into(),
            "removed::generated-model".into(),
        )
        .await
        .unwrap();
    writer
        .finish(
            &orbit_store::types::RunCompletion::Completed {
                history: vec![orbit_model::types::ChatMessage::user(
                    "generated archived question",
                )],
            },
            vec![],
        )
        .await
        .unwrap();
    drop(writer);
    fs::write(&fixture.service.options.config, "invalid = [").unwrap();
    let Response::Opened(info) = fixture
        .request(Request::Browse {
            configuration: None,
            resume: Some(id.clone()),
        })
        .await
        .unwrap()
    else {
        panic!("missing archived conversation");
    };
    assert_eq!(
        info.workspace,
        fixture
            .external
            .canonicalize()
            .unwrap()
            .display()
            .to_string()
    );
    let Response::Page(page) = fixture
        .request(Request::Poll {
            session: id.clone(),
            after: 0,
        })
        .await
        .unwrap()
    else {
        panic!("missing transcript");
    };
    assert_eq!(page.updates.len(), 2);
    assert!(!page.more);
    assert!(
        fixture
            .request(Request::Prepare {
                session: id.clone(),
                model: None,
                configuration: None
            })
            .await
            .is_err()
    );
    let Response::Page(retry) = fixture
        .request(Request::Poll {
            session: id.clone(),
            after: 0,
        })
        .await
        .unwrap()
    else {
        panic!("failed preparation must preserve the archive");
    };
    assert_eq!(
        serde_json::to_value(page.updates).unwrap(),
        serde_json::to_value(retry.updates).unwrap()
    );
    assert!(
        fixture
            .service
            .session(&id)
            .await
            .is_err()
    );
    assert_eq!(
        fixture
            .handle
            .store
            .session(&id)
            .await
            .unwrap()
            .model,
        "removed::generated-model"
    );
    fixture
        .service
        .release_client(vec![id.clone()], 0)
        .await;
    assert!(
        fixture
            .service
            .browsing(&id)
            .await
            .is_none()
    );
}
