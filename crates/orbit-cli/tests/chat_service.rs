#[cfg(test)]
use orbit_config::test_support::{TestConfig, TestModel, TestProvider};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    process::Command,
};

#[tokio::test]
async fn headless_chat_uses_the_service_policy_and_persists_the_answer() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
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
            socket.read_exact(&mut body).await.unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            let (kind, response) = if body["stream"] == true {
                assert!(
                    body.get("tools")
                        .is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty)),
                    "{body}"
                );
                assert_eq!(body["model"], "second");
                ("text/event-stream", "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Service answer\"}}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".into())
            } else {
                ("application/json", json!({"choices":[{"message":{"role":"assistant","content":"Title"},"finish_reason":"stop"}]}).to_string())
            };
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
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
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        Command::new(env!("CARGO_BIN_EXE_orbit-cli"))
            .current_dir(root)
            .env("HOME", root)
            .env("XDG_CACHE_HOME", root.join("cache"))
            .args([
                "--config",
                "config.toml",
                "--database",
                "sessions.db",
                "--auth-dir",
                "auth",
                "chat",
                "--mode",
                "read-only",
                "--disable",
                "all",
                "--model",
                "second",
                "test prompt",
            ])
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
        String::from_utf8(output.stdout).unwrap().trim(),
        "Service answer"
    );
    let store = orbit_store::store::Store::open(root.join("sessions.db"))
        .await
        .unwrap();
    let sessions = store.list().await.unwrap();
    assert_eq!(sessions.len(), 1);
    let snapshot = store.load(&sessions[0].id).await.unwrap();
    assert_eq!(snapshot.session.model, "test::second");
    assert!(
        serde_json::to_string(&snapshot.history)
            .unwrap()
            .contains("Service answer")
    );
    let stop = Command::new(env!("CARGO_BIN_EXE_orbit-cli"))
        .current_dir(root)
        .env("HOME", root)
        .env("XDG_CACHE_HOME", root.join("cache"))
        .args([
            "--config",
            "config.toml",
            "--database",
            "sessions.db",
            "--auth-dir",
            "auth",
            "connect",
            "--stop",
        ])
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
    drop(model);
}
