//! Synthetic stdio MCP peer for transport lifecycle tests, without external services.

use std::{
    fs::{self, OpenOptions},
    io::{BufRead, Write},
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::Result;
use serde_json::{Value, json};

const STALLED_PEER_DELAY: Duration = Duration::from_secs(60);

fn send(target: &mut impl Write, mut value: Value) -> Result<()> {
    value["jsonrpc"] = "2.0".into();
    serde_json::to_writer(&mut *target, &value)?;
    target.write_all(b"\n")?;
    target.flush()?;
    Ok(())
}

fn main() -> Result<()> {
    let mode = std::env::args()
        .nth(1)
        .unwrap_or_default();
    if mode == "--descendant" {
        std::thread::sleep(STALLED_PEER_DELAY);
        return Ok(());
    }
    if mode == "startup_error" {
        eprintln!("missing MCP subcommand: use --mcp");
        std::process::exit(7);
    }
    let root = std::env::current_dir()?;
    let starts = root.join("starts");
    let generation = match fs::read_to_string(&starts) {
        Ok(value) => value.parse::<usize>()? + 1,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 1,
        Err(error) => return Err(error.into()),
    };
    fs::write(starts, generation.to_string())?;
    let mut calls = 0;
    let mut source = std::io::stdin().lock().lines();
    let mut target = std::io::stdout().lock();
    while let Some(line) = source.next() {
        let message: Value = serde_json::from_str(&line?)?;
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("requests.jsonl"))?;
        let mut record = serde_json::to_vec(&message)?;
        record.push(b'\n');
        log.write_all(&record)?;
        let id = message["id"].clone();
        match message["method"].as_str() {
            Some("initialize") if mode != "init_timeout" => send(
                &mut target,
                json!({
                    "id": id,
                    "result": {"capabilities": {"tools": {"listChanged": mode == "list_changed"}}},
                }),
            )?,
            Some("tools/list") => {
                let cursor = message["params"].get("cursor");
                if matches!(mode.as_str(), "pages" | "cursor_loop" | "duplicates") {
                    let name = if cursor.is_none() || mode == "duplicates" {
                        "work"
                    } else {
                        "second"
                    };
                    let mut result = json!({
                        "tools": [{"name": name, "inputSchema": {"type": "object"}}],
                    });
                    if cursor.is_none() || mode == "cursor_loop" {
                        result["nextCursor"] = "page-two".into();
                    }
                    send(&mut target, json!({"id": id, "result": result}))?;
                    continue;
                }
                let mut schema = json!({"type": "object"});
                let name = if mode == "echo" { "echo" } else { "work" };
                if mode == "list_changed" && calls > 0 {
                    schema["required"] = json!(["changed"]);
                }
                if mode == "changed" && generation > 1 {
                    schema["required"] = json!(["new_field"]);
                }
                let mut tool = json!({"name": name, "inputSchema": schema});
                if mode == "echo" {
                    tool["description"] = "Echo input".into();
                }
                send(
                    &mut target,
                    json!({
                        "id": id,
                        "result": {"tools": [tool]},
                    }),
                )?;
                if mode == "blocked_write" {
                    std::thread::sleep(STALLED_PEER_DELAY);
                }
            }
            Some("tools/call") => {
                let name = if mode == "echo" { "echo" } else { "work" };
                assert_eq!(message["params"]["name"], name);
                calls += 1;
                match mode.as_str() {
                    "echo" => send(
                        &mut target,
                        json!({"id": id, "result": {"content": [{"type": "text", "text": "ok"}]}}),
                    )?,
                    "structured" => send(
                        &mut target,
                        json!({"id": id, "result": {"content": [], "structuredContent": {"count": 3}}}),
                    )?,
                    "list_changed" => {
                        send(&mut target, json!({"id": id, "result": {"content": []}}))?;
                        send(
                            &mut target,
                            json!({"method": "notifications/tools/list_changed"}),
                        )?;
                    }
                    "rpc_error" if calls == 1 => send(
                        &mut target,
                        json!({"id": id, "error": {"code": -32_000, "message": "busy"}}),
                    )?,
                    "tool_error" if calls == 1 => send(
                        &mut target,
                        json!({"id": id, "result": {"isError": true, "content": []}}),
                    )?,
                    "timeout" | "partial" | "changed" | "malformed" | "exit" if generation == 1 => {
                        let child = Command::new(std::env::current_exe()?)
                            .arg("--descendant")
                            .stdout(Stdio::null())
                            .spawn()?;
                        fs::write(root.join("child.pid"), child.id().to_string())?;
                        match mode.as_str() {
                            "partial" => target.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":")?,
                            "malformed" => target.write_all(b"invalid-json\n")?,
                            "exit" => return Ok(()),
                            _ => (),
                        }
                        target.flush()?;
                    }
                    "collision" => {
                        send(&mut target, json!({"id": id, "method": "roots/list"}))?;
                        let roots: Value = serde_json::from_str(&source.next().unwrap()?)?;
                        assert!(roots["result"].get("roots").is_some());
                        send(&mut target, json!({"id": id, "result": {"content": []}}))?;
                    }
                    _ => send(&mut target, json!({"id": id, "result": {"content": []}}))?,
                }
            }
            _ => (),
        }
    }
    if root.join("child.pid").exists() && generation == 1 {
        std::thread::sleep(STALLED_PEER_DELAY);
    }
    Ok(())
}
