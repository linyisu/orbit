use super::*;
use clap::Parser;
use orbit_protocol::input_queue::{Action, Snapshot, State};

async fn request(backend: &Backend, request: Request) -> Result<Response, String> {
    let (result, receiver) = tokio::sync::oneshot::channel();
    backend.send(Command::Settings { request, result }).unwrap();
    tokio::time::timeout(Duration::from_secs(5), receiver)
        .await
        .unwrap()
        .unwrap()
}

#[test]
fn queue_transport_uses_row_cursors_exact_content_and_run_receipts() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join(".fixture-input-queue"), "").unwrap();
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
        let Event::Response(Response::Opened(info)) = until(&mut events, |event| matches!(event, Event::Response(Response::Opened(_)))).await else { unreachable!() };
        let command = |action| Request::InputQueue { session: info.id.clone(), expected_workspace: info.workspace.clone(), action };
        let Response::InputQueue(Snapshot::List { entries, next, run }) = request(&backend, command(Action::List { after: None })).await.unwrap() else { panic!("queue expected") };
        assert_eq!(entries.len(), 32);
        assert_eq!(next, Some(224));
        assert_eq!(run, Some(71));
        assert_eq!(entries[1].state, State::Claimed);
        let first = &entries[0];
        let edit = |expected: String, run| command(Action::Edit { id: first.id, expected, content: "updated queue draft".into(), run });
        assert!(request(&backend, edit(first.content.clone(), Some(72))).await.unwrap_err().contains("运行已变化"));
        assert!(request(&backend, edit("stale content".into(), run)).await.unwrap_err().contains("内容已变化"));
        std::fs::write(directory.path().join(".fixture-input-queue-fail-once"), "").unwrap();
        assert!(request(&backend, edit(first.content.clone(), run)).await.unwrap_err().contains("草稿应保留"));
        assert!(matches!(request(&backend, edit(first.content.clone(), run)).await.unwrap(), Response::InputQueue(Snapshot::Edited { entry }) if entry.content == "updated queue draft"));
        assert!(request(&backend, command(Action::Remove { id: first.id, expected: first.content.clone(), run })).await.unwrap_err().contains("内容已变化"));
        assert!(matches!(request(&backend, command(Action::Remove { id: first.id, expected: "updated queue draft".into(), run })).await.unwrap(), Response::InputQueue(Snapshot::Removed { id }) if id == first.id));
        let Response::InputQueue(Snapshot::List { entries: older, next, .. }) = request(&backend, command(Action::List { after: next })).await.unwrap() else { panic!("older queue expected") };
        assert_eq!(older.len(), 3);
        assert_eq!(older[0].id, 231);
        assert!(next.is_none());
        std::fs::write(directory.path().join(".fixture-input-queue-unknown-once"), "").unwrap();
        assert!(request(&backend, command(Action::Edit { id: older[0].id, expected: older[0].content.clone(), content: "saved despite lost receipt".into(), run })).await.unwrap_err().contains("回执已丢失"));
        let Response::InputQueue(Snapshot::List { entries: reconciled, .. }) = request(&backend, command(Action::List { after: Some(224) })).await.unwrap() else { panic!("reconciliation expected") };
        assert_eq!(reconciled[0].content, "saved despite lost receipt");
        let claimed = &entries[1];
        assert!(request(&backend, command(Action::Remove { id: claimed.id, expected: claimed.content.clone(), run })).await.unwrap_err().contains("已被取走"));
        assert!(request(&backend, Request::InputQueue { session: info.id.clone(), expected_workspace: "previous-workspace".into(), action: Action::List { after: None } }).await.unwrap_err().contains("工作区已切换"));
        assert!(request(&backend, Request::InputQueue { session: "other-session".into(), expected_workspace: info.workspace.clone(), action: Action::List { after: None } }).await.unwrap_err().contains("不属于当前会话"));
    });
}
