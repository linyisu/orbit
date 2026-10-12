use super::*;
use clap::Parser;
use orbit_protocol::review::{Action, Kind, Snapshot};

fn options(directory: &std::path::Path) -> Options {
    let mut options = Options::parse_from(["orbit-gui"]);
    options.config = directory.join("unused.toml");
    options.workspace = Some(directory.into());
    options.server = Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into());
    options.database = Some(directory.join("unused.db"));
    options.auth_dir = Some(directory.join("auth"));
    options.model = Some("fixture::local".into());
    options.resume = Some("demo".into());
    options
}

fn review(session: &str, workspace: &str, action: Action) -> Request {
    Request::Review {
        session: session.into(),
        expected_workspace: workspace.into(),
        action,
    }
}

fn undo(run_id: i64, call_id: &str) -> Action {
    Action::Undo {
        path: "src/main.rs".into(),
        run_id,
        call_id: call_id.into(),
    }
}

async fn request(backend: &Backend, request: Request) -> Result<Response, String> {
    let (result, receiver) = tokio::sync::oneshot::channel();
    backend.send(Command::Settings { request, result }).unwrap();
    tokio::time::timeout(Duration::from_secs(10), receiver)
        .await
        .expect("review request timed out")
        .expect("review request was abandoned")
}

#[test]
fn review_transport_validates_scope_and_receipt_before_completing_undo() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().to_string_lossy().into_owned();
    let marker = directory.path().join(".fixture-review-undone");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (backend, mut events) = Backend::start(options(directory.path()), &runtime);
    runtime.block_on(async {
        let Event::Response(Response::Opened(info)) = until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        })
        .await
        else {
            unreachable!()
        };
        assert_eq!(info.workspace, workspace);
        let session = info.id;
        let list = || Action::List {
            kind: Kind::Checkpoint,
            offset: 0,
        };
        let diff = || Action::Diff {
            kind: Kind::Checkpoint,
            path: "src/main.rs".into(),
        };

        let Response::Review(Snapshot::List { entries, next, .. }) =
            request(&backend, review(&session, &workspace, list()))
                .await
                .unwrap()
        else {
            panic!("checkpoint listing expected")
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "src/main.rs");
        assert_eq!(entries[0].kind, Kind::Checkpoint);
        assert!(!entries[0].undone);
        assert_eq!(next, None);

        let Response::Review(Snapshot::Diff {
            path,
            patch,
            checkpoint,
            ..
        }) = request(&backend, review(&session, &workspace, diff()))
            .await
            .unwrap()
        else {
            panic!("checkpoint diff expected")
        };
        assert_eq!(path, "src/main.rs");
        assert!(patch.contains("--- a/src/main.rs\n+++ b/src/main.rs\n"));
        assert!(patch.contains("+fn main() { println!(\"Hello Orbit\"); }"));
        let checkpoint = checkpoint.expect("reviewed checkpoint receipt");
        assert_eq!(checkpoint.run_id, 7);
        assert_eq!(checkpoint.call_id, "fixture-write");
        assert!(!checkpoint.undone);

        let stale_scope = request(
            &backend,
            review(&session, "previous-workspace", undo(7, "fixture-write")),
        )
        .await
        .unwrap_err();
        assert!(stale_scope.contains("工作区已切换"));
        assert!(!marker.exists());
        for (run_id, call_id) in [(6, "fixture-write"), (7, "previous-write")] {
            let stale_receipt = request(
                &backend,
                review(&session, &workspace, undo(run_id, call_id)),
            )
            .await
            .unwrap_err();
            assert!(stale_receipt.contains("检查点已变更"));
            assert!(!marker.exists());
        }

        let Response::Review(Snapshot::Undone { path, detail }) = request(
            &backend,
            review(
                &session,
                &workspace,
                undo(checkpoint.run_id, &checkpoint.call_id),
            ),
        )
        .await
        .unwrap() else {
            panic!("undo completion expected")
        };
        assert_eq!(path, "src/main.rs");
        assert!(detail.contains("已撤销"));
        assert_eq!(std::fs::read(&marker).unwrap(), b"undone");

        let Response::Review(Snapshot::List { entries, .. }) =
            request(&backend, review(&session, &workspace, list()))
                .await
                .unwrap()
        else {
            panic!("updated checkpoint listing expected")
        };
        assert_eq!(entries.len(), 1);
        assert!(entries[0].undone);
        let Response::Review(Snapshot::Diff {
            checkpoint: Some(checkpoint),
            ..
        }) = request(&backend, review(&session, &workspace, diff()))
            .await
            .unwrap()
        else {
            panic!("updated checkpoint receipt expected")
        };
        assert!(checkpoint.undone);
        assert!(
            request(
                &backend,
                review(&session, &workspace, undo(7, "fixture-write"))
            )
            .await
            .unwrap_err()
            .contains("检查点已变更")
        );
        assert_eq!(std::fs::read(&marker).unwrap(), b"undone");
    });
}

#[test]
fn review_undo_uses_an_independent_handshake_and_keeps_the_main_connection_available() {
    let directory = tempfile::tempdir().unwrap();
    // Enable the fixture's append-only request log without changing its protocol behavior.
    std::fs::write(directory.path().join(".fixture-recovery"), "").unwrap();
    let workspace = directory.path().to_string_lossy().into_owned();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (backend, mut events) = Backend::start(options(directory.path()), &runtime);
    runtime.block_on(async {
        let Event::Response(Response::Opened(info)) = until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        }).await else { unreachable!() };
        assert_eq!(info.id, "demo");
        until(&mut events, |event| matches!(event, Event::Response(Response::Page(page)) if !page.more)).await;

        let (result, receiver) = tokio::sync::oneshot::channel();
        backend.send(Command::Settings {
            request: review(&info.id, &workspace, undo(7, "fixture-write")),
            result,
        }).unwrap();
        backend.send(Command::Request(Request::Poll { session: info.id.clone(), after: 0 })).unwrap();
        let (completion, page) = tokio::join!(
            tokio::time::timeout(Duration::from_secs(10), receiver),
            until(&mut events, |event| matches!(event, Event::Response(Response::Page(page)) if !page.updates.is_empty())),
        );
        assert!(matches!(completion.unwrap().unwrap().unwrap(), Response::Review(Snapshot::Undone { path, .. }) if path == "src/main.rs"));
        let Event::Response(Response::Page(page)) = page else { unreachable!() };
        assert_eq!(page.model, "fixture::local");
        assert_eq!(page.next, 3);
        assert_eq!(page.updates.len(), 3);
        assert!(!page.busy);

        // This explicit poll occurs after the independent request has completed.
        backend.send(Command::Request(Request::Poll { session: info.id.clone(), after: 0 })).unwrap();
        until(&mut events, |event| matches!(event, Event::Response(Response::Page(page)) if page.updates.len() == 3)).await;
    });
    drop(backend);
    let requests: Vec<serde_json::Value> =
        std::fs::read_to_string(directory.path().join(".fixture-requests"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let hellos: Vec<_> = requests
        .iter()
        .enumerate()
        .filter_map(|(index, request)| (request["method"] == "hello").then_some(index))
        .collect();
    assert_eq!(hellos.len(), 2, "undo must create one additional peer");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "open")
            .count(),
        1,
        "the independent peer must address the existing session without opening another one"
    );
    let undo = requests
        .iter()
        .position(|request| request["method"] == "review" && request["action"]["action"] == "undo")
        .unwrap();
    assert!(hellos[1] < undo);
    assert_eq!(requests[undo]["session"], "demo");
    assert_eq!(requests[undo]["expected_workspace"], workspace);
    assert!(
        requests[undo + 1..]
            .iter()
            .any(|request| request["method"] == "poll"
                && request["session"] == "demo"
                && request["after"] == 0)
    );
}

#[test]
fn review_stage_and_unstage_transport_preserve_files_and_reject_stale_receipts() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.name", "Orbit"],
        vec!["config", "user.email", "test@example.invalid"],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(root.join("tracked.txt"), "base\n").unwrap();
    for args in [vec!["add", "tracked.txt"], vec!["commit", "-qm", "base"]] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(root.join("tracked.txt"), "reviewed\n").unwrap();
    let workspace = root.to_string_lossy().into_owned();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (backend, mut events) = Backend::start(options(root), &runtime);
    runtime.block_on(async {
        let Event::Response(Response::Opened(info)) = until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        })
        .await
        else {
            unreachable!()
        };
        let session = info.id;
        let diff = |kind| Action::Diff {
            kind,
            path: "tracked.txt".into(),
        };
        let Response::Review(Snapshot::Diff {
            index_receipt: Some(receipt),
            patch,
            ..
        }) = request(&backend, review(&session, &workspace, diff(Kind::Unstaged)))
            .await
            .unwrap()
        else {
            panic!("review receipt expected")
        };
        assert!(patch.contains("+reviewed"));
        std::fs::write(root.join("tracked.txt"), "later\n").unwrap();
        let index = std::fs::read(root.join(".git/index")).unwrap();
        let error = request(
            &backend,
            review(
                &session,
                &workspace,
                Action::Stage {
                    path: "tracked.txt".into(),
                    receipt,
                },
            ),
        )
        .await
        .unwrap_err();
        assert!(error.contains("已变更"));
        assert_eq!(std::fs::read(root.join(".git/index")).unwrap(), index);
        let Response::Review(Snapshot::Diff {
            index_receipt: Some(receipt),
            ..
        }) = request(&backend, review(&session, &workspace, diff(Kind::Unstaged)))
            .await
            .unwrap()
        else {
            panic!("fresh receipt expected")
        };
        let error = request(
            &backend,
            review(
                &session,
                "/old-workspace",
                Action::Stage {
                    path: "tracked.txt".into(),
                    receipt: receipt.clone(),
                },
            ),
        )
        .await
        .unwrap_err();
        assert!(error.contains("工作区已切换"));
        assert!(matches!(
            request(
                &backend,
                review(
                    &session,
                    &workspace,
                    Action::Stage {
                        path: "tracked.txt".into(),
                        receipt
                    }
                )
            )
            .await
            .unwrap(),
            Response::Review(Snapshot::IndexUpdated { staged: true, .. })
        ));
        assert_eq!(std::fs::read(root.join("tracked.txt")).unwrap(), b"later\n");
        let Response::Review(Snapshot::Diff {
            index_receipt: Some(receipt),
            patch,
            ..
        }) = request(&backend, review(&session, &workspace, diff(Kind::Staged)))
            .await
            .unwrap()
        else {
            panic!("staged receipt expected")
        };
        assert!(patch.contains("+later"));
        assert!(matches!(
            request(
                &backend,
                review(
                    &session,
                    &workspace,
                    Action::Unstage {
                        path: "tracked.txt".into(),
                        receipt
                    }
                )
            )
            .await
            .unwrap(),
            Response::Review(Snapshot::IndexUpdated { staged: false, .. })
        ));
        assert_eq!(std::fs::read(root.join("tracked.txt")).unwrap(), b"later\n");
        let Response::Review(Snapshot::List { entries, .. }) = request(
            &backend,
            review(
                &session,
                &workspace,
                Action::List {
                    kind: Kind::Staged,
                    offset: 0,
                },
            ),
        )
        .await
        .unwrap() else {
            panic!("staged list expected")
        };
        assert!(entries.iter().all(|entry| entry.path != "tracked.txt"));
    });
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["show", ":tracked.txt"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"base\n");
    assert!(
        !root.join(".fixture-review-undone").exists(),
        "index updates must not use Orbit checkpoint undo"
    );
}
