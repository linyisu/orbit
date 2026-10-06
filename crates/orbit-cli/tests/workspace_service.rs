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
        Self::connect_with(
            root,
            Path::new(env!("ORBIT_EMBEDDED_SERVER")),
            &root.join("config.toml"),
            &root.join("sessions.db"),
        )
        .await
    }

    async fn connect_with(root: &Path, executable: &Path, config: &Path, database: &Path) -> Self {
        let mut child = Command::new(executable)
            .arg("workspace-service")
            .arg("--workspace")
            .arg(root)
            .arg("--config")
            .arg(config)
            .arg("--database")
            .arg(database)
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
            client
                .request(json!({"method":"hello","version":orbit_protocol::VERSION}))
                .await["type"],
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
    mock_tool_model(
        "write_file",
        json!({"path":"result.txt","content":"written remotely"}),
        None,
    )
    .await
}

async fn mock_tool_model(
    name: &'static str,
    arguments: Value,
    expected: Option<&'static str>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let mut index = 0;
        while index < 2 {
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
            if request["stream"] == false {
                let response = json!({"id":"title","model":"mock","choices":[{"index":0,"message":{"role":"assistant","content":"test title"},"finish_reason":"stop"}]}).to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
                continue;
            }
            assert_eq!(request["stream"], true);
            let delta = if index == 0 {
                json!({"tool_calls":[{"index":0,"id":"write","type":"function","function":{"name":name,"arguments":arguments.to_string()}}]})
            } else {
                if let Some(expected) = expected {
                    assert!(
                        request["messages"].as_array().unwrap().iter().any(
                            |message| message["role"] == "tool"
                                && message["content"]
                                    .as_str()
                                    .is_some_and(|text| text.contains(expected))
                        ),
                        "tool result missing expected output"
                    );
                }
                json!({"content":"任务完成。"})
            };
            let finish = if index == 0 { "tool_calls" } else { "stop" };
            let events = format!(
                "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                json!({"choices":[{"index":0,"delta":delta}]}),
                json!({"choices":[{"index":0,"delta":{},"finish_reason":finish}]})
            );
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{events}", events.len()).as_bytes()).await.unwrap();
            index += 1;
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
    let opened = client
        .request(json!({"method":"open","resume":null,"model":"test::mock"}))
        .await;
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
    let upgraded = root.join("upgraded-server");
    std::fs::copy(env!("ORBIT_EMBEDDED_SERVER"), &upgraded).unwrap();
    let mut observer = Client::connect_with(
        root,
        &upgraded,
        &root.join("config.toml"),
        &root.join("sessions.db"),
    )
    .await;
    assert_eq!(
        observer.request(json!({"method":"open","resume":id})).await["type"],
        "opened"
    );
    let observed = observer
        .request(json!({"method":"poll","session":id,"after":0}))
        .await;
    assert_eq!(observed["value"]["busy"], true);
    assert_eq!(observed["value"]["approvals"][0]["id"], approval);
    let mut competing = start.clone();
    competing["request_id"] = "another-terminal".into();
    assert_eq!(observer.request(competing).await["type"], "error");
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
    assert_eq!(observer.request(json!({"method":"approve","session":id,"approval":approval,"allow":true,"remember":false})).await["type"], "ok");
    assert_eq!(client.request(json!({"method":"approve","session":id,"approval":approval,"allow":true,"remember":false})).await["type"], "error");
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
    let observed = observer
        .request(json!({"method":"poll","session":id,"after":0}))
        .await;
    assert_eq!(observed["value"]["busy"], false);
    assert_eq!(observed["value"]["approvals"], json!([]));
    for updates in [&page["value"]["updates"], &observed["value"]["updates"]] {
        assert_eq!(
            updates
                .as_array()
                .unwrap()
                .iter()
                .filter(|update| update.get("Started").is_some())
                .count(),
            1
        );
        assert!(updates.to_string().contains("任务完成。"));
    }
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
        updates
            .iter()
            .rev()
            .find(|update| update.get("Finished").is_some())
            .unwrap()["Finished"]["status"],
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
    assert!(
        empty["value"]["updates"]
            .as_array()
            .unwrap()
            .iter()
            .all(|update| update.get("Profile").is_some())
    );
    let cursor = empty["value"]["next"].as_u64().unwrap();
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
    assert_eq!(client.request(json!({"method":"session_action","session":id,"action":{"action":"rename","title":"restored title"}})).await["type"], "report");
    let profile = client.request(json!({"method":"open","resume":id})).await;
    assert_eq!(profile["value"]["title"], "restored title");
    assert_eq!(
        client
            .request(json!({"method":"undo","session":id,"path":"result.txt"}))
            .await["type"],
        "ok"
    );
    let approval = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let page = client
                .request(json!({"method":"poll","session":id,"after":0}))
                .await;
            if let Some(approval) = page["value"]["approvals"].as_array().unwrap().first() {
                break approval["id"].as_u64().unwrap();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(root.join("result.txt").exists());
    drop(client);
    let mut client = Client::connect(root).await;
    assert_eq!(client.request(json!({"method":"approve","session":id,"approval":approval,"allow":true,"remember":false})).await["type"], "ok");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let page = client
                .request(json!({"method":"poll","session":id,"after":0}))
                .await;
            if page["value"]["busy"] == false {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(!root.join("result.txt").exists());
    assert!(
        store
            .workspace_changes(&id)
            .await
            .unwrap()
            .iter()
            .all(|change| change.undone)
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
            let output = Command::new(env!("CARGO_BIN_EXE_orbit-cli"))
                .arg("connect")
                .args(&target)
                .arg("--server")
                .arg(env!("ORBIT_EMBEDDED_SERVER"))
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
    let opened = client
        .request(json!({"method":"open","resume":null,"model":"test::mock"}))
        .await;
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
            let page = client
                .request(json!({"method":"poll","session":id,"after":0}))
                .await;
            if page["value"]["busy"] == false {
                assert_eq!(
                    page["value"]["updates"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .rev()
                        .find(|update| update.get("Finished").is_some())
                        .unwrap()["Finished"]["status"],
                    "cancelled"
                );
                assert!(page["value"]["approvals"].as_array().unwrap().is_empty());
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
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
            Command::new(env!("CARGO_BIN_EXE_orbit-cli"))
                .arg("connect")
                .args(&target)
                .arg("--server")
                .arg(env!("ORBIT_EMBEDDED_SERVER"))
                .arg("--workspace")
                .arg(project.path())
                .arg("--config")
                .arg(&config)
                .arg("--auth-dir")
                .arg(&auth)
                .arg("--database")
                .arg(project.path().join("sessions.db"))
                .args(["--model", "test::mock"])
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
        Command::new(env!("CARGO_BIN_EXE_orbit-cli"))
            .arg("connect")
            .args(&target)
            .arg("--server")
            .arg(env!("ORBIT_EMBEDDED_SERVER"))
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

#[tokio::test]
async fn workspace_controls_follow_directory_and_preserve_saved_profile() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (url, model) = mock_tool_model("read_file", json!({"path":"proof.txt"}), None).await;
    std::fs::write(
        root.join("config.toml"),
        format!(
            r#"default_provider = "test"
[providers.test]
base_url = "{url}"
api_key = "test-only"
auto_refresh_models = false
models = [{{ id = "mock" }}, {{ id = "other" }}]
"#
        ),
    )
    .unwrap();
    let project = root.join("next project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(project.join("proof.txt"), "target directory").unwrap();
    let mut client = Client::connect(root).await;
    let opened = client
        .request(json!({"method":"open","model":"test::mock"}))
        .await;
    assert_eq!(opened["type"], "opened", "{opened}");
    let id = opened["value"]["id"].as_str().unwrap().to_owned();
    let completion = client
        .request(json!({"method":"complete","session":id,"input":"/cd next"}))
        .await;
    assert_eq!(completion["type"], "completions", "{completion}");
    assert_eq!(
        completion["value"]["entries"][0]["display"],
        "next project/"
    );
    for request in [
        json!({"method":"set_mode","session":id,"mode":"ask"}),
        json!({"method":"set_model","session":id,"model":"test::other","effort":null}),
        json!({"method":"configure_models","session":id,"configuration":null}),
    ] {
        let response = client.request(request).await;
        assert_eq!(response["type"], "ok", "{response}");
    }
    let moved = client.request(json!({"method":"session_action","session":id,"action":{"action":"change_directory","path":"'~/next project'"}})).await;
    assert_eq!(moved["type"], "ok", "{moved}");
    let page = client
        .request(json!({"method":"poll","session":id,"after":0}))
        .await;
    let updates = page["value"]["updates"].as_array().unwrap();
    assert!(
        updates.iter().all(|update| update.get("Report").is_none()),
        "{page}"
    );
    assert!(
        updates
            .iter()
            .any(|update| update["Profile"]["workspace"] == project.to_string_lossy().as_ref()),
        "{page}"
    );
    let invalid = client.request(json!({"method":"session_action","session":id,"action":{"action":"change_directory","path":"missing-directory"}})).await;
    assert_eq!(invalid["type"], "error", "{invalid}");
    let info = client.request(json!({"method":"open","resume":id})).await;
    assert_eq!(
        info["value"]["workspace"],
        project.to_string_lossy().as_ref()
    );
    assert_eq!(info["value"]["mode"], "ask");
    assert_eq!(info["value"]["model"], "test::other");
    let file = client
        .request(json!({"method":"read_file","session":id,"path":"proof.txt"}))
        .await;
    assert_eq!(file["type"], "file", "{file}");
    let task = client.request(json!({"method":"session_action","session":id,"action":{"action":"task","task":{"action":"add","title":"service task"}}})).await;
    assert_eq!(task["type"], "report", "{task}");
    let task_completion = client
        .request(json!({"method":"complete","session":id,"input":"/tasks done "}))
        .await;
    assert_eq!(
        task_completion["value"]["entries"][0]["description"], "service task",
        "{task_completion}"
    );
    let list = client
        .request(json!({"method":"list_sessions","session":id}))
        .await;
    assert!(list["value"].as_array().unwrap().is_empty(), "{list}");
    let completions = client
        .request(json!({"method":"complete","session":id,"input":"/resume "}))
        .await;
    assert!(
        completions["value"]["entries"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{completions}"
    );
    let store = orbit_store::store::Store::open(root.join("sessions.db"))
        .await
        .unwrap();
    let saved = client
        .request(json!({"method":"open","model":"test::mock"}))
        .await;
    assert_eq!(saved["type"], "opened", "{saved}");
    let saved_id = saved["value"]["id"].as_str().unwrap().to_owned();
    let moved = client.request(json!({"method":"session_action","session":saved_id,"action":{"action":"change_directory","path":project}})).await;
    assert_eq!(moved["type"], "ok", "{moved}");
    let started = client.request(json!({"method":"start","session":saved_id,"request_id":"history-run","prompt":"read proof.txt","mode":"auto","model":null})).await;
    assert_eq!(started["type"], "ok", "{started}");
    let page = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let page = client
                .request(json!({"method":"poll","session":saved_id,"after":0}))
                .await;
            if page["value"]["busy"] == false {
                break page;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(page.to_string().contains("target directory"), "{page}");
    let list = client
        .request(json!({"method":"list_sessions","session":id}))
        .await;
    assert_eq!(list["value"].as_array().unwrap().len(), 1, "{list}");
    assert_eq!(list["value"][0]["id"], saved_id);
    let completions = client
        .request(json!({"method":"complete","session":id,"input":"/resume "}))
        .await;
    assert_eq!(
        completions["value"]["entries"].as_array().unwrap().len(),
        1,
        "{completions}"
    );
    assert_eq!(
        completions["value"]["entries"][0]["name"],
        format!("/resume {saved_id}")
    );
    let legacy_window = store.open_tui_window().await.unwrap();
    legacy_window.update(vec![id.clone()]).await.unwrap();
    legacy_window.close().await.unwrap();
    let restored = client.request(json!({"method":"restore_sessions"})).await;
    assert!(
        restored["value"]
            .as_array()
            .unwrap()
            .iter()
            .any(|session| session["id"] == id),
        "{restored}"
    );
    let deletion = client
        .request(json!({"method":"admin","action":{"action":"delete_session","id":id}}))
        .await;
    assert_eq!(deletion["type"], "report", "{deletion}");
    assert_eq!(
        client.request(json!({"method":"shutdown"})).await["type"],
        "ok"
    );
    model.abort();
}

#[tokio::test]
async fn native_execution_and_model_inheritance_use_the_packaged_local_service() {
    use orbit_client::{
        configuration::Local,
        connect::Opened,
        interaction,
        transport::{Connection, Target},
    };
    use orbit_protocol::{Mode, Request, Response};
    use std::sync::Arc;
    if std::env::var_os("ORBIT_TEST_REQUIRE_NO_SANDBOX").is_some() {
        assert!(!Path::new("/usr/bin/bwrap").exists());
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let (url, model) = mock_tool_model(
        "run_command",
        json!({"command":"echo native-command-ok; pwd"}),
        Some("native-command-ok"),
    )
    .await;
    let config = root.join("config.toml");
    std::fs::write(&config, format!("default_provider = \"test\"\n[providers.test]\nbase_url = \"{url}\"\napi_key = \"test-only\"\nauto_refresh_models = false\nmodels = [{{ id = \"first\" }}, {{ id = \"selected\" }}]\n")).unwrap();
    let auth = root.join("auth");
    let connection = Arc::new(Connection {
        local_configuration: Some(Local {
            config: config.clone(),
            auth: auth.clone(),
            model: Some("test::selected".into()),
        }),
        target: Target::Local,
        workspace: root.to_string_lossy().into(),
        executable: env!("ORBIT_EMBEDDED_SERVER").into(),
        config,
        database: Some(root.join("sessions.db")),
        auth_dir: Some(auth),
        allow_mcp_start: false,
        unattended: true,
        interactive: None,
    });
    let mut opened = Opened::connect_progress(connection, None, interaction::channel().0)
        .await
        .unwrap();
    assert_eq!(opened.info.model, "test::selected");
    let id = opened.info.id.clone();
    assert!(matches!(
        opened
            .client
            .request(Request::Start {
                session: id.clone(),
                request_id: "native-command".into(),
                prompt: "execute command".into(),
                images: vec![],
                compact: false,
                mode: Mode::Yolo,
                model: None
            })
            .await
            .unwrap(),
        Response::Ok
    ));
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let Response::Page(page) = opened
                .client
                .request(Request::Poll {
                    session: id.clone(),
                    after: 0,
                })
                .await
                .unwrap()
            else {
                panic!("expected events")
            };
            if !page.busy {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    model.await.unwrap();
    let store = orbit_store::store::Store::open(root.join("sessions.db"))
        .await
        .unwrap();
    let snapshot = store.load(&id).await.unwrap();
    let history = serde_json::to_string(&snapshot.history).unwrap();
    assert!(history.contains("native-command-ok"));
    assert!(history.contains(root.to_str().unwrap()));
    let mut connection = (*opened.connection).clone();
    let local = connection.local_configuration.as_mut().unwrap();
    local.model = None;
    local.remember_model("test::first").unwrap();
    let connection = Arc::new(connection);
    let fresh = Opened::connect_progress(connection.clone(), None, interaction::channel().0)
        .await
        .unwrap();
    assert_eq!(fresh.info.model, "test::first");
    let restored = Opened::connect_progress(connection.clone(), Some(id), interaction::channel().0)
        .await
        .unwrap();
    assert_eq!(restored.info.model, "test::selected");
    assert_eq!(
        connection
            .local_configuration
            .as_ref()
            .unwrap()
            .last_model()
            .unwrap()
            .as_deref(),
        Some("test::first")
    );
    assert!(matches!(
        opened.client.request(Request::Shutdown).await.unwrap(),
        Response::Ok
    ));
}

#[tokio::test]
async fn native_execution_refuses_an_explicit_unavailable_sandbox() {
    if orbit_runtime::execution::capabilities::check_sandbox()
        .await
        .is_ok()
    {
        assert!(
            std::env::var_os("ORBIT_TEST_REQUIRE_NO_SANDBOX").is_none(),
            "this CI scenario must not provide a sandbox"
        );
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("config.toml"),
        r#"default_provider = "test"
[command]
sandbox = true
[providers.test]
base_url = "http://127.0.0.1:9/v1"
api_key = "test-only"
auto_refresh_models = false
models = [{ id = "mock" }]
"#,
    )
    .unwrap();
    let mut client = Client::connect(root).await;
    let opened = client
        .request(json!({"method":"open","model":"test::mock"}))
        .await;
    assert_eq!(opened["type"], "error", "{opened}");
    assert!(
        opened["value"].as_str().unwrap().contains("sandbox"),
        "{opened}"
    );
    assert_eq!(
        client.request(json!({"method":"shutdown"})).await["type"],
        "ok"
    );
}

#[tokio::test]
async fn packaged_service_lists_history_created_by_the_native_store() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    std::fs::write(
        root.join("config.toml"),
        r#"default_provider = "test"
[providers.test]
base_url = "http://127.0.0.1:1"
api_key = "test-only"
auto_refresh_models = false
models = [{ id = "mock" }]
"#,
    )
    .unwrap();
    let store = orbit_store::store::Store::open(root.join("sessions.db"))
        .await
        .unwrap();
    let mut writer = store
        .create(root.display().to_string(), "test::mock".into(), false)
        .await
        .unwrap();
    let history_id = writer.id().to_owned();
    writer
        .begin("existing conversation".into(), "test::mock".into())
        .await
        .unwrap();
    writer
        .finish(
            "completed",
            Vec::new(),
            Some(vec![orbit_model::types::ChatMessage::user(
                "existing conversation",
            )]),
        )
        .await
        .unwrap();
    drop(writer);

    let mut client = Client::connect(root).await;
    let opened = client
        .request(json!({"method":"open","model":"test::mock"}))
        .await;
    assert_eq!(opened["type"], "opened", "{opened}");
    let id = opened["value"]["id"].as_str().unwrap();
    for request in [
        json!({"method":"sessions"}),
        json!({"method":"list_sessions","session":id}),
    ] {
        let history = client.request(request).await;
        assert_eq!(history["value"].as_array().unwrap().len(), 1, "{history}");
        assert_eq!(history["value"][0]["id"], history_id);
    }
    let completions = client
        .request(json!({"method":"complete","session":id,"input":"/resume "}))
        .await;
    assert_eq!(
        completions["value"]["entries"][0]["name"],
        format!("/resume {history_id}")
    );
    assert_eq!(
        store.session(id).await.unwrap().project_id,
        store.session(&history_id).await.unwrap().project_id
    );
    assert_eq!(store.projects().await.unwrap().len(), 1);
    assert_eq!(
        client.request(json!({"method":"shutdown"})).await["type"],
        "ok"
    );
}

#[tokio::test]
async fn model_selection_is_explicit_and_catalog_refresh_never_switches_it() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let config = r#"[providers.test]
base_url = "http://127.0.0.1:1/v1"
auto_refresh_models = false
models = [{ id = "first" }, { id = "chosen" }]
"#;
    std::fs::write(root.join("config.toml"), config).unwrap();
    let mut client = Client::connect(root).await;
    let missing = client.request(json!({"method":"open"})).await;
    assert_eq!(missing["type"], "error", "{missing}");
    let opened = client
        .request(json!({"method":"open","model":"test::chosen"}))
        .await;
    assert_eq!(opened["type"], "opened", "{opened}");
    assert_eq!(opened["value"]["model"], "test::chosen");
    let id = opened["value"]["id"].as_str().unwrap();
    let restored = client
        .request(json!({"method":"open","resume":id,"model":"test::first"}))
        .await;
    assert_eq!(restored["value"]["model"], "test::chosen");
    std::fs::write(
        root.join("config.toml"),
        config.replace(", { id = \"chosen\" }", ""),
    )
    .unwrap();
    let refreshed = client
        .request(json!({"method":"configure_models","session":id}))
        .await;
    assert_eq!(refreshed["type"], "ok", "{refreshed}");
    let profile = client.request(json!({"method":"open","resume":id})).await;
    assert_eq!(profile["value"]["model"], "test::chosen");
    assert_eq!(profile["value"]["models"], json!(["test::first"]));
    let refused = client.request(json!({"method":"start","session":id,"request_id":"removed","prompt":"hello","mode":"auto"})).await;
    assert_eq!(refused["type"], "error", "{refused}");
    let selected = client
        .request(json!({"method":"set_model","session":id,"model":"test::first"}))
        .await;
    assert_eq!(selected["type"], "ok", "{selected}");
    let profile = client.request(json!({"method":"open","resume":id})).await;
    assert_eq!(profile["value"]["model"], "test::first");
    assert_eq!(
        client.request(json!({"method":"shutdown"})).await["type"],
        "ok"
    );
}

#[tokio::test]
async fn two_sessions_share_a_service_across_executables_configs_and_database_aliases() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    for name in ["first", "second"] {
        std::fs::write(
            root.join(format!("{name}.toml")),
            format!(
                r#"[providers.{name}]
base_url = "http://127.0.0.1:1/v1"
api_key = "test-only"
auto_refresh_models = false
models = [{{ id = "mock" }}]
"#
            ),
        )
        .unwrap();
    }
    let upgraded = root.join("upgraded-server");
    std::fs::copy(env!("ORBIT_EMBEDDED_SERVER"), &upgraded).unwrap();
    let database = root.join("sessions.db");
    let alias = root.join("alias.db");
    std::os::unix::fs::symlink(&database, &alias).unwrap();
    let mut first = Client::connect_with(
        root,
        Path::new(env!("ORBIT_EMBEDDED_SERVER")),
        &root.join("first.toml"),
        &database,
    )
    .await;
    let mut second = Client::connect_with(root, &upgraded, &root.join("second.toml"), &alias).await;
    let a = first
        .request(json!({"method":"open","model":"first::mock"}))
        .await;
    let b = second
        .request(json!({"method":"open","model":"second::mock"}))
        .await;
    assert_eq!(a["type"], "opened", "{a}");
    assert_eq!(b["type"], "opened", "{b}");
    let a = a["value"]["id"].as_str().unwrap().to_owned();
    let b = b["value"]["id"].as_str().unwrap().to_owned();
    assert_ne!(a, b);
    let (a_restored, b_restored) = tokio::join!(
        second.request(json!({"method":"open","resume":a})),
        first.request(json!({"method":"open","resume":b})),
    );
    assert_eq!(a_restored["type"], "opened", "{a_restored}");
    assert_eq!(b_restored["type"], "opened", "{b_restored}");
    assert_eq!(a_restored["value"]["model"], "first::mock");
    assert_eq!(b_restored["value"]["model"], "second::mock");
    for id in [&a, &b] {
        assert_eq!(
            second
                .request(json!({"method":"set_mode","session":id,"mode":"ask"}))
                .await["type"],
            "ok"
        );
        assert_eq!(first.request(json!({"method":"session_action","session":id,"action":{"action":"rename","title":id}})).await["type"], "report");
        let left = first
            .request(json!({"method":"poll","session":id,"after":0}))
            .await;
        let right = second
            .request(json!({"method":"poll","session":id,"after":0}))
            .await;
        assert_eq!(left, right);
        let profile = second.request(json!({"method":"open","resume":id})).await;
        assert_eq!(profile["value"]["title"], *id);
        assert_eq!(profile["value"]["mode"], "ask");
    }
    assert_eq!(
        second.request(json!({"method":"shutdown"})).await["type"],
        "ok"
    );
}
