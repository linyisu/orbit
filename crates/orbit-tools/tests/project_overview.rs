use std::fs;

use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_tools::{ProjectOverview, Workspace};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn put(root: &std::path::Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

async fn overview(tool: &ProjectOverview, arguments: Value) -> Value {
    let output = tool
        .execute(
            ToolCall::function("overview", "project_overview", arguments),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(output.len() <= 32 * 1024, "output bytes: {}", output.len());
    serde_json::from_str(&output).unwrap()
}

#[tokio::test]
async fn summarizes_manifests_tests_and_symbols_without_executing_scripts() {
    let root = tempfile::tempdir().unwrap();
    put(
        root.path(),
        "Cargo.toml",
        "[package]\nname = 'demo'\nversion = '0.1.0'\n",
    );
    put(
        root.path(),
        "src/lib.rs",
        "pub fn parse_input() {}\npub struct Parser;\n",
    );
    put(
        root.path(),
        "tests/parser.rs",
        "fn handles_empty_input() {}\n",
    );
    put(root.path(), "AGENTS.md", "Read before editing.\n");
    put(
        root.path(),
        "frontend/package.json",
        r#"{"name":"web","scripts":{"test":"touch SHOULD_NOT_EXIST"}}"#,
    );
    let tool = ProjectOverview::new(Workspace::new(root.path()).unwrap());

    let result = overview(&tool, json!({ "query": "parse input" })).await;
    assert_eq!(result["inventory"]["languages"]["Rust"], 2);
    assert!(result["manifests"].to_string().contains("demo"), "{result}");
    assert!(result["manifests"].to_string().contains("test"), "{result}");
    assert_eq!(result["instruction_files"], json!(["AGENTS.md"]));
    assert_eq!(result["test_files"], json!(["tests/parser.rs"]));
    assert!(
        result["symbols"].to_string().contains("parse_input"),
        "{result}"
    );
    assert!(!root.path().join("SHOULD_NOT_EXIST").exists());

    let scoped = overview(&tool, json!({ "path": "frontend" })).await;
    assert_eq!(scoped["inventory"]["languages"], json!({}));
    assert_eq!(scoped["manifests"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn excludes_ignored_and_protected_aliases_and_bounds_the_result() {
    let root = tempfile::tempdir().unwrap();
    put(root.path(), ".gitignore", "ignored/\n");
    put(
        root.path(),
        "ignored/hidden.py",
        "def should_not_appear(): pass\n",
    );
    put(root.path(), "private.rs", "pub fn private_secret() {}\n");
    fs::hard_link(root.path().join("private.rs"), root.path().join("alias.rs")).unwrap();
    for index in 0..300 {
        put(
            root.path(),
            &format!("src/long_name_{index}.rs"),
            "pub fn visible() {}\n",
        );
    }
    let mut workspace = Workspace::new(root.path()).unwrap();
    workspace.protect(root.path().join("private.rs")).unwrap();
    let tool = ProjectOverview::new(workspace);

    let result = overview(&tool, json!({})).await;
    assert!(!result.to_string().contains("private_secret"), "{result}");
    assert!(!result.to_string().contains("alias.rs"), "{result}");
    assert!(!result.to_string().contains("hidden.py"), "{result}");
    assert_eq!(result["inventory"]["files"], 301);
    assert_eq!(result["truncated"]["content"], true);
}
