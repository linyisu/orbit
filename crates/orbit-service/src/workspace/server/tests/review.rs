use super::*;
use orbit_protocol::review::{Action, Kind, Snapshot};

async fn actor_review(handle: &Handle, workspace: &str, action: Action) -> Result<Snapshot> {
    let (result, response) = tokio::sync::oneshot::channel();
    handle
        .command(session::Command::Review {
            action,
            expected_workspace: workspace.into(),
            result,
        })
        .await?;
    response.await?
}

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", orbit_runtime::git::NULL_DEVICE)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn initialize_git(root: &Path) {
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Orbit"]);
    git(root, &["config", "user.email", "test@example.invalid"]);
    fs::write(root.join("tracked.txt"), "base\n").unwrap();
    git(root, &["add", "tracked.txt"]);
    git(root, &["commit", "-qm", "base"]);
    fs::write(root.join("tracked.txt"), "working\n").unwrap();
}

#[tokio::test]
async fn review_uses_published_workspace_identity_after_opening_a_path_alias() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let root = &fixture
        .service
        .options
        .initial_directory;
    initialize_git(root);
    let alias = root.join("..").join("workspace");
    let setup = crate::workspace::setup::prepare_workspace(&fixture.service.options, &alias, None)
        .await
        .unwrap();
    let handle = Handle::spawn(setup, None)
        .await
        .unwrap();
    let alias = alias.to_string_lossy();
    assert_ne!(alias, handle.info.workspace);
    let action = || Action::History {
        revision: "HEAD".into(),
        offset: 0,
    };
    assert!(
        actor_review(&handle, &alias, action())
            .await
            .unwrap_err()
            .to_string()
            .contains("工作区已切换")
    );
    assert!(matches!(
        actor_review(&handle, &handle.info.workspace, action()).await.unwrap(),
        Snapshot::History { commits, .. } if commits.len() == 1
    ));
    handle
        .command(session::Command::Close)
        .await
        .unwrap();
}

#[tokio::test]
async fn history_keeps_readonly_access_but_respects_disabled_capabilities() {
    for disabled in [
        None,
        Some(orbit_protocol::Capability::Files),
        Some(orbit_protocol::Capability::Git),
    ] {
        let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
        let root = &fixture
            .service
            .options
            .initial_directory;
        initialize_git(root);
        let mut setup =
            crate::workspace::setup::prepare_workspace(&fixture.service.options, root, None)
                .await
                .unwrap();
        setup.resources.policy.read_only = true;
        setup.resources.policy.disabled = disabled.into_iter().collect();
        let handle = Handle::spawn(setup, None)
            .await
            .unwrap();
        let result = actor_review(
            &handle,
            &handle.info.workspace,
            Action::History {
                revision: "HEAD".into(),
                offset: 0,
            },
        )
        .await;
        if disabled.is_some() {
            assert!(result.is_err());
        } else {
            assert!(
                matches!(result.unwrap(), Snapshot::History { commits, .. } if commits.len() == 1)
            );
        }
        let patch = actor_review(
            &handle,
            &handle.info.workspace,
            Action::Commit {
                revision: "HEAD".into(),
                include_patch: true,
            },
        )
        .await;
        if disabled.is_some() {
            assert!(patch.is_err());
        } else {
            assert!(
                matches!(patch.unwrap(), Snapshot::Commit { patch: Some(patch), .. }
                if patch.contains("+base") && !patch.contains("+working"))
            );
        }
        let changes = actor_review(
            &handle,
            &handle.info.workspace,
            Action::Status {
                offset: 0,
                include_patch: true,
            },
        )
        .await;
        if disabled.is_some() {
            assert!(changes.is_err());
        } else {
            assert!(
                matches!(changes.unwrap(), Snapshot::Status { entries, patch: Some(patch), .. }
                if entries[0].path == "tracked.txt" && patch.contains("+working"))
            );
        }
        handle
            .command(session::Command::Close)
            .await
            .unwrap();
    }
}

fn index_action(staged: bool, receipt: String) -> Action {
    if staged {
        Action::Stage {
            path: "tracked.txt".into(),
            receipt,
        }
    } else {
        Action::Unstage {
            path: "tracked.txt".into(),
            receipt,
        }
    }
}

#[tokio::test]
async fn index_actor_checks_scope_disabled_capabilities_and_readonly_before_writing() {
    use orbit_protocol::Capability;
    for (disabled, readonly, message) in [
        (Some(Capability::Files), false, "文件功能已禁用"),
        (Some(Capability::Git), false, "Git 功能已禁用"),
        (Some(Capability::Write), false, "写入功能已禁用"),
        (None, true, "只读会话"),
    ] {
        let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
        let root = &fixture
            .service
            .options
            .initial_directory;
        initialize_git(root);
        let before = fs::read(root.join(".git/index")).unwrap();
        let mut setup =
            crate::workspace::setup::prepare_workspace(&fixture.service.options, root, None)
                .await
                .unwrap();
        setup.resources.policy.disabled = disabled.into_iter().collect();
        setup.resources.policy.read_only = readonly;
        let handle = Handle::spawn(setup, None)
            .await
            .unwrap();
        let workspace = handle.info.workspace.clone();
        for staged in [true, false] {
            let error = actor_review(
                &handle,
                "/old-workspace",
                index_action(staged, "0".repeat(64)),
            )
            .await
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("工作区已切换")
            );
            let error = actor_review(&handle, &workspace, index_action(staged, "0".repeat(64)))
                .await
                .unwrap_err();
            assert!(error.to_string().contains(message), "{error:#}");
            assert_eq!(fs::read(root.join(".git/index")).unwrap(), before);
            assert_eq!(fs::read(root.join("tracked.txt")).unwrap(), b"working\n");
            assert!(
                handle
                    .store
                    .workspace_changes(&handle.info.id)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        handle
            .command(session::Command::Close)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn index_actor_refuses_updates_while_an_agent_is_running() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let fixture = Fixture::new(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
    let root = &fixture
        .service
        .options
        .initial_directory;
    initialize_git(root);
    let before = fs::read(root.join(".git/index")).unwrap();
    let session = fixture.handle.info.id.clone();
    fixture
        .request(Request::Start {
            session: session.clone(),
            request_id: "index-busy".into(),
            prompt: "wait for cancellation".into(),
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
    for staged in [true, false] {
        let error = fixture
            .request(Request::Review {
                session: session.clone(),
                expected_workspace: fixture.handle.info.workspace.clone(),
                action: index_action(staged, "0".repeat(64)),
            })
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("请先结束正在运行的任务")
        );
        assert_eq!(fs::read(root.join(".git/index")).unwrap(), before);
    }
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
            .workspace_changes(&session)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn successful_index_transactions_do_not_create_agent_runs_or_file_undo_receipts() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let root = &fixture
        .service
        .options
        .initial_directory;
    initialize_git(root);
    let session = fixture.handle.info.id.clone();
    let workspace = fixture.handle.info.workspace.clone();
    let persisted_before = fixture
        .handle
        .store
        .list()
        .await
        .unwrap()
        .len();
    for (kind, staged, expected) in [
        (Kind::Unstaged, true, b"working\n".as_slice()),
        (Kind::Staged, false, b"base\n".as_slice()),
    ] {
        let Response::Review(Snapshot::Diff {
            index_receipt: Some(receipt),
            ..
        }) = fixture
            .request(Request::Review {
                session: session.clone(),
                expected_workspace: workspace.clone(),
                action: Action::Diff {
                    kind,
                    path: "tracked.txt".into(),
                },
            })
            .await
            .unwrap()
        else {
            panic!("index receipt expected")
        };
        assert!(matches!(
            fixture
                .request(Request::Review {
                    session: session.clone(),
                    expected_workspace: workspace.clone(),
                    action: index_action(staged, receipt)
                })
                .await
                .unwrap(),
            Response::Review(Snapshot::IndexUpdated { .. })
        ));
        assert_eq!(git(root, &["show", ":tracked.txt"]), expected);
        assert_eq!(fs::read(root.join("tracked.txt")).unwrap(), b"working\n");
        assert!(
            fixture
                .handle
                .store
                .workspace_changes(&session)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fixture
                .handle
                .store
                .list()
                .await
                .unwrap()
                .len(),
            persisted_before
        );
        assert!(!fixture.page(0).await.busy);
    }
}

#[tokio::test]
async fn desktop_review_and_undo_respect_disabled_capabilities() {
    let fixture = Fixture::new("http://127.0.0.1:1/v1").await;
    let mut setup = crate::workspace::setup::prepare_workspace(
        &fixture.service.options,
        &fixture
            .service
            .options
            .initial_directory,
        None,
    )
    .await
    .unwrap();
    setup.resources.policy.disabled = vec![orbit_protocol::Capability::Git];
    assert!(
        crate::workspace::review::execute(
            &setup,
            Action::List {
                kind: Kind::Unstaged,
                offset: 0
            }
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("Git 功能已禁用")
    );
    setup.resources.policy.disabled = vec![orbit_protocol::Capability::Files];
    assert!(
        crate::workspace::review::execute(
            &setup,
            Action::List {
                kind: Kind::Checkpoint,
                offset: 0
            }
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("文件功能已禁用")
    );
    setup.resources.policy.disabled = vec![orbit_protocol::Capability::Write];
    let handle = Handle::spawn(setup, None)
        .await
        .unwrap();
    assert!(
        handle
            .command(session::Command::Undo { path: None })
            .await
            .unwrap_err()
            .to_string()
            .contains("文件写入已禁用")
    );
}

#[tokio::test]
async fn checkpoint_review_waits_for_approval_and_refuses_stale_receipts_and_disk_changes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let fixture = Fixture::new(&format!("http://{}/v1", listener.local_addr().unwrap())).await;
    let inside = fixture
        .service
        .options
        .initial_directory
        .join("inside.txt");
    fs::write(&inside, "original").unwrap();
    let model = tokio::spawn(super::permissions::write_model(
        listener,
        fixture.external.join("outside.txt"),
    ));
    fixture.mode(Mode::Yolo).await;
    let id = fixture.handle.info.id.clone();
    fixture
        .request(Request::Start {
            session: id.clone(),
            request_id: "review".into(),
            prompt: "write the files".into(),
            images: vec![],
            compact: false,
            model: None,
        })
        .await
        .unwrap();
    fixture
        .wait(|page| !page.busy && page.next > 0)
        .await;
    model.await.unwrap();
    assert_eq!(fs::read_to_string(&inside).unwrap(), "inside");
    let workspace = fixture.handle.info.workspace.clone();
    let review = |action| Request::Review {
        session: id.clone(),
        expected_workspace: workspace.clone(),
        action,
    };
    assert!(
        fixture
            .request(Request::Review {
                session: id.clone(),
                expected_workspace: "old-workspace".into(),
                action: Action::List {
                    kind: Kind::Checkpoint,
                    offset: 0
                }
            })
            .await
            .unwrap_err()
            .to_string()
            .contains("工作区已切换")
    );
    let Response::Review(Snapshot::Diff {
        patch,
        checkpoint: Some(receipt),
        ..
    }) = fixture
        .request(review(Action::Diff {
            kind: Kind::Checkpoint,
            path: "inside.txt".into(),
        }))
        .await
        .unwrap()
    else {
        panic!("checkpoint diff missing")
    };
    assert!(patch.contains("-original"));
    assert!(patch.contains("+inside"));
    let undo = |run_id, call_id| {
        review(Action::Undo {
            path: "inside.txt".into(),
            run_id,
            call_id,
        })
    };
    assert!(
        fixture
            .request(undo(receipt.run_id + 1, receipt.call_id.clone()))
            .await
            .unwrap_err()
            .to_string()
            .contains("检查点已变更")
    );
    fs::write(&inside, "manual edit").unwrap();
    assert!(
        fixture
            .request(undo(receipt.run_id, receipt.call_id.clone()))
            .await
            .unwrap_err()
            .to_string()
            .contains("current file differs")
    );
    assert_eq!(fs::read_to_string(&inside).unwrap(), "manual edit");
    fs::write(&inside, "inside").unwrap();
    fixture.mode(Mode::Ask).await;
    let request = fixture.request(undo(receipt.run_id, receipt.call_id.clone()));
    tokio::pin!(request);
    let pending = tokio::select! {
        result = &mut request => panic!("undo returned before approval: {result:?}"),
        page = fixture.wait(|page| !page.approvals.is_empty()) => page,
    };
    assert_eq!(fs::read_to_string(&inside).unwrap(), "inside");
    fixture
        .request(Request::Approve {
            session: id.clone(),
            approval: pending.approvals[0].id,
            allow: true,
            remember: false,
        })
        .await
        .unwrap();
    assert!(matches!(
        request.await.unwrap(),
        Response::Review(Snapshot::Undone { .. })
    ));
    assert_eq!(fs::read_to_string(&inside).unwrap(), "original");
    assert!(!fixture.page(0).await.busy);
    assert!(
        fixture
            .request(undo(receipt.run_id, receipt.call_id))
            .await
            .unwrap_err()
            .to_string()
            .contains("检查点已变更")
    );
}
