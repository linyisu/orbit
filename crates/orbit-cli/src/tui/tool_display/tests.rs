use serde_json::json;

use super::*;

#[test]
fn overview_preview_has_readable_evidence_and_expand_controls() {
    let value = json!({
        "inventory": {
            "files": 3,
            "languages": { "Rust": 2 },
            "directories": [{ "path": "src", "files": 2 }]
        },
        "manifests": [{ "path": "Cargo.toml", "package": "example", "parsed": true, "verification_candidates": [{ "argv": ["cargo", "test"] }] }],
        "test_files": ["tests/check.rs"],
        "instruction_files": [],
        "symbols": [{ "path": "src/lib.rs", "line": 3, "name": "parse" }],
        "truncated": { "scan": false, "content": false, "output": false }
    });
    let tool = row("project_overview", value);

    for width in [1, 20, 80] {
        for expanded in [false, true] {
            assert!(
                lines(&tool, expanded, width)
                    .unwrap()
                    .iter()
                    .all(|line| line.width() <= usize::from(width))
            );
        }
    }

    let expanded = lines(&tool, true, 80)
        .unwrap()
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(expanded.contains("待验证入口"));
    assert!(expanded.contains("src/lib.rs:3  parse"));
    assert!(expanded.contains("[收起]"));
    assert!(
        lines(&tool, false, 80)
            .unwrap()
            .last()
            .unwrap()
            .to_string()
            .contains("[展开]")
    );
}

fn row(name: &str, output: Value) -> ToolRow {
    ToolRow {
        id: "display-test".into(),
        name: name.into(),
        arguments: "{}".into(),
        status: ToolStatus::Finished,
        output: output.to_string(),
    }
}

#[test]
fn agent_read_summaries_show_task_and_content_instead_of_internal_cursors() {
    let args = json!({"agent_id":"orbit-agent","message_id":5601,"offset":4000}).to_string();
    let mut tool = row(
        "read_agent",
        json!({
            "agent":{"id":"orbit-agent","task":"审查 Orbit 渲染逻辑"},
            "view":"result", "message_id":5601, "offset":4000, "next_offset":8000,
            "content":"\n## 修复中文字符边界\n第二行不放进摘要",
            "result_available":true
        }),
    );

    tool.arguments = args.clone();
    let original = tool.output.clone();
    assert_eq!(group_target_summary(&tool), "审查 Orbit 渲染逻辑");
    assert_eq!(group_call_summary(&tool), "最终结果 · ## 修复中文字符边界");
    let summary = tool_summary(&tool.name, &tool.arguments, &tool.output);
    assert_eq!(
        summary,
        "审查 Orbit 渲染逻辑 · 最终结果 · ## 修复中文字符边界"
    );
    assert!(!summary.contains("orbit-agent"));
    assert!(!summary.contains("5601"));
    assert!(!summary.contains("4000"));
    assert!(!summary.contains("第二行"));
    assert_eq!(tool.output, original);

    for (content, expected) in [
            (json!({"role":"user","content":"检查 Orbit 工具显示"}).to_string(), "任务说明 · 检查 Orbit 工具显示"),
            (json!({"role":"assistant","content":null,"tool_calls":[{"metadata":{"signature":"orbit-signature"},"function":{"name":"read_file","arguments":json!({"path":"src/orbit.rs"}).to_string()}}]}).to_string(), "工具调用 · read_file src/orbit.rs"),
            (json!({"role":"tool","content":json!({"content":"1: fn orbit() {}\n2: // Orbit"}).to_string()}).to_string(), "工具结果 · 1: fn orbit() {}"),
            (json!({"role":"assistant","content":"Orbit 审查报告\n后续内容"}).to_string(), "回答 · Orbit 审查报告"),
            (String::new(), "没有后续记录"),
        ] {
            tool.output = json!({"agent":{"task":"审查 Orbit 渲染逻辑"},"content":content,"total_characters":content.chars().count()}).to_string();
            assert_eq!(group_call_summary(&tool), expected);
        }

    tool.output = json!({"content":"{\"role\":\"assistant\",\"tool_calls\":[", "message_id":5601})
        .to_string();
    let summary = group_call_summary(&tool);
    assert!(summary.starts_with("执行记录片段 · 读取了"));
    assert!(!summary.contains("tool_calls"));
    tool.output = json!({"view":"result","result_available":false,"status":"running"}).to_string();
    assert_eq!(group_call_summary(&tool), "结果尚未生成");
    tool.output.clear();
    assert_eq!(group_call_summary(&tool), "读取中");
}

#[test]
fn memory_save_shows_title_and_scoped_preview_without_changing_the_result() {
    let content = format!(
        "审查结果：{}\n第二行\n第三行\n第四行",
        "中文内容".repeat(50)
    );
    let mut tool = row(
        "memory_save",
        json!({
            "memory": {
                "id": 12,
                "active": true,
                "category": "fact",
                "title": "审查记录",
                "content": content
            },
            "source_kind": "session_memory"
        }),
    );

    tool.arguments =
        json!({ "title": "审查记录", "category": "fact", "content": content }).to_string();
    let original = tool.output.clone();

    assert_eq!(memory_summary(&tool.arguments), "审查记录");

    let preview = lines(&tool, false, 60).unwrap();

    assert!(
        preview[0]
            .to_string()
            .contains("已保存 · 事实 · 当前会话 · #12")
    );
    assert!(preview.last().unwrap().to_string().contains("[展开]"));

    for width in [0, 1, 5, 30, 80] {
        for expanded in [false, true] {
            assert!(
                lines(&tool, expanded, width)
                    .unwrap()
                    .iter()
                    .all(|line| line.width() <= width as usize)
            );
        }
    }

    let expanded = lines(&tool, true, 60).unwrap();
    let actual: String = expanded[1..expanded.len() - 1]
        .iter()
        .map(|line| line.to_string().strip_prefix("    ").unwrap().to_owned())
        .collect();

    assert_eq!(actual, content.lines().collect::<String>());
    assert_eq!(tool.output, original);
}

fn highlighted(lines: &[Line<'static>]) -> String {
    lines
        .iter()
        .flat_map(|line| &line.spans)
        .filter(|span| span.style == super::super::markdown::mark_style(Style::default()))
        .map(|span| span.content.as_ref())
        .collect()
}

#[test]
fn search_title_includes_query_and_scope_even_before_results_arrive() {
    let mut tool = row("search_text", json!({ "matches": [] }));
    tool.arguments =
        json!({ "query": "workspace snapshot", "path": "crates/orbit-tools/src" }).to_string();
    tool.status = ToolStatus::Running;
    let view = super::super::render::transcript_lines_at_width(
        &[super::super::app::TranscriptItem::Tool(tool)],
        false,
        120,
    );

    assert!(
        view[0]
            .to_string()
            .contains("search_text \"workspace snapshot\"(crates/orbit-tools/src)")
    );
    assert_eq!(search_summary(r#"{"query":"foo"}"#), "\"foo\"(.)");
}

#[test]
fn highlighting_follows_literal_regex_case_and_unicode_search_rules() {
    for (config, text, expected) in [
        (json!({ "query": "a.b" }), "a.b A.B axb", "a.bA.B"),
        (
            json!({ "query": "a.b", "case_sensitive": true }),
            "a.b A.B axb",
            "a.b",
        ),
        (
            json!({ "query": "a.b", "regex": true }),
            "a.b A.B axb",
            "a.bA.Baxb",
        ),
        (
            json!({ "query": r"中文\s+value", "regex": true }),
            "前缀：中文\tVALUE 后缀",
            "中文    VALUE",
        ),
    ] {
        let pattern = matcher(&config).unwrap();
        let mut view: Vec<Line<'static>> = vec![];
        source(
            &mut view,
            1,
            4,
            text,
            Some(&pattern),
            &mut Renderer::new(""),
            true,
            80,
        );

        assert_eq!(highlighted(&view), expected);
    }

    assert!(matcher(&json!({ "query": "[", "regex": true })).is_none());
}

#[test]
fn highlight_survives_wrapping_and_focuses_long_line_previews() {
    let pattern = matcher(&json!({ "query": "^ABCDEFGH$", "regex": true })).unwrap();
    let mut view: Vec<Line<'static>> = vec![];
    source(
        &mut view,
        123,
        4,
        "ABCDEFGH",
        Some(&pattern),
        &mut Renderer::new(""),
        true,
        15,
    );

    assert_eq!(view.len(), 2);
    assert_eq!(highlighted(&view), "ABCDEFGH");
    assert!(view.iter().all(|line| line.width() <= 15));

    let pattern = matcher(&json!({ "query": "TARGET" })).unwrap();
    let text = format!("{} TARGET {}", "中文".repeat(50), "suffix".repeat(10));

    for width in [11, 12, 32, 80] {
        let mut view: Vec<Line<'static>> = vec![];
        source(
            &mut view,
            1,
            4,
            &text,
            Some(&pattern),
            &mut Renderer::new(""),
            false,
            width,
        );

        assert!(view.iter().all(|line| line.width() <= usize::from(width)));

        if width >= 32 {
            assert_eq!(highlighted(&view), "TARGET");
        }
    }
}

#[test]
fn search_groups_files_and_merges_overlapping_context_without_mutating_results() {
    let row = row(
        "search_text",
        json!({
            "matches": [{
                "path": "src/main.rs",
                "line": 12,
                "column": 1,
                "text": "let first = 1;",
                "context": [{ "line": 11, "text": "fn main() {" }, { "line": 13, "text": "let second = 2;" }]
            }, {
                "path": "src/main.rs",
                "line": 13,
                "column": 1,
                "text": "let second = 2;",
                "context": [{ "line": 12, "text": "let first = 1;" }]
            }, {
                "path": "src/main.rs",
                "line": 13,
                "column": 5,
                "text": "let second = 2;",
                "context": []
            }],
            "has_more": true,
            "scan_truncated": true
        }),
    );
    let original = row.output.clone();
    let view = lines(&row, true, 80).unwrap();
    let text = view
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.contains("3 处匹配 · 1 个文件"));
    assert_eq!(text.matches("src/main.rs").count(), 1);
    assert_eq!(text.matches("let first = 1;").count(), 1);
    assert_eq!(text.matches("let second = 2;").count(), 1);
    assert!(text.find("11 │").unwrap() < text.find("12 │").unwrap());
    assert!(text.contains("当前结果未全部返回"));
    assert!(text.contains("部分文件尚未搜索"));
    assert!(!text.contains("\"matches\""));
    assert_eq!(row.output, original);
}

#[test]
fn search_retains_syntax_state_in_context_and_resets_it_across_gaps() {
    let row = row(
        "search_text",
        json!({
            "query": "orbit",
            "matches": [
                { "path": "src/orbit.rs", "line": 2, "text": "fn orbit_in_comment() {}",
                  "context": [{ "line": 1, "text": "/*" }] },
                { "path": "src/orbit.rs", "line": 20, "text": "fn orbit() {}" }
            ]
        }),
    );
    let view = lines(&row, true, 80).unwrap();
    for (number, expected) in [
        (2, Color::Rgb(163, 173, 190)),
        (20, Color::Rgb(198, 120, 221)),
    ] {
        let line = view
            .iter()
            .find(|line| line.to_string().contains(&format!("{number} │")))
            .unwrap();
        let keyword = line
            .spans
            .iter()
            .find(|span| span.content.starts_with("fn"))
            .unwrap();

        assert_eq!(keyword.style.fg, Some(expected));
    }
}

#[test]
fn file_preview_clips_by_cells_and_expansion_keeps_source_and_gutter() {
    let source = "    中文👩‍💻e\u{301}\tlet value = 123456789;".repeat(3);
    let row = row(
        "read_file",
        json!({
            "content": format!("744: {source}\n745: end\n"),
            "start_line": 744,
            "end_line": 745,
            "total_lines": 745,
            "truncated": false
        }),
    );

    for width in [0, 1, 9, 12, 32, 80] {
        for expanded in [false, true] {
            let view = lines(&row, expanded, width).unwrap();

            assert!(view.iter().all(|line| line.width() <= usize::from(width)));
        }
    }

    let collapsed = lines(&row, false, 32).unwrap();

    assert_eq!(collapsed.len(), 3);
    assert!(collapsed[0].to_string().ends_with('…'));

    let expanded = lines(&row, true, 32).unwrap();
    let reconstructed = expanded[..expanded.len() - 2]
        .iter()
        .map(|line| {
            let text = line.to_string();

            assert!(text.starts_with("     744 │ ") || text.starts_with("         │ "));

            text.split_once("│ ").unwrap().1.to_owned()
        })
        .collect::<String>();

    assert_eq!(reconstructed, safe_output(&source));

    for end in [4, 92, 168, 1024] {
        let file = self::row(
            "read_file",
            json!({
                "content": "1: first\n2: second\n",
                "start_line": 1,
                "end_line": end,
                "total_lines": end,
                "truncated": false
            }),
        );
        let view = lines(&file, false, 80).unwrap();

        assert_eq!(view[0].to_string(), "       1 │ first");
        assert_eq!(view[1].to_string(), "       2 │ second");
    }
}

#[test]
fn empty_search_and_invalid_or_failed_results_stay_distinct() {
    let empty = row("search_text", json!({ "matches": [] }));

    assert!(
        lines(&empty, false, 80).unwrap()[0]
            .to_string()
            .contains("0 处匹配")
    );

    let mut invalid = row("read_file", json!({ "error": "cannot read" }));

    assert!(lines(&invalid, false, 80).is_none());

    invalid.name = "search_text".into();
    invalid.output = "tool execution failed: denied".into();
    invalid.status = ToolStatus::Failed;

    assert!(lines(&invalid, false, 80).is_none());
}

#[test]
fn write_file_preview_shows_line_numbers() {
    let args = json!({
        "path": "hello.py",
        "content": "print(\"Hello, world!\")\nprint(42)"
    })
    .to_string();
    let preview = write_file_preview(&args, 80).expect("preview should succeed");
    assert_eq!(preview.len(), 2);
    assert_eq!(
        preview[0].to_string(),
        "       1 │ print(\"Hello, world!\")"
    );
    assert_eq!(preview[1].to_string(), "       2 │ print(42)");
}

#[test]
fn edit_file_preview_shows_diff_lines() {
    let args = json!({
        "path": "test.txt",
        "old_text": "old line",
        "new_text": "new line"
    })
    .to_string();
    let preview = edit_file_preview(&args, 80).expect("preview should succeed");
    assert_eq!(preview.len(), 2);
    assert_eq!(preview[0].to_string(), "    -   1 │ old line");
    assert_eq!(preview[1].to_string(), "    +   1 │ new line");
    assert_eq!(
        preview[0].spans[0].style.bg,
        Some(crate::tui::theme::THEME.diff_removed_bg)
    );
    assert_eq!(
        preview[1].spans[0].style.bg,
        Some(crate::tui::theme::THEME.diff_added_bg)
    );

    let py_args = json!({
        "path": "demo.py",
        "old_text": "x = 1",
        "new_text": "def foo():\n    return 42"
    })
    .to_string();
    let py_preview = edit_file_preview(&py_args, 80).expect("py preview should succeed");
    assert_eq!(py_preview.len(), 3);
    assert_eq!(py_preview[0].to_string(), "    -   1 │ x = 1");
    assert_eq!(py_preview[1].to_string(), "    +   1 │ def foo():");
    assert_eq!(py_preview[2].to_string(), "    +   2 │     return 42");
    for span in &py_preview[0].spans {
        assert_eq!(
            span.style.bg,
            Some(crate::tui::theme::THEME.diff_removed_bg)
        );
    }

    for span in &py_preview[1].spans {
        assert_eq!(span.style.bg, Some(crate::tui::theme::THEME.diff_added_bg));
    }
    for span in &py_preview[2].spans {
        assert_eq!(span.style.bg, Some(crate::tui::theme::THEME.diff_added_bg));
    }
}

#[test]
fn command_output_summary_displays_terminal_number_and_id() {
    let args1 = json!({
        "output_id": "cmd-12345-67890-0",
        "stream": "stdout"
    })
    .to_string();

    assert_eq!(
        command_output_summary(&args1),
        "终端 #1 (cmd-12345-67890-0)"
    );

    let args2 = json!({
        "output_id": "cmd-12345-67890-1",
        "stream": "stderr"
    })
    .to_string();

    assert_eq!(
        command_output_summary(&args2),
        "终端 #2 [stderr] (cmd-12345-67890-1)"
    );

    let args3 = json!({
        "output_id": "cmd-custom"
    })
    .to_string();
    assert_eq!(command_output_summary(&args3), "终端 cmd-custom");
}
