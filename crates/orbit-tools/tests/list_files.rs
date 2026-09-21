use std::fs;

use orbit_agent::tool::{Tool, ToolError};
use orbit_model::types::ToolCall;
use orbit_tools::{list_files::ListFiles, workspace::Workspace};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

async fn run(tool: &ListFiles, args: Value) -> Value {
    serde_json::from_str(
        &tool
            .execute(
                ToolCall::function("list", "list_files", args),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn paginates_breadth_first_and_supports_explicit_generated_directories() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["src", "target", ".hidden"] {
        fs::create_dir(dir.path().join(name)).unwrap();
    }
    for name in [
        "README.md",
        "src/中文.rs",
        "src/z.rs",
        "target/build",
        ".hidden/file",
    ] {
        fs::write(dir.path().join(name), "test").unwrap();
    }

    let tool = ListFiles::new(Workspace::new(dir.path()).unwrap());

    let first = run(&tool, json!({ "depth": 3, "limit": 2 })).await;
    assert_eq!(first["next_offset"], 2);

    let second = run(&tool, json!({ "depth": 3, "offset": 2 })).await;
    let paths: Vec<_> = first["entries"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["entries"].as_array().unwrap())
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        paths,
        ["README.md", "src", "target", "src/z.rs", "src/中文.rs"]
    );
    assert_eq!(second["next_offset"], Value::Null);

    assert_eq!(
        run(&tool, json!({ "path": "target" })).await["entries"][0]["path"],
        "target/build"
    );

    let hidden = run(&tool, json!({ "include_hidden": true, "depth": 2 })).await;
    assert!(
        hidden["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["path"] == ".hidden/file")
    );
}

#[tokio::test]
async fn excludes_protected_aliases_symlinks_and_special_files() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("private"), "secret").unwrap();

    fs::create_dir(dir.path().join(".git")).unwrap();
    for name in [".env", "config.toml", "credentials.json", "public"] {
        fs::write(dir.path().join(name), "data").unwrap();
    }
    fs::hard_link(
        dir.path().join("credentials.json"),
        dir.path().join("alias"),
    )
    .unwrap();
    symlink(outside.path(), dir.path().join("linked-dir")).unwrap();
    symlink(
        outside.path().join("private"),
        dir.path().join("linked-file"),
    )
    .unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        dir.path().join("pipe"),
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        0,
    )
    .unwrap();

    let mut workspace = Workspace::new(dir.path()).unwrap();
    workspace.protect(dir.path().join("config.toml")).unwrap();
    workspace
        .protect(dir.path().join("credentials.json"))
        .unwrap();
    let tool = ListFiles::new(workspace);

    assert_eq!(
        run(&tool, json!({ "include_hidden": true, "depth": 8 })).await["entries"],
        json!([{ "path": "public", "kind": "file" }])
    );

    for path in ["..", "/tmp", "linked-dir", ".git", "public"] {
        assert!(
            tool.execute(
                ToolCall::function("list", "list_files", json!({ "path": path })),
                CancellationToken::new()
            )
            .await
            .is_err()
        );
    }

    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        tool.execute(ToolCall::function("list", "list_files", json!({})), token)
            .await,
        Err(ToolError::Cancelled)
    ));
}

#[tokio::test]
async fn validates_arguments_and_caps_serialized_output() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..400 {
        fs::write(
            dir.path().join(format!("{index:04}{}", "长".repeat(70))),
            "",
        )
        .unwrap();
    }

    let tool = ListFiles::new(Workspace::new(dir.path()).unwrap());

    let page = run(&tool, json!({ "limit": 1000 })).await;
    assert!(page.to_string().len() <= 32 * 1024);
    assert_eq!(page["has_more"], true);
    assert!(page["next_offset"].as_u64().unwrap() > 0);

    for args in [
        json!({ "depth": 0 }),
        json!({ "depth": 9 }),
        json!({ "limit": 0 }),
        json!({ "offset": 20001 }),
        json!({ "unknown": true }),
        json!({ "path": "" }),
    ] {
        assert!(matches!(
            tool.execute(
                ToolCall::function("list", "list_files", args),
                CancellationToken::new()
            )
            .await,
            Err(ToolError::InvalidArguments { .. })
        ));
    }
}
