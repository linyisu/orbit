use std::{path::Path, process::Stdio, time::Duration};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::{Child, ChildStdin, ChildStdout, Command},
};

struct Client {
    child: Child,
    input: ChildStdin,
    output: ChildStdout,
}

impl Client {
    async fn connect(root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_orbit"))
            .arg("workspace-service")
            .arg("--workspace")
            .arg(root)
            .arg("--config")
            .arg(root.join("config.toml"))
            .arg("--database")
            .arg(root.join("sessions.db"))
            .arg("--auth-dir")
            .arg(root.join("auth"))
            .env("HOME", root)
            .env("XDG_CONFIG_HOME", root.join("config-home"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut client = Self {
            input: child.stdin.take().unwrap(),
            output: child.stdout.take().unwrap(),
            child,
        };
        let mut marker = vec![0; b"\0ORBIT-WORKSPACE/1\n".len()];
        client.output.read_exact(&mut marker).await.unwrap();
        assert_eq!(marker, b"\0ORBIT-WORKSPACE/1\n");
        assert_eq!(
            client.request(json!({"method":"hello","version":3})).await["type"],
            "hello"
        );
        client
    }

    async fn request(&mut self, request: Value) -> Value {
        tokio::time::timeout(Duration::from_secs(20), async {
            let request = serde_json::to_vec(&request).unwrap();
            self.input.write_u32(request.len() as u32).await.unwrap();
            self.input.write_all(&request).await.unwrap();
            self.input.flush().await.unwrap();
            let size = self.output.read_u32().await.unwrap();
            assert!(size < 32 * 1024 * 1024);
            let mut response = vec![0; size as usize];
            self.output.read_exact(&mut response).await.unwrap();
            serde_json::from_slice(&response).unwrap()
        })
        .await
        .expect("workspace request timed out")
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

async fn mock_model() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        for index in 0..2 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut byte = [0; 1];
            while !request.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
            }
            let header = String::from_utf8(request).unwrap();
            assert!(
                header
                    .to_ascii_lowercase()
                    .contains("authorization: bearer test-only")
            );
            let length: usize = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|length| length.trim().parse().unwrap())
                })
                .unwrap();
            let mut body = vec![0; length];
            socket.read_exact(&mut body).await.unwrap();
            let request: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["stream"], true);
            let delta = if index == 0 {
                json!({"tool_calls":[{"index":0,"id":"write","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"result.txt\",\"content\":\"written remotely\"}"}}]})
            } else {
                json!({"content":"任务完成。"})
            };
            let finish = if index == 0 { "tool_calls" } else { "stop" };
            let events = format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                json!({"choices":[{"index":0,"delta":delta}]}),
                json!({"choices":[{"index":0,"delta":{},"finish_reason":finish}]})
            );
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{events}", events.len()).as_bytes()).await.unwrap();
        }
    });
    (format!("http://{address}/v1"), task)
}

#[tokio::test]
async fn disconnect_preserves_approval_and_run_duplicate_submission_does_not_repeat_writes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (url, model) = mock_model().await;
    std::fs::write(
        root.join("config.toml"),
        format!(
            r#"default_provider = "test"
[providers.test]
base_url = "{url}"
api_key = "test-only"
auto_refresh_models = false
models = [{{ id = "mock" }}]
"#
        ),
    )
    .unwrap();
    let mut client = Client::connect(root).await;
    let opened = client.request(json!({"method":"open","resume":null})).await;
    assert_eq!(opened["type"], "opened", "{opened}");
    let id = opened["value"]["id"].as_str().unwrap().to_owned();
    let store = orbit_store::store::Store::open(root.join("sessions.db"))
        .await
        .unwrap();
    let mut child = store
        .create_subagent(
            &id,
            "child-model".into(),
            "explore".into(),
            "child task".into(),
        )
        .await
        .unwrap();
    child
        .begin("child task".into(), "child-model".into())
        .await
        .unwrap();
    child
        .append(
            (0..140)
                .map(|_| {
                    orbit_agent::events::AgentEvent::Model(
                        orbit_model::types::StreamEvent::TextDelta("child answer\n".into()),
                    )
                })
                .collect(),
        )
        .await
        .unwrap();
    let child_page = client
        .request(json!({"method":"read_subagent","session":id,"agent":child.id(),"after":0}))
        .await;
    assert_eq!(child_page["type"], "subagent");
    assert_eq!(child_page["value"]["more"], true);
    let cursor = child_page["value"]["next"].as_u64().unwrap();
    child
        .finish("completed", vec![], Some(vec![]))
        .await
        .unwrap();
    let child_page = client
        .request(json!({"method":"read_subagent","session":id,"agent":child.id(),"after":cursor}))
        .await;
    assert_eq!(child_page["value"]["busy"], false);
    assert_eq!(
        child_page["value"]["updates"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["Finished"]["status"],
        "completed"
    );
    for method in ["read_subagent", "send_subagent"] {
        let response = client
            .request(
                json!({"method":method,"session":id,"agent":id,"after":0,"message":"unrelated"}),
            )
            .await;
        assert_eq!(response["type"], "error");
    }
    let start = json!({"method":"start","session":id,"request_id":"stable-request","prompt":"write a file","mode":"ask","model":null});
    assert_eq!(client.request(start.clone()).await["type"], "ok");
    let approval = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let page = client
                .request(json!({"method":"poll","session":id,"after":0}))
                .await;
            assert_eq!(page["type"], "page", "{page}");
            if let Some(approval) = page["value"]["approvals"].as_array().unwrap().first() {
                break approval["id"].as_u64().unwrap();
            }
            assert_eq!(
                page["value"]["busy"], true,
                "run unexpectedly finished: {page}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(!root.join("result.txt").exists());
    assert_eq!(
        client.request(json!({"method":"shutdown"})).await["type"],
        "error"
    );
    drop(client);
    let mut client = Client::connect(root).await;
    assert_eq!(
        client.request(json!({"method":"open","resume":id})).await["type"],
        "opened"
    );
    assert_eq!(client.request(start.clone()).await["type"], "ok");
    let mut conflicting = start.clone();
    conflicting["prompt"] = "different prompt".into();
    assert_eq!(client.request(conflicting).await["type"], "error");
    assert_eq!(client.request(json!({"method":"approve","session":id,"approval":approval,"allow":true,"remember":false})).await["type"], "ok");
    let page = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let page = client
                .request(json!({"method":"poll","session":id,"after":0}))
                .await;
            if page["value"]["busy"] == false {
                break page;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("result.txt")).unwrap(),
        "written remotely"
    );
    let updates = page["value"]["updates"].as_array().unwrap();
    assert_eq!(
        updates
            .iter()
            .filter(|update| update.get("Started").is_some())
            .count(),
        1
    );
    assert_eq!(
        updates.last().unwrap()["Finished"]["status"],
        "completed",
        "{page}"
    );
    assert_eq!(client.request(start.clone()).await["type"], "ok");
    assert_eq!(
        client
            .request(json!({"method":"read_file","session":id,"path":"result.txt"}))
            .await["type"],
        "file"
    );
    assert_eq!(
        client
            .request(json!({"method":"read_file","session":id,"path":"config.toml"}))
            .await["type"],
        "error"
    );
    assert_eq!(
        client
            .request(json!({"method":"read_file","session":id,"path":"../outside"}))
            .await["type"],
        "error"
    );
    let cursor = page["value"]["next"].as_u64().unwrap();
    let empty = client
        .request(json!({"method":"poll","session":id,"after":cursor}))
        .await;
    assert!(empty["value"]["updates"].as_array().unwrap().is_empty());
    assert_eq!(
        client
            .request(json!({"method":"poll","session":id,"after":cursor+1}))
            .await["type"],
        "error"
    );
    assert_eq!(
        client.request(json!({"method":"shutdown"})).await["type"],
        "ok"
    );
    drop(client);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut client = Client::connect(root).await;
    assert_eq!(
        client.request(json!({"method":"open","resume":id})).await["type"],
        "opened"
    );
    assert_eq!(client.request(start).await["type"], "ok");
    let restored = client
        .request(json!({"method":"poll","session":id,"after":0}))
        .await;
    assert_eq!(restored["value"]["busy"], false);
    assert_eq!(
        restored["value"]["updates"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|update| update.get("Started").is_some())
            .count(),
        1
    );
    assert_eq!(
        client.request(json!({"method":"shutdown"})).await["type"],
        "ok"
    );
    model.await.unwrap();
}

#[tokio::test]
async fn ssh_and_wsl_launchers_use_the_same_service_and_ignore_shell_banners() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let bin = transport_launchers(root);
    let workspace = root.join("project 'quoted' $(touch unexpected)");
    std::fs::create_dir(&workspace).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    for target in [
        vec![],
        vec!["--ssh", "test-alias"],
        vec!["--wsl", "Test Distro", "--wsl-user", "dev"],
    ] {
        for operation in ["--list", "--stop"] {
            let output = Command::new(env!("CARGO_BIN_EXE_orbit"))
                .arg("connect")
                .args(&target)
                .arg("--server")
                .arg(env!("CARGO_BIN_EXE_orbit"))
                .arg("--workspace")
                .arg(&workspace)
                .arg("--database")
                .arg(root.join("test.db"))
                .arg("--auth-dir")
                .arg(root.join("auth"))
                .arg(operation)
                .env("PATH", &path)
                .env("HOME", root)
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!String::from_utf8_lossy(&output.stdout).contains("test login banner"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!root.join("unexpected").exists());
}

#[tokio::test]
async fn cancelling_a_disconnected_pending_approval_does_not_write_the_file() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (url, model) = mock_model().await;
    let _model = tokio_util::task::AbortOnDropHandle::new(model);
    std::fs::write(
        root.join("config.toml"),
        format!(
            r#"default_provider = "test"
[providers.test]
base_url = "{url}"
api_key = "test-only"
auto_refresh_models = false
models = [{{ id = "mock" }}]
"#
        ),
    )
    .unwrap();
    let mut client = Client::connect(root).await;
    let opened = client.request(json!({"method":"open","resume":null})).await;
    let id = opened["value"]["id"].as_str().unwrap().to_owned();
    assert_eq!(client.request(json!({"method":"start","session":id,"request_id":"cancel-me","prompt":"write a file","mode":"ask","model":null})).await["type"], "ok");
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let page = client
                .request(json!({"method":"poll","session":id,"after":0}))
                .await;
            if !page["value"]["approvals"].as_array().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    drop(client);
    let mut client = Client::connect(root).await;
    assert_eq!(
        client
            .request(json!({"method":"cancel","session":id}))
            .await["type"],
        "ok"
    );
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let page = client.request(json!({"method":"poll","session":id,"after":0})).await;
            if page["value"]["busy"] == false {
                assert_eq!(page["value"]["updates"].as_array().unwrap().last().unwrap()["Finished"]["status"], "cancelled");
                assert!(page["value"]["approvals"].as_array().unwrap().is_empty());
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.unwrap();
    assert!(!root.join("result.txt").exists());
    assert_eq!(
        client.request(json!({"method":"shutdown"})).await["type"],
        "ok"
    );
}

fn transport_launchers(root: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    for program in ["ssh", "wsl.exe"] {
        let path = bin.join(program);
        std::fs::write(
            &path,
            r#"#!/usr/bin/python3
import os,sys
print('test login banner', flush=True)
if os.path.basename(sys.argv[0]) == 'ssh':
    assert sys.argv[-2] == 'test-alias'
    os.execvp('sh', ['sh', '-c', sys.argv[-1]])
else:
    assert sys.argv[1:3] == ['--distribution', 'Test Distro']
    args = sys.argv[sys.argv.index('--exec')+1:]
    os.execvp(args[0], args)
"#,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    bin
}

#[tokio::test]
async fn remote_prompt_uses_local_configuration_and_credentials_in_an_unconfigured_project() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let bin = transport_launchers(root);
    for target in [vec!["--ssh", "test-alias"], vec!["--wsl", "Test Distro"]] {
        let project = tempfile::tempdir_in(root).unwrap();
        let local = tempfile::tempdir_in(root).unwrap();
        let (url, model) = mock_model().await;
        let auth = local.path().join("auth");
        orbit_auth::AuthStore::new(auth.clone())
            .unwrap()
            .set_api_key("local-only", "test-only".into(), &url)
            .await
            .unwrap();
        let config = local.path().join("local.toml");
        std::fs::write(
            &config,
            format!(
                r#"default_provider = "test"
[providers.test]
base_url = "{url}"
credential = "local-only"
auto_refresh_models = false
models = [{{ id = "mock" }}]
"#
            ),
        )
        .unwrap();
        let output = tokio::time::timeout(
            Duration::from_secs(30),
            Command::new(env!("CARGO_BIN_EXE_orbit"))
                .arg("connect")
                .args(&target)
                .arg("--server")
                .arg(env!("CARGO_BIN_EXE_orbit"))
                .arg("--workspace")
                .arg(project.path())
                .arg("--config")
                .arg(&config)
                .arg("--auth-dir")
                .arg(&auth)
                .arg("--database")
                .arg(project.path().join("sessions.db"))
                .args(["--prompt", "write a file"])
                .env(
                    "PATH",
                    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
                )
                .env("HOME", root)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join("result.txt")).unwrap(),
            "written remotely"
        );
        assert!(!project.path().join("config.toml").exists());
        assert!(!project.path().join("auth").exists());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("test-only"));
        model.await.unwrap();
        Command::new(env!("CARGO_BIN_EXE_orbit"))
            .arg("connect")
            .args(&target)
            .arg("--server")
            .arg(env!("CARGO_BIN_EXE_orbit"))
            .arg("--workspace")
            .arg(project.path())
            .arg("--database")
            .arg(project.path().join("sessions.db"))
            .arg("--stop")
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("HOME", root)
            .output()
            .await
            .unwrap();
    }
}
