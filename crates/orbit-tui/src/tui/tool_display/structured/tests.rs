use serde_json::json;

use super::*;

fn tool(name: &str, value: Value) -> ToolRow {
    ToolRow {
        id: "test".into(),
        name: name.into(),
        arguments: "{}".into(),
        status: ToolStatus::Finished,
        output: value.to_string(),
    }
}

fn text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(Line::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn directory_preview_preserves_nested_paths_and_separates_paging_from_expansion() {
    let mut row = tool(
        "list_files",
        json!({
            "entries": [{ "kind": "directory", "path": "src/中文" }, { "kind": "file", "path": "src/a/mod.rs" }, { "kind": "file", "path": "src/b/mod.rs" }, { "kind": "file", "path": "src/c.rs" }, { "kind": "file", "path": "src/d.rs" }, { "kind": "file", "path": "src/e.rs" }, { "kind": "file", "path": "src/f.rs" }],
            "has_more": true,
            "next_offset": 7
        }),
    );

    row.arguments = json!({ "path": "src" }).to_string();
    let original = row.output.clone();
    let preview = text(&files(&row, false, 80).unwrap());

    assert!(!preview.contains("本页"));
    assert!(preview.contains("中文/"));
    assert!(preview.contains("a/mod.rs") && preview.contains("b/mod.rs"));
    assert!(!preview.contains("src/"));
    assert!(preview.contains("另有 1 个条目") && preview.contains("[展开]"));

    let expanded = text(&files(&row, true, 80).unwrap());

    assert!(expanded.contains("f.rs") && expanded.contains("[收起]"));
    assert!(!expanded.contains("需再次查询"));
    assert_eq!(row.output, original);
}

#[test]
fn git_views_show_evidence_instead_of_json_and_do_not_invent_repairs() {
    let history = tool(
        "git_inspect",
        json!({
            "commits": [{
                "id": "a".repeat(40),
                "short_id": "aaaaaaa",
                "author": "作者",
                "authored_at": "2026-09-19T10:00:00+08:00",
                "subject": "修复列表"
            }]
        }),
    );
    let preview = text(&git(&history, false, 90).unwrap());

    assert!(preview.contains("aaaaaaa  修复列表") && preview.contains("[展开]"));
    assert!(!preview.contains("author"));

    let expanded = text(&git(&history, true, 90).unwrap());

    assert!(expanded.contains("作者  2026-09-19") && expanded.contains(&"a".repeat(40)));

    let malformed = tool(
        "git_inspect",
        json!({
            "commits": [{
                "id": "",
                "short_id": "author",
                "author": "2026-09-18",
                "authored_at": "bad",
                "subject": "shifted"
            }]
        }),
    );

    assert!(text(&git(&malformed, false, 90).unwrap()).contains("历史记录字段异常"));
    assert!(text(&git(&malformed, true, 90).unwrap()).contains("shifted"));

    let status = tool(
        "git_inspect",
        json!({
            "branch": {
                "head": "dev",
                "oid": "abc",
                "upstream": "origin/dev",
                "ahead": 1,
                "behind": 0
            },
            "entries": [{ "kind": "renamed_or_copied", "xy": "R.", "original_path": "旧.rs", "path": "新.rs" }, { "kind": "untracked", "path": "new.rs" }]
        }),
    );
    let status = text(&git(&status, false, 90).unwrap());

    assert!(status.contains("R.  旧.rs → 新.rs") && status.contains("??  new.rs"));

    let diff = tool(
        "git_inspect",
        json!({ "staged": true, "lines": ["--- a/file", "+++ b/file", "@@ -1 +1 @@", "-old", "+new"] }),
    );
    let lines = git(&diff, true, 80).unwrap();

    assert!(lines.iter().any(
        |l| l.to_string().ends_with("+new") && l.spans.last().unwrap().style.fg == Some(ADDED)
    ));
    assert!(
        lines.iter().any(|l| l.to_string().ends_with("-old")
            && l.spans.last().unwrap().style.fg == Some(REMOVED))
    );
}

#[test]
fn structured_results_fit_narrow_screens_escape_controls_and_keep_model_payload() {
    let long = format!("中文👩‍💻e\u{301}{}\u{1b}[31m", "很长的路径/".repeat(15));
    let rows = [
        tool(
            "list_files",
            json!({ "entries": [{ "path": long, "kind": "file" }] }),
        ),
        tool("git_inspect", json!({ "lines": [long], "staged": false })),
        tool(
            "read_command_output",
            json!({
                "output_id": "p-0",
                "stream": "stdout",
                "offset": 0,
                "next_offset": 100,
                "total_retained_bytes": 200,
                "truncated": true,
                "content": long
            }),
        ),
        tool(
            "web_search",
            json!({
                "query": "test",
                "provider": "google",
                "results": [{ "title": long, "url": "https://example.com", "snippet": long }],
                "results_returned": 1
            }),
        ),
        tool(
            "read_document",
            json!({
                "path": "doc.csv",
                "format": "csv",
                "content": long,
                "start_char": 0,
                "total_chars": 100,
                "truncated": false
            }),
        ),
        tool(
            "request_capability",
            json!({
                "status": "granted",
                "capability": { "path": long, "access": "read_write" }
            }),
        ),
        tool(
            "export_artifact",
            json!({ "path": long, "name": "f.txt", "bytes": 128, "sha256": "abc" }),
        ),
        tool(
            "git_export_patch",
            json!({ "path": long, "bytes": 64, "sha256": "def" }),
        ),
        tool("read_image", json!({ "path": long, "loaded": true })),
        tool(
            "extension_tool",
            json!({
                "nested": { "list": [long, "second"] }
            }),
        ),
    ];

    for row in rows {
        let original = row.output.clone();

        for width in [0, 1, 4, 5, 6, 12, 30, 80] {
            for expanded in [false, true] {
                let lines = super::super::lines(&row, expanded, width).unwrap();

                assert!(
                    lines.iter().all(|l| l.width() <= usize::from(width)),
                    "{} {width}",
                    row.name
                );
                assert!(!text(&lines).contains('\u{1b}'));
            }
        }

        assert_eq!(row.output, original);
    }
}

#[test]
fn expansion_wraps_with_a_stable_gutter_and_preserves_text() {
    let original = "中文目录/很长的文件名称.rs".repeat(8);
    let mut view = View::new(true, 30);
    view.push(&original, Color::Reset);
    let lines = view.finish();
    let joined = lines[..lines.len() - 1]
        .iter()
        .map(|l| l.to_string().strip_prefix("    ").unwrap().to_owned())
        .collect::<String>();

    assert_eq!(joined, original);
}

#[test]
fn command_output_view_formats_stream_range_and_truncation() {
    let row = tool(
        "read_command_output",
        json!({
            "output_id": "proc-0",
            "stream": "stdout",
            "offset": 0,
            "next_offset": 1024,
            "total_retained_bytes": 4096,
            "truncated": true,
            "content": "line 1\nline 2\n"
        }),
    );
    let rendered = text(&super::command_output(&row, false, 80).unwrap());
    assert!(rendered.contains("[STDOUT]"));
    assert!(rendered.contains("字节 0 B..1.0 KB"));
    assert!(rendered.contains("line 1"));
    assert!(rendered.contains("line 2"));
    assert!(rendered.contains("next_offset = 1024"));
}

#[test]
fn web_search_view_formats_results_and_links() {
    let row = tool(
        "web_search",
        json!({
            "query": "rust lang",
            "provider": "DuckDuckGo",
            "results": [
                {
                    "title": "Rust Programming Language",
                    "url": "https://www.rust-lang.org",
                    "snippet": "Empowering everyone to build reliable software."
                }
            ],
            "results_returned": 1,
            "requested_max_results": 5,
            "fewer_results": false
        }),
    );
    let rendered = text(&super::web_search(&row, false, 80).unwrap());
    assert!(rendered.contains(&format!("{} 搜索结果 · 来源: DuckDuckGo", icons::MAGNIFY)));
    assert!(rendered.contains("1. Rust Programming Language"));
    assert!(rendered.contains("https://www.rust-lang.org"));
    assert!(rendered.contains("Empowering everyone"));
}

#[test]
fn web_search_warns_before_suspect_results_and_expands_diagnostics() {
    let row = tool(
        "web_search",
        json!({
            "provider": "Bing",
            "query_match_unconfirmed": true,
            "warnings": ["Relevance is unconfirmed"],
            "response_url": "https://www.bing.com/search?q=Turing",
            "results": [{"title": "Alan Turing", "url": "https://example.org/"}]
        }),
    );
    let collapsed = text(&super::web_search(&row, false, 120).unwrap());
    assert!(collapsed.find("相关性未确认").unwrap() < collapsed.find("Alan Turing").unwrap());
    assert!(!collapsed.contains("搜索页面:"));
    let expanded = text(&super::web_search(&row, true, 120).unwrap());
    assert!(expanded.contains("Relevance is unconfirmed"));
    assert!(expanded.contains("https://www.bing.com/search?q=Turing"));
}

#[test]
fn document_view_formats_format_and_characters() {
    let row = tool(
        "read_document",
        json!({
            "path": "data.csv",
            "format": "csv",
            "content": "col1,col2\nval1,val2",
            "start_char": 0,
            "total_chars": 20,
            "truncated": false
        }),
    );
    let rendered = text(&super::document(&row, true, 80).unwrap());
    assert!(rendered.contains("[CSV] data.csv"));
    assert!(rendered.contains("col1,col2"));
    assert!(rendered.contains("val1,val2"));
}

#[test]
fn capability_view_formats_granted_and_denied() {
    let granted_row = tool(
        "request_capability",
        json!({
            "status": "granted",
            "capability": {
                "path": "/opt/cargo",
                "access": "read_write"
            }
        }),
    );
    let rendered = text(&super::capability(&granted_row, true, 80).unwrap());
    assert!(rendered.contains(&format!("{} 外部目录访问授权已授予", icons::CHECK)));
    assert!(rendered.contains("/opt/cargo"));
    assert!(rendered.contains("读写"));

    let denied_row = tool(
        "request_capability",
        json!({
            "status": "denied",
            "message": "User denied permission"
        }),
    );
    let rendered = text(&super::capability(&denied_row, true, 80).unwrap());
    assert!(rendered.contains(&format!("{} 外部目录访问授权被拒绝", icons::CLOSE)));
    assert!(rendered.contains("User denied permission"));
}

#[test]
fn artifact_and_patch_and_image_views_format_cleanly() {
    let artifact_row = tool(
        "export_artifact",
        json!({
            "path": "/artifacts/chart.png",
            "name": "chart.png",
            "bytes": 2048,
            "sha256": "abcdef1234567890abcdef"
        }),
    );
    let rendered = text(&super::export_artifact(&artifact_row, true, 80).unwrap());
    assert!(rendered.contains(&format!("{} 工件已成功导出", icons::PACKAGE_VARIANT_CLOSED)));
    assert!(rendered.contains("chart.png (2.0 KB)"));
    assert!(rendered.contains("sha256:abcdef1234567890…"));

    let patch_row = tool(
        "git_export_patch",
        json!({
            "path": "/artifacts/diff.patch",
            "bytes": 1024,
            "sha256": "123456"
        }),
    );
    let rendered = text(&super::git_export_patch(&patch_row, true, 80).unwrap());
    assert!(rendered.contains(&format!("{} Git 补丁已导出", icons::PACKAGE_VARIANT_CLOSED)));
    assert!(rendered.contains("1.0 KB"));

    let image_row = tool(
        "read_image",
        json!({
            "path": "img.png",
            "loaded": true
        }),
    );
    let rendered = text(&super::read_image(&image_row, true, 80).unwrap());
    assert!(rendered.contains(&format!(
        "{} 图像已载入上下文 · img.png",
        icons::IMAGE_OUTLINE
    )));
}
