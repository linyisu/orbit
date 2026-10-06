use std::time::Duration;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::task::AbortOnDropHandle;

use super::*;

async fn fixture(deltas: usize) -> (tempfile::TempDir, PathBuf, AbortOnDropHandle<()>) {
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            r#"default_provider = "test"
auto_compact = false
[providers.test]
base_url = "http://{address}/v1"
api_key = "test-only"
auto_refresh_models = false
max_retries = 0
[[providers.test.models]]
id = "mock"
"#
        ),
    )
    .unwrap();
    let server = AbortOnDropHandle::new(tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut bytes = [0; 4096];
        loop {
            let length = socket.read(&mut bytes).await.unwrap();
            if length == 0 {
                return;
            }
            request.extend_from_slice(&bytes[..length]);
            if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
                let body_length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .unwrap()
                    .trim()
                    .parse::<usize>()
                    .unwrap();
                if request.len() >= header_end + 4 + body_length {
                    break;
                }
            }
        }
        let mut events = String::new();
        for _ in 0..deltas {
            events.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Orbit answer\"},\"finish_reason\":null}]}\n\n");
        }
        events.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{events}",
            events.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
    }));
    (directory, config, server)
}

fn options(workspace: &Path) -> ChatOptions {
    ChatOptions {
        workspace: workspace.into(),
        prompt: "test prompt".into(),
        model: None,
        mode: ChatMode::ReadOnly,
        resume: None,
        allow_mcp_start: false,
        images: vec![],
        disabled: vec![ChatCapability::All],
        export_patch: false,
    }
}

#[tokio::test]
async fn application_chat_delivers_events_and_persists_its_injected_workspace() {
    let (directory, config, _server) = fixture(1).await;
    let database = directory.path().join("sessions.db");
    let mut text = String::new();
    let result = run(
        config,
        Some(database.clone()),
        options(directory.path()),
        directory.path().join("auth"),
        CancellationToken::new(),
        |update| {
            if let ChatUpdate::Agent(AgentEvent::Model(
                orbit_model::types::StreamEvent::TextDelta(delta),
            )) = update
            {
                text.push_str(delta);
            }
            Ok(())
        },
    )
    .await
    .unwrap();
    assert!(matches!(result, ChatOutcome::Completed { answer, .. } if answer == "Orbit answer"));
    assert_eq!(text, "Orbit answer");
    let store = orbit_store::store::Store::open(database).await.unwrap();
    let sessions = store.list().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].workspace, directory.path().to_str().unwrap());
    let snapshot = store.load(&sessions[0].id).await.unwrap();
    assert_eq!(snapshot.runs[0].status, "completed");
}

#[tokio::test]
async fn a_failed_event_consumer_cancels_the_run_and_releases_the_writer() {
    let (directory, config, _server) = fixture(512).await;
    let database = directory.path().join("sessions.db");
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        run(
            config,
            Some(database.clone()),
            options(directory.path()),
            directory.path().join("auth"),
            CancellationToken::new(),
            |update| {
                if matches!(update, ChatUpdate::Agent(AgentEvent::Model(_))) {
                    anyhow::bail!("output disconnected");
                }
                Ok(())
            },
        ),
    )
    .await
    .unwrap();
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("output disconnected")
    );
    let store = orbit_store::store::Store::open(database).await.unwrap();
    let sessions = store.list().await.unwrap();
    assert_eq!(
        store.load(&sessions[0].id).await.unwrap().runs[0].status,
        "failed"
    );
    assert!(store.acquire(&sessions[0].id).await.is_ok());
}
