//! Offline framed workspace peer for session control and model refresh regression tests.

use std::io::{Read, Write};

use anyhow::Result;
use serde_json::{Value, json};

fn main() -> Result<()> {
    let mut source = std::io::stdin().lock();
    let mut target = std::io::stdout().lock();
    target.write_all(orbit_protocol::PREAMBLE)?;
    target.flush()?;
    loop {
        let mut header = [0; 4];
        match source.read_exact(&mut header) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error.into()),
        }
        let mut payload = vec![0; u32::from_be_bytes(header) as usize];
        source.read_exact(&mut payload)?;
        let request: Value = serde_json::from_slice(&payload)?;
        let response = match request["method"].as_str() {
            Some("hello") => json!({
                "type": "hello",
                "value": {
                    "version": request["version"],
                    "workspace": ".",
                    "capabilities": {
                        "system": std::env::consts::OS,
                        "architecture": std::env::consts::ARCH,
                        "shell": "synthetic-peer",
                        "native_processes": true,
                        "files": true,
                        "features": [orbit_protocol::MODEL_INDEPENDENT_SESSIONS],
                    },
                },
            }),
            Some("all_sessions") => json!({"type": "sessions", "value": []}),
            Some("open" | "browse") => json!({
                "type": "opened",
                "value": {
                    "id": "synthetic-session",
                    "phase": "browsing",
                    "title": "Generated observation",
                    "workspace": ".",
                    "model": "slow::declared-model",
                    "models": ["slow::declared-model"],
                    "supports_vision": false,
                    "reasoning_effort": null,
                    "reasoning_efforts": [],
                    "context_tokens": 32_000,
                    "input_context_tokens": 28_000,
                    "max_output_tokens": 4_000,
                },
            }),
            Some("poll") => json!({
                "type": "page",
                "value": {
                    "next": 0,
                    "more": false,
                    "busy": true,
                    "model": "slow::declared-model",
                    "updates": [],
                    "approvals": [],
                },
            }),
            Some("configure_models") => {
                assert_eq!(request["session"], "synthetic-session");
                assert!(
                    request["configuration"]["content"]
                        .as_str()
                        .unwrap()
                        .contains("refreshed-model")
                );
                json!({"type": "ok"})
            }
            _ => json!({"type": "ok"}),
        };
        let payload = serde_json::to_vec(&response)?;
        target.write_all(&u32::try_from(payload.len())?.to_be_bytes())?;
        target.write_all(&payload)?;
        target.flush()?;
    }
}
