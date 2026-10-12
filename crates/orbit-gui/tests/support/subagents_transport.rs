use super::*;
use clap::Parser;
use orbit_protocol::subagents::{Action, Snapshot};

async fn request(backend: &Backend, request: Request) -> Result<Response, String> {
    let (result, receiver) = tokio::sync::oneshot::channel();
    backend.send(Command::Settings { request, result }).unwrap();
    tokio::time::timeout(Duration::from_secs(5), receiver)
        .await
        .unwrap()
        .unwrap()
}

#[test]
fn native_child_discovery_owns_each_row_and_waits_for_cancelled_storage_state() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join(".fixture-subagents"), "").unwrap();
    std::fs::write(
        directory.path().join(".fixture-subagents-send-fail-once"),
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
        let Event::Response(Response::Opened(info)) = until(&mut events, |event| matches!(event, Event::Response(Response::Opened(_)))).await else { unreachable!() };
        let command = |action| Request::Subagents { session: info.id.clone(), expected_workspace: info.workspace.clone(), action };
        let Response::Subagents(Snapshot::List { agents, next }) = request(&backend, command(Action::List { offset: 0 })).await.unwrap() else { panic!("list expected") };
        assert_eq!(agents.len(), 32);
        assert_eq!(next, Some(32));
        let child = agents.iter().find(|agent| agent.id == "child-running").unwrap().clone();
        assert_eq!(child.status, "running");
        let develop = agents.iter().find(|agent| agent.id == "child-develop").unwrap();
        assert_ne!(develop.workspace, info.workspace);
        let Response::Subagents(Snapshot::List { agents: older, next }) = request(&backend, command(Action::List { offset: 32 })).await.unwrap() else { panic!("older rows expected") };
        assert_eq!(older.len(), 3);
        assert!(next.is_none());
        assert!(request(&backend, Request::Subagents { session: info.id.clone(), expected_workspace: "stale-workspace".into(), action: Action::List { offset: 0 } }).await.unwrap_err().contains("工作区已切换"));
        assert!(request(&backend, Request::Subagents { session: "navigation".into(), expected_workspace: info.workspace.clone(), action: Action::Cancel { id: child.id.clone(), run: child.run } }).await.unwrap_err().contains("不属于"));
        assert!(request(&backend, command(Action::Cancel { id: child.id.clone(), run: child.run + 1 })).await.unwrap_err().contains("另一轮任务"));
        let send = || command(Action::Send { id: "child".into(), message: "仅向第一行跟进".into() });
        assert!(request(&backend, send()).await.unwrap_err().contains("草稿应保留"));
        assert!(matches!(request(&backend, send()).await.unwrap(), Response::Subagents(Snapshot::Sent { id, status, .. }) if id == "child" && status == "running"));
        let Response::Subagent(page) = request(&backend, Request::ReadSubagent { session: info.id.clone(), agent: "child".into(), after: 0 }).await.unwrap() else { panic!("child stream expected") };
        assert!(page.busy);
        assert!(page.updates.iter().any(|update| format!("{update:?}").contains("仅向第一行跟进")));
        assert!(matches!(request(&backend, command(Action::Cancel { id: child.id.clone(), run: child.run })).await.unwrap(), Response::Subagents(Snapshot::Stopping { id, run }) if id == child.id && run == child.run));
        let Response::Subagents(Snapshot::List { agents: pending, .. }) = request(&backend, command(Action::List { offset: 0 })).await.unwrap() else { panic!("pending list expected") };
        assert_eq!(pending.iter().find(|agent| agent.id == child.id).unwrap().status, "running", "cancel acknowledgement is not the terminal result");
        let Response::Subagents(Snapshot::List { agents: final_rows, .. }) = request(&backend, command(Action::List { offset: 0 })).await.unwrap() else { panic!("final list expected") };
        assert_eq!(final_rows.iter().find(|agent| agent.id == child.id).unwrap().status, "cancelled");
        assert_eq!(final_rows.iter().find(|agent| agent.id == "child").unwrap().status, "running", "cancelling one row leaves its sibling running");
        assert!(request(&backend, Request::ReadSubagent { session: "navigation".into(), agent: child.id, after: 0 }).await.unwrap_err().contains("不是当前会话"));
    });
}
