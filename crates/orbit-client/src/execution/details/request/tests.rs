use super::*;
use crate::protocol::model::ToolCall;
use serde_json::json;

fn request(name: &str, args: Value) -> Request {
    Request::new(
        &ToolRun::new(&ToolCall::function("sample", name, args)),
        "/project",
    )
}

#[test]
fn command_copy_preserves_the_script_without_field_names_or_execution_policy() {
    let script = "printf '%s\\n' 'sample'\nprintf 'tail'\n";
    let view = request(
        "Exec",
        json!({
            "command": script,
            "cwd": "src",
            "network": "deny",
            "workspace_access": "read_only",
            "timeout_seconds": 17
        }),
    );
    assert_eq!(view.content(), script);
    assert_eq!(view.context(), "目录：/project/src");
    assert_eq!(view.language(), "bash");
    assert_eq!(view.layout(), RequestLayout::Body);
}

#[test]
fn operation_content_keeps_queries_paths_and_file_bodies_distinct() {
    for (name, args, expected) in [
        (
            "ReadFile",
            json!({"path": "src/alpha.rs", "start_line": 7}),
            "src/alpha.rs",
        ),
        (
            "WriteFile",
            json!({"path": "alpha.txt", "content": "# literal\n"}),
            "# literal\n",
        ),
        (
            "Grep",
            json!({"query": "some_pattern", "path": "src", "regex": true}),
            "some_pattern",
        ),
        (
            "WebSearch",
            json!({"query": "public reference", "region": "US"}),
            "public reference",
        ),
        (
            "WebFetch",
            json!({"url": "https://example.com/a"}),
            "https://example.com/a",
        ),
        ("ListFiles", json!({}), "."),
        ("SubmitPlan", json!({"generation": 3}), "提交计划供审阅"),
        ("CancelPlan", json!({"generation": 4}), "放弃自主计划草稿"),
        (
            "SpawnAgent",
            json!({"task": "Inspect generated sources", "role": "review"}),
            "Inspect generated sources",
        ),
    ] {
        assert_eq!(request(name, args).content(), expected, "{name}");
    }
    let search = request(
        "Grep",
        json!({"query": "needle", "glob": "**/*.rs", "regex": true}),
    );
    assert!(search.context().contains("**/*.rs") && search.context().contains("正则：true"));
    let document = request(
        "ReadDocument",
        json!({"path": "sample.xlsx", "sheet": "North", "start_row": 5, "max_rows": 10}),
    );
    assert_eq!(document.content(), "sample.xlsx");
    assert_eq!(document.context(), "工作表：North · 起始行：5 · 行数：10");
    assert_eq!(document.layout(), RequestLayout::Summary);

    let mut edited = ToolRun::new(&ToolCall::function(
        "write",
        "WriteFile",
        json!({
            "path": "generated.rs", "content": "requested text"
        }),
    ));
    edited
        .changes
        .push(crate::execution::FileChange {
            path: "generated.rs".into(),
            before: None,
            after: "confirmed text\n".into(),
        });
    assert_eq!(Request::new(&edited, "").layout(), RequestLayout::Hidden);
}

#[test]
fn edit_preview_preserves_order_and_reports_replacement_scope() {
    let view = request(
        "EditFile",
        json!({
            "path": "alpha.txt",
            "edits": [
                {
                    "old_text": "first\n",
                    "new_text": "second\n",
                    "replace_all": true
                },
                {
                    "old_text": "second\n",
                    "new_text": "third\n"
                }
            ]
        }),
    );
    assert!(
        view.content()
            .contains("-first\n+second\n")
    );
    assert!(
        view.content()
            .contains("-second\n+third\n")
    );
    assert!(view.content().find("-first") < view.content().find("-second"));
    assert!(view.context().contains("全部匹配项") && view.context().contains("2 项有序替换"));
}

#[test]
fn dynamic_tools_and_malformed_calls_keep_their_original_identity_and_data() {
    let args = json!({"nested": {"_orbit_purpose": 5}, "query": "literal"});
    let tool = ToolRun::new(&ToolCall::function(
        "opaque",
        "mcp_service_search",
        args.clone(),
    ));
    let view = Request::new(&tool, "/project");
    assert_eq!(serde_json::from_str::<Value>(view.content()).unwrap(), args);
    assert_eq!(tool.name, "mcp_service_search");
    let mut malformed = tool;
    malformed.name = "Exec".into();
    malformed.arguments = "unfinished {".into();
    assert_eq!(Request::new(&malformed, "").content(), "unfinished {");
}
