use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::Value;

fn receive_request(stream: &mut TcpStream) -> Value {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0, "model request ended before its headers");
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length: ")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .expect("request content-length");
    while bytes.len() - header_end < content_length {
        let count = stream.read(&mut chunk).unwrap();
        assert!(count > 0, "model request ended before its body");
        bytes.extend_from_slice(&chunk[..count]);
    }
    serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap()
}

#[test]
fn selected_provider_and_prompt_file_survive_chat_resume() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::clone(&received);
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            requests.lock().unwrap().push(receive_request(&mut stream));
            let payload = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            )
            .unwrap();
        }
    });

    let config = root.path().join("config.toml");
    let database = root.path().join("sessions.db");
    let prompt_file = root.path().join("prompt.txt");
    std::fs::write(&prompt_file, "First line\nSecond line\n").unwrap();
    std::fs::write(
        &config,
        format!(
            "default_provider = 'first'\n\n[providers.first]\nbase_url = 'http://127.0.0.1:9/v1'\napi_key = 'test'\n[[providers.first.models]]\nid = 'first'\n\n[providers.second]\nbase_url = 'http://{address}/v1'\napi_key = 'test'\n[[providers.second.models]]\nid = 'second'\n[providers.second.models.reasoning]\nadapter = 'openai_chat'\nefforts = ['low', 'high']\ndefault = 'low'\n[providers.second.models.reasoning.extra]\nenable_thinking = true\n"
        ),
    )
    .unwrap();

    let first = Command::new(env!("CARGO_BIN_EXE_orbit"))
        .current_dir(root.path())
        .args(["--config", config.to_str().unwrap()])
        .args(["--database", database.to_str().unwrap()])
        .args([
            "chat",
            "--approval",
            "deny",
            "--model",
            "second::second",
            "--reasoning",
            "high",
        ])
        .args(["--prompt-file", prompt_file.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let stderr = String::from_utf8_lossy(&first.stderr);
    let session_id = stderr
        .lines()
        .find_map(|line| line.strip_prefix("session "))
        .expect("new session id");

    let second = Command::new(env!("CARGO_BIN_EXE_orbit"))
        .current_dir(root.path())
        .args(["--config", config.to_str().unwrap()])
        .args(["--database", database.to_str().unwrap()])
        .args([
            "chat",
            "--approval",
            "deny",
            "--resume",
            session_id,
            "Continue",
        ])
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    server.join().unwrap();

    let requests = received.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["model"], "second");
    assert_eq!(requests[1]["model"], "second");
    assert_eq!(requests[0]["reasoning_effort"], "high");
    assert_eq!(requests[1]["reasoning_effort"], "high");
    assert_eq!(requests[0]["enable_thinking"], true);
    assert!(
        requests[0]["messages"]
            .to_string()
            .contains("First line\\nSecond line")
    );
    assert!(requests[1]["messages"].to_string().contains("Continue"));
}
