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

#[test]
fn source_gutter_uses_verified_file_lines_without_changing_source_indentation() {
    let mut record = tool(
        "ReadFile",
        serde_json::json!({"path": "generated.rs"}),
        Some(
            serde_json::json!({"start_line": 97, "content": "97: 42: literal\n98:     indented\n"}),
        ),
    );
    let source = super::source_text(&record).unwrap();
    assert_eq!(source.first_line(), 97);
    assert_eq!(source.text(), "42: literal\n    indented\n");
    for content in [
        "97: first\n99: skipped",
        "98: mismatched",
        "unprefixed text",
    ] {
        record.output = Some(serde_json::json!({"start_line": 97, "content": content}).to_string());
        assert!(super::source_text(&record).is_none());
        assert_eq!(super::content(&record, Stream::Result), content);
    }
}

#[test]
fn state_and_command_views_keep_user_content_and_drop_transport_metadata() {
    for (name, output, expected) in [
        (
            "ListTasks",
            serde_json::json!({
                "tasks": [{
                    "id": 8,
                    "title": "Review generated module",
                    "details": "Check cancellation",
                    "status": "in_progress",
                    "project_id": "private-project"
                }],
                "source_kind": "project_task_state"
            }),
            "Review generated module",
        ),
        (
            "ListMemories",
            serde_json::json!({
                "memories": [{
                    "id": 3,
                    "title": "Style",
                    "content": "Use descriptive names",
                    "category": "preference",
                    "active": true,
                    "session_id": "private-session"
                }]
            }),
            "Use descriptive names",
        ),
        (
            "StopCommands",
            serde_json::json!({
                "jobs": [{
                    "command": "generated-worker",
                    "status": "cancelling",
                    "elapsed_ms": 9,
                    "call_id": "private-call"
                }]
            }),
            "正在停止",
        ),
        (
            "WaitCommands",
            serde_json::json!({
                "jobs": [{
                    "command": "generated-worker",
                    "status": "failed",
                    "elapsed_ms": 11,
                    "result": {
                        "error": { "message": "Generated worker failed", "code": "execution_failed" }
                    }
                }]
            }),
            "Generated worker failed",
        ),
        (
            "CheckEvidence",
            serde_json::json!({
                "commands": [],
                "changes": [],
                "command_count": 0,
                "snapshot_id": "private-hash",
                "baseline_available": true
            }),
            "0 项文件变更",
        ),
        (
            "CheckEvidence",
            serde_json::json!({
                "commands": [{"command": "generated-check", "input_state": "unknown", "exit_code": 0}],
                "changes": [{"path": "generated.rs", "change_kind": null, "origin": "unknown"}],
                "command_count": 1,
                "workspace_scan": {"complete": false, "error": "generated scan budget exhausted"}
            }),
            "当前文件未完成核对",
        ),
    ] {
        let view = content(
            &tool(name, serde_json::json!({}), Some(output)),
            Stream::Result,
        );
        assert!(view.contains(expected), "{view}");
        if name == "CheckEvidence" && expected == "当前文件未完成核对" {
            assert!(view.contains("generated.rs · 当前状态未确认"), "{view}");
        }
        assert!(
            !view.contains("private-") && !view.contains("source_kind"),
            "{view}"
        );
    }
}

#[test]
fn failure_receipts_keep_diagnostics_without_exposing_transport_metadata() {
    for (code, title) in [
        ("invalid_arguments", "调用参数无效"),
        ("authorization_required", "需要访问授权"),
        ("dependency_unavailable", "所需依赖不可用"),
        ("conflict", "输入已发生变化"),
        ("cancelled", "工具调用已取消"),
        ("future_code", "工具执行失败"),
    ] {
        let mut record = tool(
            "ReadFile",
            serde_json::json!({"path": "generated.rs"}),
            Some(serde_json::json!({
                "error": {
                    "code": code,
                    "message": "generated diagnostic\nsecond line",
                    "retryable": false,
                    "recovery": "opaque_machine_hint",
                    "message_truncated": false
                }
            })),
        );
        record.status = ToolStatus::Failed;
        let failure = super::failure(&record).unwrap();
        assert_eq!(failure.title(), title);
        assert_eq!(failure.message(), "generated diagnostic\nsecond line");
        let displayed = content(&record, Stream::Result);
        assert_eq!(displayed, "generated diagnostic\nsecond line");
        assert!(!displayed.contains("opaque_machine_hint") && !displayed.contains("retryable"));
        record.status = ToolStatus::Complete;
        assert!(super::failure(&record).is_none());
    }
    let mut record = tool("ReadFile", serde_json::json!({}), None);
    record.status = ToolStatus::Failed;
    record.output = Some("dependency unavailable is only diagnostic text here".into());
    assert_eq!(super::failure(&record).unwrap().title(), "工具执行失败");
    assert!(content(&record, Stream::Result).contains(record.output.as_deref().unwrap()));
}

#[test]
fn command_failure_presentation_preserves_streams_and_distinguishes_cancellation() {
    let mut record = tool(
        "Exec",
        serde_json::json!({"command": "generated-check"}),
        Some(serde_json::json!({
            "exit_code": 7,
            "elapsed": {"secs": 2, "nanos": 0},
            "stdout": "partial output",
            "stderr": "generated failure",
            "timed_out": false,
            "cancelled": false,
            "output_truncated": false,
            "stdout_total_bytes": 14,
            "stderr_total_bytes": 17
        })),
    );
    assert_eq!(super::failure(&record).unwrap().title(), "命令执行失败");
    assert_eq!(content(&record, Stream::Stderr), "generated failure");
    let mut value: serde_json::Value =
        serde_json::from_str(record.output.as_deref().unwrap()).unwrap();
    value["cancelled"] = true.into();
    value["timed_out"] = true.into();
    record.output = Some(value.to_string());
    assert_eq!(super::failure(&record).unwrap().title(), "命令已取消");
    assert_eq!(content(&record, Stream::Stdout), "partial output");
    value["cancelled"] = false.into();
    record.output = Some(value.to_string());
    assert_eq!(super::failure(&record).unwrap().title(), "命令执行超时");
    value["timed_out"] = false.into();
    value["exit_code"] = 0.into();
    record.output = Some(value.to_string());
    assert!(
        super::failure(&record).is_none(),
        "stderr alone is not a failure"
    );
    value["exit_code"] = serde_json::Value::Null;
    value["signal"] = 15.into();
    value["core_dumped"] = true.into();
    record.output = Some(value.to_string());
    let failure = super::failure(&record).unwrap();
    assert_eq!(failure.title(), "命令被信号终止");
    assert!(failure.message().contains("15") && failure.message().contains("核心转储"));
}
