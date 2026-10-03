use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use orbit_agent::{
    agent::{Agent, AgentConfig},
    events::AgentEvent,
    model::MockModel,
    tool::{Tool, ToolError, ToolRegistry},
};
use orbit_model::types::{ChatCompletion, ChatMessage, ChatRequest, ToolCall};
use orbit_runtime::filesystem::Workspace;
use orbit_tools::read_file::ReadFile;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "orbit-tools-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();

        Self(path)
    }

    fn tool(&self) -> ReadFile {
        ReadFile::new(Workspace::new(&self.0).unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn call(arguments: Value) -> ToolCall {
    ToolCall::function("read-1", "read_file", arguments)
}

#[tokio::test]
async fn reads_numbered_unicode_lines_and_reports_remaining_content() {
    let root = Fixture::new();
    fs::write(root.0.join("source.rs"), "第一行\nsecond\nthird\n").unwrap();
    let output = root
        .tool()
        .execute(
            call(json!({ "path": "source.rs", "start_line": 2, "max_lines": 1 })),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();

    assert_eq!(value["content"], "2: second\n");
    assert!(value["total_lines"].is_null());
    assert_eq!(value["truncated"], true);
}

#[tokio::test]
async fn a_long_unicode_line_can_be_read_without_losing_its_tail() {
    let root = Fixture::new();
    let original = "汉".repeat(15_000);
    fs::write(root.0.join("long.txt"), &original).unwrap();
    let tool = root.tool();
    let first = tool
        .execute(
            call(json!({ "path": "long.txt" })),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let first: Value = serde_json::from_str(&first).unwrap();

    let second = tool
        .execute(
            call(json!({ "path": "long.txt", "start_line": first["next_start_line"], "start_char": first["next_start_char"] })),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let second: Value = serde_json::from_str(&second).unwrap();
    let reconstructed = format!(
        "{}{}",
        first["content"]
            .as_str()
            .unwrap()
            .strip_prefix("1: ")
            .unwrap(),
        second["content"]
            .as_str()
            .unwrap()
            .strip_prefix("1: ")
            .unwrap()
            .trim_end_matches('\n')
    );

    assert_eq!(reconstructed, original);
    assert_eq!(first["truncated"], true);
    assert_eq!(second["truncated"], false);
}

#[tokio::test]
async fn rejects_escape_paths_symlinks_and_private_metadata() {
    let root = Fixture::new();
    let outside = Fixture::new();
    fs::write(outside.0.join("secret"), "private").unwrap();
    std::os::unix::fs::symlink(&outside.0, root.0.join("linked-dir")).unwrap();
    std::os::unix::fs::symlink(outside.0.join("secret"), root.0.join("linked-file")).unwrap();

    for path in [
        "../secret",
        "/etc/passwd",
        "linked-dir/secret",
        "linked-file",
        ".env",
        ".env.local",
        ".git/config",
    ] {
        let result = root
            .tool()
            .execute(call(json!({ "path": path })), CancellationToken::new())
            .await;

        assert!(result.is_err(), "must reject {path}");
    }
}

#[tokio::test]
async fn permits_non_protected_hidden_metadata() {
    let root = Fixture::new();
    fs::create_dir_all(root.0.join(".agent-state")).unwrap();
    fs::write(root.0.join(".agent-state/session"), "metadata").unwrap();

    let output = root
        .tool()
        .execute(
            call(json!({ "path": ".agent-state/session" })),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();

    assert_eq!(value["content"], "1: metadata\n");
}

#[tokio::test]
async fn permits_ordinary_configuration_filenames() {
    let root = Fixture::new();
    fs::write(root.0.join("config.toml"), "feature = true\n").unwrap();

    let output = root
        .tool()
        .execute(
            call(json!({ "path": "config.toml" })),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();

    assert_eq!(value["content"], "1: feature = true\n");
}

#[tokio::test]
async fn protects_custom_config_and_its_hard_links() {
    let root = Fixture::new();
    fs::write(root.0.join("credentials.toml"), "private").unwrap();
    fs::hard_link(root.0.join("credentials.toml"), root.0.join("alias.txt")).unwrap();
    let mut workspace = Workspace::new(&root.0).unwrap();
    workspace.protect(root.0.join("credentials.toml")).unwrap();
    let tool = ReadFile::new(workspace);

    for path in ["credentials.toml", "alias.txt"] {
        let error = tool
            .execute(call(json!({ "path": path })), CancellationToken::new())
            .await
            .unwrap_err();

        assert!(error.to_string().contains("blocked"));
    }
}

#[tokio::test]
async fn validates_ranges_and_rejects_binary_oversized_and_nonregular_files() {
    let root = Fixture::new();
    fs::write(root.0.join("text"), "hello").unwrap();
    fs::write(root.0.join("binary"), b"a\0b").unwrap();
    fs::write(root.0.join("large"), vec![b'a'; 1024 * 1024 + 1]).unwrap();
    fs::create_dir(root.0.join("directory")).unwrap();

    for args in [
        json!({ "path": "text", "start_line": 0 }),
        json!({ "path": "text", "start_line": 2 }),
        json!({ "path": "text", "max_lines": 501 }),
        json!({ "path": "text", "extra": true }),
        json!({ "path": "binary" }),
        json!({ "path": "directory" }),
        json!({ "path": "missing" }),
    ] {
        assert!(
            root.tool()
                .execute(call(args.clone()), CancellationToken::new())
                .await
                .is_err(),
            "{args}"
        );
    }
}

#[tokio::test]
async fn bounds_unicode_output_and_honors_cancellation() {
    let root = Fixture::new();
    fs::write(root.0.join("long.txt"), "中".repeat(20000)).unwrap();
    let value: Value = serde_json::from_str(
        &root
            .tool()
            .execute(
                call(json!({ "path": "long.txt" })),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
    )
    .unwrap();

    assert!(value["content"].as_str().unwrap().len() <= 32 * 1024);
    assert_eq!(value["truncated"], true);

    let token = CancellationToken::new();
    token.cancel();

    assert_eq!(
        root.tool()
            .execute(call(json!({ "path": "long.txt" })), token)
            .await
            .unwrap_err(),
        ToolError::Cancelled
    );
}

#[tokio::test]
async fn actual_file_result_and_lifecycle_events_flow_through_agent() {
    let root = Fixture::new();
    fs::write(root.0.join("source.rs"), "fn main() {}\n").unwrap();
    let model = MockModel::new([
        ChatCompletion {
            id: None,
            model: None,
            message: ChatMessage::assistant_tool_calls(vec![call(json!({ "path": "source.rs" }))]),
            finish_reason: Some("tool_calls".into()),
            usage: None,
        },
        ChatCompletion {
            id: None,
            model: None,
            message: ChatMessage::assistant("finished"),
            finish_reason: Some("stop".into()),
            usage: None,
        },
    ]);
    let mut registry = ToolRegistry::new();
    registry.register(root.tool());
    let agent = Agent::new(model.clone(), registry, AgentConfig::default());
    let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
    let result = agent
        .run_with_agent_events(
            ChatRequest::new(vec![ChatMessage::user("read source.rs")]),
            CancellationToken::new(),
            sender,
        )
        .await
        .unwrap();

    assert_eq!(result.turns, 2);
    assert_eq!(result.tool_calls, 1);
    assert!(
        matches!(&model.requests()[1].messages[2], ChatMessage::Tool { content, .. } if content.contains("fn main()"))
    );

    let mut lifecycle = Vec::new();

    while let Some(event) = receiver.recv().await {
        match event {
            AgentEvent::ToolStarted { .. } => lifecycle.push("started"),
            AgentEvent::ToolFinished { content, .. } => {
                assert!(content.contains("fn main()"));

                lifecycle.push("finished");
            }
            AgentEvent::Completed { .. } => lifecycle.push("completed"),
            _ => {}
        }
    }

    assert_eq!(lifecycle, ["started", "finished", "completed"]);
}

#[test]
fn protects_database_sidecars_created_after_registry_initialization() {
    let root = Fixture::new();
    let sidecar = root.0.join("sessions.db-wal");
    let mut workspace = Workspace::new(&root.0).unwrap();
    workspace.protect(&sidecar).unwrap();
    fs::write(&sidecar, "private conversation").unwrap();

    assert!(
        workspace
            .open_read(std::path::Path::new("sessions.db-wal"))
            .is_err()
    );

    fs::hard_link(&sidecar, root.0.join("alias")).unwrap();

    assert!(workspace.open_read(std::path::Path::new("alias")).is_err());
}

#[tokio::test]
async fn read_file_can_read_granted_outside_path() {
    let root = Fixture::new();
    let outside = Fixture::new();
    let outside_file = outside.0.join("external.txt");
    fs::write(&outside_file, "granted outside content\n").unwrap();

    let workspace = Workspace::new(&root.0).unwrap();

    let tool = ReadFile::new(workspace.clone());
    let err = tool
        .execute(
            call(json!({ "path": outside_file.display().to_string() })),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("permission denied"));

    let resolved = orbit_runtime::filesystem::ResolvedPath::resolve(&outside_file, None).unwrap();
    workspace
        .policy()
        .record_grant(orbit_runtime::policy::FilesystemGrant {
            environment: orbit_runtime::capability::EnvironmentId::Local,
            path: resolved,
            read: true,
            write: false,
        });

    let output = tool
        .execute(
            call(json!({ "path": outside_file.display().to_string() })),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&output).unwrap();

    assert_eq!(value["content"], "1: granted outside content\n");
}

#[tokio::test]
async fn reads_middle_of_large_files_and_resumes_versioned_cursor() {
    let root = Fixture::new();
    let text = "data\n".repeat(300_000);
    fs::write(root.0.join("large.txt"), text).unwrap();
    let tool = root.tool();
    let first: Value = serde_json::from_str(
        &tool
            .execute(
                call(json!({"path":"large.txt", "start_line":200_000, "max_lines":2})),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["content"], "200000: data\n200001: data\n");
    let second: Value = serde_json::from_str(
        &tool
            .execute(
                call(json!({"path":"large.txt", "cursor":first["next_cursor"], "max_lines":1})),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(second["content"], "200002: data\n");
    fs::write(root.0.join("large.txt"), "changed").unwrap();
    assert!(
        tool.execute(
            call(json!({"path":"large.txt", "cursor":first["next_cursor"]})),
            CancellationToken::new()
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("changed")
    );
}

#[tokio::test]
async fn permits_internal_links_and_rejects_secret_targets_and_loops() {
    let root = Fixture::new();
    fs::create_dir(root.0.join("actual")).unwrap();
    fs::write(root.0.join("actual/file"), "inside").unwrap();
    fs::write(root.0.join(".env"), "secret").unwrap();
    std::os::unix::fs::symlink("actual", root.0.join("alias")).unwrap();
    std::os::unix::fs::symlink(".env", root.0.join("secret-alias")).unwrap();
    std::os::unix::fs::symlink("loop", root.0.join("loop")).unwrap();
    let tool = root.tool();
    assert!(
        tool.execute(call(json!({"path":"alias/file"})), CancellationToken::new())
            .await
            .unwrap()
            .contains("inside")
    );
    for path in ["secret-alias", "loop"] {
        assert!(
            tool.execute(call(json!({"path":path})), CancellationToken::new())
                .await
                .is_err()
        );
    }
}

#[test]
fn template_trust_is_exact_and_read_write_is_separate() {
    let root = Fixture::new();
    fs::write(root.0.join(".env.example"), "EXAMPLE=1").unwrap();
    fs::write(root.0.join(".env.local"), "SECRET=1").unwrap();
    let mut workspace = Workspace::new(&root.0).unwrap();
    workspace
        .configure_policy(
            orbit_runtime::policy::WorkspacePermissions {
                read_templates: vec![".env.example".into()],
                ..Default::default()
            },
            orbit_runtime::capability::NetworkPolicy::Deny,
        )
        .unwrap();
    assert!(
        workspace
            .open_read(std::path::Path::new(".env.example"))
            .is_ok()
    );
    assert!(
        workspace
            .open_read(std::path::Path::new(".env.local"))
            .is_err()
    );
    assert!(
        workspace
            .authorize_write_parent(std::path::Path::new(".env.example"))
            .is_err()
    );
    workspace
        .configure_policy(
            orbit_runtime::policy::WorkspacePermissions {
                write_templates: vec![".env.example".into()],
                ..Default::default()
            },
            orbit_runtime::capability::NetworkPolicy::Deny,
        )
        .unwrap();
    assert!(
        workspace
            .authorize_write_parent(std::path::Path::new(".env.example"))
            .is_ok()
    );
}

#[tokio::test]
async fn four_byte_unicode_at_eof_never_exceeds_content_budget() {
    let root = Fixture::new();
    let original = format!("a{}", "😀".repeat(8191));
    fs::write(root.0.join("exact.txt"), &original).unwrap();
    let output: Value = serde_json::from_str(
        &root
            .tool()
            .execute(call(json!({"path":"exact.txt"})), CancellationToken::new())
            .await
            .unwrap(),
    )
    .unwrap();
    let content = output["content"].as_str().unwrap();
    assert!(content.len() <= 32 * 1024);
    assert_eq!(
        content.strip_prefix("1: ").unwrap().trim_end_matches('\n'),
        original
    );
    assert_eq!(output["truncated"], false);
}
