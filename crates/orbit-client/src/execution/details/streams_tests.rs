use super::{Stream, ToolRun, ToolStatus, content, note, status};

fn tool(name: &str, arguments: serde_json::Value, output: Option<serde_json::Value>) -> ToolRun {
    ToolRun {
        id: "test-call".into(),
        name: name.into(),
        purpose: None,
        changes: Vec::new(),
        detail: String::new(),
        status: ToolStatus::Complete,
        output: output.map(|output| output.to_string()),
        arguments: arguments.to_string(),
        generation: 1,
        stdout_preview: None,
        stderr_preview: None,
        command_progress: None,
        elapsed_ms: None,
        live: false,
        run_finished: false,
    }
}

#[test]
fn command_streams_use_actual_result_and_preserve_literal_markdown_and_unicode() {
    let tool = tool(
        "Exec",
        serde_json::json!({
            "command": "example"
        }),
        Some(serde_json::json!({
            "exit_code": 3,
            "elapsed": {
                "secs": 0,
                "nanos": 9_000_000
            },
            "stdout": "# literal\n````\n甲乙\r\n",
            "stderr": "actual error\n",
            "timed_out": false,
            "cancelled": false,
            "output_truncated": true,
            "stdout_total_bytes": 40_000,
            "stderr_total_bytes": 13,
            "retained_output_truncated": true,
            "retained_output_error": "disk unavailable"
        })),
    );
    assert_eq!(content(&tool, Stream::Stdout), "# literal\n````\n甲乙\r\n");
    assert_eq!(content(&tool, Stream::Stderr), "actual error\n");
    assert_eq!(status(&tool), "退出码 3 · 9 ms");
    let note = note(&tool, Stream::Stdout);
    assert!(
        note.contains("32KiB") && note.contains("保留不完整") && note.contains("disk unavailable")
    );
}

#[test]
fn command_view_preserves_the_complete_multiline_request() {
    let command = "python3 - <<'PY'\nprint('甲乙 `code`')\nPY\n";
    let tool = tool(
        "Exec",
        serde_json::json!({
            "command": command,
            "cwd": "/project/with spaces"
        }),
        None,
    );
    assert_eq!(content(&tool, Stream::Command), command);
    assert!(content(&tool, Stream::Stdout).is_empty());
}

#[test]
fn received_output_page_is_a_snapshot_and_more_pages_does_not_mean_retention_loss() {
    let tool = tool(
        "CommandOutput",
        serde_json::json!({
            "output_id": "cmd-1"
        }),
        Some(serde_json::json!({
            "output_id": "cmd-1",
            "stream": "stderr",
            "offset": 12,
            "next_offset": 18,
            "total_retained_bytes": 90,
            "truncated": true,
            "content": "甲乙"
        })),
    );
    assert_eq!(content(&tool, Stream::Result), "甲乙");
    let note = note(&tool, Stream::Result);
    assert!(note.contains("字节") && note.contains("12–18") && note.contains("可续读"));
    assert!(!note.contains("丢失") && !note.contains("实时"));
}

#[test]
fn background_receipts_never_become_realtime_even_if_they_contain_preview_like_fields() {
    let mut tool = tool(
        "Exec",
        serde_json::json!({
            "command": "background",
            "background": true
        }),
        Some(serde_json::json!({
            "job_id": "job-1",
            "output_id": "cmd-1",
            "background": true,
            "running": true,
            "status": "running"
        })),
    );
    tool.status = ToolStatus::Background;
    assert!(tool.command_result().is_none());
    assert!(content(&tool, Stream::Stdout).is_empty());
    assert!(content(&tool, Stream::Result).contains("job-1"));
    assert!(note(&tool, Stream::Result).contains("不提供后台实时日志"));
    tool.run_finished = true;
    assert_eq!(status(&tool), "结果未确认");
    assert!(note(&tool, Stream::Result).contains("未收到后台命令的完成结果"));
}

#[test]
fn malformed_or_legacy_outputs_remain_inspectable_without_markdown_wrapping() {
    let mut tool = tool(
        "ReadFile",
        serde_json::json!({
            "path": "source.rs"
        }),
        None,
    );
    tool.output = Some("# not a heading\n````text\nraw\n````".into());
    assert_eq!(
        content(&tool, Stream::Result),
        tool.output.as_deref().unwrap()
    );
    assert!(content(&tool, Stream::Arguments).contains("source.rs"));
    tool.name = "Exec".into();
    tool.status = ToolStatus::Failed;
    assert!(tool.command_result().is_none());
    assert_eq!(status(&tool), "失败");
    assert_eq!(content(&tool, Stream::Result), tool.output.unwrap());
}

#[test]
fn headers_preserve_all_arguments_without_duplicate_result_pages() {
    let command = "printf 'first\\n'\nprintf 'second\\n'";
    let mut record = tool(
        "Exec",
        serde_json::json!({
            "command": command, "cwd": "src", "timeout_seconds": 10,
            "environment": {"MODE": "test"}
        }),
        Some(serde_json::json!({
            "exit_code": 0, "elapsed": {"secs": 0, "nanos": 0},
            "stdout": "actual output", "stderr": "", "timed_out": false,
            "stdout_total_bytes": 13, "stderr_total_bytes": 0,
            "cancelled": false, "output_truncated": false
        })),
    );
    record.purpose = Some("Inspect generated output".into());
    let parameters = super::parameters(&record, "/generated/project");
    assert_eq!(parameters.matches(command).count(), 1);
    assert!(parameters.contains("cwd: /generated/project/src"));
    assert!(parameters.contains("timeout_seconds: 10") && parameters.contains("MODE"));
    assert_eq!(super::streams(&record), &[Stream::Stdout]);
    assert_eq!(content(&record, Stream::Stdout), "actual output");
    record.name = "mcp_fixture".into();
    record.arguments = serde_json::json!({"_orbit_purpose": 4}).to_string();
    assert!(super::parameters(&record, "").contains("_orbit_purpose: 4"));
}

#[test]
fn file_pages_contain_only_the_received_text_and_completed_status_is_empty() {
    let record = tool(
        "ReadFile",
        serde_json::json!({"path": "fixture.rs"}),
        Some(serde_json::json!({
            "path": "fixture.rs", "content": "1: fn generated() {}", "has_more": true,
            "next_start_line": 2
        })),
    );
    assert_eq!(content(&record, Stream::Result), "1: fn generated() {}");
    assert!(status(&record).is_empty());
    assert_eq!(super::streams(&record), &[Stream::Result]);
}
