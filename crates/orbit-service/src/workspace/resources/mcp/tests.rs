use super::*;
use crate::workspace::session::{Command, Handle};
use orbit_config::test_support::{TestConfig, TestProvider};
use serde_json::json;
use std::{sync::Arc, time::Duration};

async fn fixture(root: &Path) -> PreparedWorkspace {
    std::fs::create_dir(root.join(".git")).unwrap();
    TestConfig::new()
        .provider(TestProvider::new("test").key("test-only").models(["mock"]))
        .write(root.join("config.toml"))
        .unwrap();
    let options = crate::WorkspaceOptions {
        model: Some("test::mock".into()),
        initial_directory: root.into(),
        config: root.join("config.toml"),
        auth_directory: root.join("auth"),
        database: root.join("sessions.db"),
        allow_mcp_start: false,
        home: Some(root.into()),
        config_home: None,
        instruction_dir: None,
        editor_command: "vi".into(),
    };
    crate::workspace::setup::prepare_service(&options, None)
        .await
        .unwrap()
}

fn config(script: &str, args: Vec<String>) -> serde_json::Value {
    let mut arguments = vec!["-c".to_owned(), script.to_owned(), "fixture".to_owned()];
    arguments.extend(args);
    json!({"servers":{"echo":{"command":"/bin/sh","args":arguments,"enabled":true,"timeout_seconds":4}}})
}

const SERVER: &str = r#"while IFS= read -r line; do
case "$line" in
*'"method":"initialize"'*) printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}' ;;
*'"method":"tools/list"'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"fixture","inputSchema":{"type":"object"}}]}}' ;;
esac
done"#;

async fn reload(
    setup: &mut PreparedWorkspace,
    config: serde_json::Value,
    allowed: bool,
) -> Snapshot {
    let (tx, rx) = oneshot::channel();
    let mut job = Reload::start(setup, config, allowed, tx).unwrap();
    let output = (&mut job.task).await.unwrap();
    job.finish(setup, output);
    rx.await.unwrap().unwrap()
}

#[tokio::test]
async fn reconnects_existing_session_preserves_runs_and_removes_stale_tools() {
    let root = tempfile::tempdir().unwrap();
    let mut setup = fixture(root.path()).await;
    setup.start_session().await.unwrap();
    let id = setup.session.id().to_owned();
    let old = setup.resources.tools.clone();
    let Snapshot::Mcp(servers) = reload(&mut setup, config(SERVER, vec![]), true).await else {
        panic!()
    };
    assert!(servers[0].connected, "{}", servers[0].detail);
    assert_eq!(servers[0].tools[0].name, "mcp__echo__echo");
    assert!(!old.contains("mcp__echo__echo"));
    assert!(setup.resources.tools.contains("read_file"));
    assert!(setup.resources.tools.contains("mcp__echo__echo"));
    assert_eq!(setup.session.id(), id);
    let running = setup.resources.tools.clone();
    reload(&mut setup, json!({"servers":{}}), true).await;
    assert!(!setup.resources.tools.contains("mcp__echo__echo"));
    assert!(running.contains("mcp__echo__echo"));
    assert!(setup.resources.tools.contains("read_file"));
    setup
        .resources
        .disabled_tools
        .push("mcp__echo__echo".into());
    let Snapshot::Mcp(servers) = reload(&mut setup, config(SERVER, vec![]), true).await else {
        panic!()
    };
    assert!(servers[0].tools.is_empty());
    assert!(!setup.resources.tools.contains("mcp__echo__echo"));
}

#[tokio::test]
async fn reconnect_respects_start_permission_validation_and_session_policy() {
    let root = tempfile::tempdir().unwrap();
    let mut setup = fixture(root.path()).await;
    let Snapshot::Mcp(servers) = reload(&mut setup, config(SERVER, vec![]), false).await else {
        panic!()
    };
    assert!(!servers[0].connected);
    assert!(!setup.resources.tools.contains("mcp__echo__echo"));
    let mut invalid = config(SERVER, vec![]);
    invalid["servers"]["echo"]["env"] = json!({"LD_PRELOAD":"forbidden"});
    let (tx, _) = oneshot::channel();
    assert!(Reload::start(&setup, invalid, true, tx).is_err());
    setup.resources.policy.read_only = true;
    let (tx, _) = oneshot::channel();
    assert!(Reload::start(&setup, config(SERVER, vec![]), true, tx).is_err());
    setup.resources.policy.read_only = false;
    setup.resources.policy.disabled.push(Capability::Mcp);
    let (tx, _) = oneshot::channel();
    assert!(Reload::start(&setup, config(SERVER, vec![]), true, tx).is_err());
}

#[tokio::test]
async fn slow_initialize_does_not_block_existing_session_commands() {
    let root = tempfile::tempdir().unwrap();
    let mut setup = fixture(root.path()).await;
    setup.start_session().await.unwrap();
    let handle = Arc::new(Handle::spawn(setup, None).await.unwrap());
    let started = root.path().join("started");
    let release = root.path().join("release");
    let script = format!("touch \"$1\"; while [ ! -f \"$2\" ]; do sleep 0.02; done;\n{SERVER}");
    let reload = {
        let handle = handle.clone();
        let config = config(
            &script,
            vec![started.display().to_string(), release.display().to_string()],
        );
        tokio::spawn(async move {
            handle
                .resources(
                    Action::ReloadMcp {
                        config,
                        allow_start: true,
                    },
                    None,
                )
                .await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while !started.exists() {
            assert!(!reload.is_finished());
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        handle.command(Command::SetMode {
            mode: orbit_protocol::Mode::Yolo,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(!reload.is_finished());
    std::fs::write(release, "ready").unwrap();
    let Snapshot::Mcp(servers) = reload.await.unwrap().unwrap() else {
        panic!()
    };
    assert!(servers[0].connected, "{}", servers[0].detail);
    handle.command(Command::Close).await.unwrap();
}
