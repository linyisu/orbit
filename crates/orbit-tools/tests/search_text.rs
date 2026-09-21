use std::fs;

use orbit_agent::tool::{Tool, ToolError};
use orbit_model::types::ToolCall;
use orbit_tools::{search_text::SearchText, workspace::Workspace};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

async fn run(tool: &SearchText, arguments: Value) -> Value {
    let output = tool
        .execute(
            ToolCall::function("search", "search_text", arguments),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    serde_json::from_str(&output).unwrap()
}

#[tokio::test]
async fn searches_literal_and_regex_with_context_globs_and_pagination() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("src")).unwrap();
    fs::create_dir(directory.path().join("docs")).unwrap();
    fs::write(
        directory.path().join("src/lib.rs"),
        "first line\nNeedle here\nneedle again\nlast line\n",
    )
    .unwrap();
    fs::write(directory.path().join("docs/note.md"), "needle in docs\n").unwrap();

    let tool = SearchText::new(Workspace::new(directory.path()).unwrap());

    let first = run(
        &tool,
        json!({ "query": "needle", "path": "src", "glob": "**/*.rs", "context_lines": 1, "limit": 1 }),
    )
    .await;
    assert_eq!(first["matches"][0]["path"], "src/lib.rs");
    assert_eq!(first["matches"][0]["line"], 2);
    assert_eq!(first["matches"][0]["column"], 1);
    assert_eq!(
        first["matches"][0]["context"][0],
        json!({ "line": 1, "text": "first line" })
    );
    assert_eq!(first["has_more"], true);
    assert_eq!(first["next_offset"], 1);

    let second = run(
        &tool,
        json!({ "query": "needle", "path": "src", "glob": "**/*.rs", "offset": 1 }),
    )
    .await;
    assert_eq!(second["matches"].as_array().unwrap().len(), 1);
    assert_eq!(second["matches"][0]["line"], 3);
    assert_eq!(second["has_more"], false);

    let regex = run(
        &tool,
        json!({ "query": "Needle\\s+here", "regex": true, "case_sensitive": true, "glob": "**/*.rs" }),
    )
    .await;
    assert_eq!(regex["matches"].as_array().unwrap().len(), 1);
    assert_eq!(regex["matches"][0]["line"], 2);

    let case_sensitive = run(&tool, json!({ "query": "needle", "case_sensitive": true })).await;
    assert_eq!(case_sensitive["matches"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn respects_ignore_rules_and_workspace_protection() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();

    fs::create_dir(directory.path().join(".git")).unwrap();
    fs::create_dir(directory.path().join("ignored-dir")).unwrap();

    fs::write(directory.path().join(".git/config"), "needle secret").unwrap();
    fs::write(directory.path().join(".env"), "needle secret").unwrap();
    fs::write(directory.path().join("ignored.txt"), "needle ignored").unwrap();
    fs::write(directory.path().join("ignored-dir/file"), "needle ignored").unwrap();
    fs::write(directory.path().join("visible.txt"), "needle visible").unwrap();
    fs::write(
        directory.path().join(".gitignore"),
        "ignored.txt\nignored-dir/\n",
    )
    .unwrap();

    fs::write(outside.path().join("secret"), "needle outside").unwrap();
    symlink(
        outside.path().join("secret"),
        directory.path().join("linked"),
    )
    .unwrap();

    let tool = SearchText::new(Workspace::new(directory.path()).unwrap());

    let result = run(&tool, json!({ "query": "needle", "include_hidden": true })).await;
    assert_eq!(
        result["matches"],
        json!([{
            "path": "visible.txt", "line": 1, "column": 1, "text": "needle visible", "context": []
        }])
    );
}

#[tokio::test]
async fn rejects_invalid_patterns_paths_and_cancelled_searches() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("file"), "text").unwrap();
    let tool = SearchText::new(Workspace::new(directory.path()).unwrap());

    for arguments in [
        json!({ "query": "" }),
        json!({ "query": ".*", "regex": true }),
        json!({ "query": "text", "glob": "[" }),
        json!({ "query": "text", "path": ".." }),
        json!({ "query": "text", "context_lines": 11 }),
        json!({ "query": "text", "limit": 0 }),
        json!({ "query": "text", "unknown": true }),
    ] {
        assert!(
            matches!(
                tool.execute(
                    ToolCall::function("search", "search_text", arguments.clone()),
                    CancellationToken::new(),
                )
                .await,
                Err(ToolError::InvalidArguments { .. })
            ),
            "expected invalid arguments: {arguments}"
        );
    }

    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        tool.execute(
            ToolCall::function("search", "search_text", json!({ "query": "text" })),
            token,
        )
        .await,
        Err(ToolError::Cancelled)
    ));
}

#[tokio::test]
async fn bounds_serialized_results_and_reports_truncation() {
    let directory = tempfile::tempdir().unwrap();
    for index in 0..800 {
        fs::write(
            directory.path().join(format!("{index:04}.txt")),
            format!("needle {}\n", "长".repeat(300)),
        )
        .unwrap();
    }

    let tool = SearchText::new(Workspace::new(directory.path()).unwrap());

    let result = run(&tool, json!({ "query": "needle", "limit": 500 })).await;
    assert!(result.to_string().len() <= 32 * 1024);
    assert_eq!(result["has_more"], true);
    assert!(result["next_offset"].as_u64().unwrap() > 0);
}
