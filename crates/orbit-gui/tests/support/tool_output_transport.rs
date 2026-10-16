use super::*;
use clap::Parser;
use orbit_client::protocol::events::AgentEvent;

#[test]
fn command_stdout_snapshots_and_nonzero_exit_survive_native_transport() {
    let directory = tempfile::tempdir().unwrap();
    let mut options = Options::parse_from(["orbit-gui"]);
    options.config = directory.path().join("unused.toml");
    options.workspace = Some(directory.path().into());
    options.server = Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into());
    options.database = Some(directory.path().join("unused.db"));
    options.auth_dir = Some(directory.path().join("auth"));
    options.model = Some("fixture::local".into());
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
        backend
            .send(Command::Send {
                session: info.id,
                id: "command-output-fixture".into(),
                prompt: "command-output-fast".into(),
                paths: vec![],
                steer: false,
            })
            .unwrap();
        let Event::Response(Response::Page(first)) = until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page)) if !page.tool_progress.is_empty()
            )
        })
        .await
        else {
            unreachable!()
        };
        assert_eq!(first.tool_progress[0].call_id, "output-command");
        let initial = first.tool_progress[0]
            .stdout
            .as_deref()
            .unwrap();
        assert!(initial.contains("build output line 0"));
        let Event::Response(Response::Page(second)) = until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page))
                    if page
                        .tool_progress
                        .first()
                        .is_some_and(|progress| {
                            progress.elapsed_ms > first.tool_progress[0].elapsed_ms
                        })
            )
        })
        .await
        else {
            unreachable!()
        };
        let next = second.tool_progress[0]
            .stdout
            .as_deref()
            .unwrap();
        assert!(
            next.starts_with(initial),
            "the peer sends whole replacement snapshots"
        );
        let Event::Response(Response::Page(final_page)) = until(&mut events, |event| {
            matches!(
                event,
                Event::Response(Response::Page(page))
                    if page.updates.iter().any(|update| {
                        matches!(
                            update,
                            orbit_protocol::Update::Agent(event)
                                if matches!(event.as_ref(), AgentEvent::ToolFinished { .. })
                        )
                    })
            )
        })
        .await
        else {
            unreachable!()
        };
        let result = final_page
            .updates
            .into_iter()
            .find_map(|update| match update {
                orbit_protocol::Update::Agent(event) => match *event {
                    AgentEvent::ToolFinished { call, content, .. }
                        if call.id == "output-command" =>
                    {
                        Some(serde_json::from_str::<serde_json::Value>(&content).unwrap())
                    }
                    _ => None,
                },
                _ => None,
            })
            .unwrap();
        assert_eq!(result["exit_code"], 2);
        assert_eq!(result["elapsed"]["nanos"], 125_000_000);
        assert!(
            result["stderr"]
                .as_str()
                .unwrap()
                .contains("compilation failed")
        );
        assert_eq!(result["output_truncated"], true);
        assert!(
            final_page.tool_progress.is_empty(),
            "completed calls stop streaming"
        );
    });
}

#[test]
fn complete_output_uses_read_only_paging_without_resubmitting_tools() {
    use orbit_client::execution::{ToolRun, ToolStatus, details::Stream};
    use orbit_client::protocol::model::ToolCall;
    use serde_json::json;
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
    options.workspace = Some(directory.path().into());
    options.server = Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into());
    options.database = Some(directory.path().join("unused.db"));
    options.auth_dir = Some(directory.path().join("auth"));
    options.model = Some("fixture::local".into());
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
        for (name, receipt, expected) in [
            (
                "WebFetch",
                json!({
                    "content": "preview webpage", "snapshot_id": "web-inspector", "has_more": true
                }),
                vec![(Stream::Result, "DOCUMENT-001", "DOCUMENT-080")],
            ),
            (
                "Exec",
                json!({
                    "exit_code": 0, "elapsed": {"secs": 0, "nanos": 0},
                    "stdout": "preview", "stderr": "preview", "output_id": "cmd-inspector",
                    "timed_out": false, "cancelled": false, "output_truncated": true,
                    "stdout_total_bytes": 10_000, "stderr_total_bytes": 4_000
                }),
                vec![
                    (Stream::Stdout, "STDOUT-001", "STDOUT-100"),
                    (Stream::Stderr, "STDERR-001", "STDERR-040"),
                ],
            ),
        ] {
            let mut tool = ToolRun::new(&ToolCall::function("generated", name, json!({})));
            tool.set_output(ToolStatus::Complete, receipt.to_string());
            let (result, response) = tokio::sync::oneshot::channel();
            backend
                .send(Command::ToolOutput {
                    session: info.id.clone(),
                    tool: Box::new(tool),
                    result,
                })
                .unwrap();
            let contents = tokio::time::timeout(Duration::from_secs(10), response)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            for (stream, first, last) in expected {
                let content = &contents
                    .iter()
                    .find(|(page, _)| *page == stream)
                    .unwrap()
                    .1;
                assert!(content.contains(first) && content.contains(last));
                assert!(!content.contains("preview"));
            }
        }
    });
    let requests = std::fs::read_to_string(
        directory
            .path()
            .join(".fixture-requests"),
    )
    .unwrap();
    let methods = requests
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["method"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert!(
        methods
            .iter()
            .filter(|method| *method == "read_tool_output")
            .count()
            > 4
    );
    assert!(
        !methods
            .iter()
            .any(|method| matches!(method.as_str(), "submit" | "start"))
    );
}
