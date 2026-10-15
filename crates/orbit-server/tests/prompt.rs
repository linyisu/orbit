#[cfg(test)]
use orbit_config::test_support::{TestConfig, TestModel, TestProvider};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn shared_prompt_uses_the_service_policy_and_persists_the_answer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let model = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                header.push(socket.read_u8().await.unwrap());
            }
            let header = String::from_utf8(header).unwrap();
            let length: usize = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|value| value.trim().parse().unwrap())
                })
                .unwrap();
            let mut body = vec![0; length];
            socket
                .read_exact(&mut body)
                .await
                .unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            let (kind, response) = if body["stream"] == true {
                assert!(
                    body.get("tools")
                        .is_none_or(|tools| tools
                            .as_array()
                            .is_some_and(Vec::is_empty)),
                    "{body}"
                );
                assert_eq!(body["model"], "second");
                (
                    "text/event-stream",
                    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Se\
                    rvice answer\"}}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_r\
                    eason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
                        .into(),
                )
            } else {
                (
                    "application/json",
                    json!({
                        "choices": [{
                            "message": {
                                "role": "assistant",
                                "content": "Title"
                            },
                            "finish_reason": "stop"
                        }]
                    })
                    .to_string(),
                )
            };
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: \
                {}\r\nConnection: close\r\n\r\n{response}",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let model = tokio_util::task::AbortOnDropHandle::new(model);
    std::fs::write(
        root.join("config.toml"),
        TestConfig::new()
            .provider(
                TestProvider::new("test")
                    .endpoint(format!("http://{address}/v1"))
                    .key("test")
                    .models([TestModel::new("first"), TestModel::new("second")]),
            )
            .providers_toml()
            .unwrap(),
    )
    .unwrap();
    use orbit_client::{
        configuration::Local,
        transport::{Connection, Target},
    };
    use orbit_protocol::{Capability, Mode, Request, Response, SessionPolicy};
    let connection = Connection {
        local_configuration: Some(Local {
            config: root.join("config.toml"),
            auth: root.join("auth"),
            model: None,
        }),
        target: Target::Local,
        workspace: root.to_string_lossy().into_owned(),
        executable: env!("CARGO_BIN_EXE_orbit-workspace-server").into(),
        config: root.join("config.toml"),
        database: Some(root.join("sessions.db")),
        auth_dir: Some(root.join("auth")),
        allow_mcp_start: false,
        unattended: true,
        interactive: None,
    };
    let mut client = connection.connect().await.unwrap();
    let Response::Opened(info) = client
        .request(Request::Open {
            resume: None,
            model: Some("test::second".into()),
            policy: SessionPolicy {
                read_only: true,
                disabled: vec![Capability::All],
            },
            configuration: None,
        })
        .await
        .unwrap()
    else {
        panic!("session did not open");
    };
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        orbit_client::prompt::execute(
            &mut client,
            &info.id,
            orbit_client::prompt::PromptOptions {
                prompt: "test prompt".into(),
                images: Vec::new(),
                mode: Mode::Auto,
                model: None,
            },
            tokio_util::sync::CancellationToken::new(),
            |_| Ok(()),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.answer, "Service answer");
    let store = orbit_store::store::Store::open(root.join("sessions.db"))
        .await
        .unwrap();
    let sessions = store.list().await.unwrap();
    assert_eq!(sessions.len(), 1);
    let snapshot = store
        .load(&sessions[0].id)
        .await
        .unwrap();
    assert_eq!(snapshot.session.model, "test::second");
    assert!(
        serde_json::to_string(&snapshot.history)
            .unwrap()
            .contains("Service answer")
    );
    assert!(matches!(
        client
            .request(Request::Shutdown)
            .await
            .unwrap(),
        Response::Ok
    ));
    drop(model);
}
