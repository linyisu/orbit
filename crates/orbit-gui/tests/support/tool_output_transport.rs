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
