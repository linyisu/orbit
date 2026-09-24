use std::path::{Path, PathBuf};

use anyhow::Result;
use orbit_agent::tool::ToolRegistry;
use orbit_store::Store;
use orbit_tools::{
    EditFile, GitInspect, ListFiles, ProjectInstructions, ProjectOverview, ReadCommandOutput,
    ReadDocument, ReadFile, ReadHistory, ReadImage, RunCommand, SearchHistory, SearchText,
    VerificationStatus, WriteFile,
    approval::ApprovalSender,
    web::{WebConfig, WebFetch, WebSearch},
    workspace::Workspace,
};

pub struct WorkspaceToolOptions {
    pub rust_toolchain: Option<PathBuf>,
    pub web: WebConfig,
    pub web_enabled: bool,
}

impl Default for WorkspaceToolOptions {
    fn default() -> Self {
        Self {
            rust_toolchain: None,
            web: WebConfig::default(),
            web_enabled: true,
        }
    }
}

/// Every tool shares the same protected workspace and explicit approval channel.
pub fn workspace_tools(
    root: &Path,
    config: &Path,
    database: &Path,
    store: Store,
    session_id: &str,
    approvals: ApprovalSender,
    options: WorkspaceToolOptions,
) -> Result<ToolRegistry> {
    let mut workspace = Workspace::new(root)?;
    workspace.protect(config)?;
    workspace.protect(database)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = database.as_os_str().to_os_string();
        sidecar.push(suffix);
        workspace.protect(std::path::PathBuf::from(sidecar))?;
    }
    workspace.protect_opaque_runtime_root(database.with_extension("session-locks"))?;
    workspace.protect_opaque_runtime_root(database.with_extension("attachments"))?;

    let mut tools = ToolRegistry::new();
    orbit_tools::project_state::register(
        &mut tools,
        store.clone(),
        root.to_string_lossy().into_owned(),
        session_id.to_owned(),
    );
    tools.register(ReadCommandOutput::new(store.clone(), session_id.to_owned()));
    tools.register(ReadFile::new(workspace.clone()));
    tools.register(ReadDocument::new(workspace.clone()));
    tools.register(ReadImage::new(
        workspace.clone(),
        store.clone(),
        session_id.to_owned(),
    ));
    tools.register(ReadHistory::new(store.clone(), session_id.to_owned()));
    tools.register(SearchHistory::new(store.clone(), session_id.to_owned()));
    tools.register(ListFiles::new(workspace.clone()));
    tools.register(GitInspect::new(workspace.clone()));
    tools.register(ProjectOverview::new(workspace.clone()));
    tools.register(ProjectInstructions::new(workspace.clone()));
    tools.register(VerificationStatus::new(
        workspace.clone(),
        store.clone(),
        session_id.to_owned(),
    ));
    tools.register(SearchText::new(workspace.clone()));
    if options.web_enabled {
        tools.register(WebFetch::new(options.web.clone())?);
        tools.register(WebSearch::new(options.web)?);
    }
    tools.register(WriteFile::new(workspace.clone(), approvals.clone()));
    tools.register(EditFile::new(workspace.clone(), approvals.clone()));
    tools.register(
        RunCommand::new(workspace, approvals, store, session_id.to_owned())
            .with_toolchain(options.rust_toolchain),
    );
    Ok(tools)
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_model::types::ToolCall;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn chat_tools_read_project_files_but_not_the_loaded_config() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(project.path().join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(
            project.path().join("AGENTS.md"),
            "Run cargo test before finishing.\n",
        )
        .unwrap();
        let config = project.path().join("config.toml");
        std::fs::write(&config, "api_key = 'secret'\n").unwrap();

        let database = project.path().join("sessions.sqlite");
        let store = Store::open(&database).await.unwrap();
        let writer = store.create(project.path(), "test-model").await.unwrap();
        let (approvals, _receiver) = orbit_tools::approval::channel();
        let tools = workspace_tools(
            project.path(),
            &config,
            &database,
            store,
            writer.id(),
            approvals,
            WorkspaceToolOptions::default(),
        )
        .unwrap();
        let names = tools
            .definitions()
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "edit_file",
                "git_inspect",
                "list_files",
                "memory_forget",
                "memory_list",
                "memory_save",
                "memory_search",
                "project_instructions",
                "project_overview",
                "read_command_output",
                "read_document",
                "read_file",
                "read_history",
                "read_image",
                "run_command",
                "search_history",
                "search_text",
                "task_list",
                "task_update",
                "verification_status",
                "web_fetch",
                "web_search",
                "write_file"
            ]
        );

        let result = tools
            .execute(
                ToolCall::function("read-1", "read_file", json!({ "path": "main.rs" })),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.contains("fn main()"));
        let rules = tools
            .execute(
                ToolCall::function(
                    "rules-1",
                    "project_instructions",
                    json!({ "path": "main.rs" }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(
            rules.contains("Run cargo test before finishing."),
            "{rules}"
        );

        let denied = tools
            .execute(
                ToolCall::function("read-2", "read_file", json!({ "path": "config.toml" })),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(denied.to_string().contains("blocked"));

        std::fs::write(&database, "private session").unwrap();
        std::fs::write(project.path().join("sessions.sqlite-wal"), "private wal").unwrap();
        std::fs::create_dir_all(database.with_extension("session-locks")).unwrap();
        std::fs::write(
            database.with_extension("session-locks").join("active"),
            "private lock",
        )
        .unwrap();

        for path in [
            "sessions.sqlite",
            "sessions.sqlite-wal",
            "sessions.session-locks/active",
        ] {
            let denied = tools
                .execute(
                    ToolCall::function(
                        format!("read-{path}"),
                        "read_file",
                        json!({ "path": path }),
                    ),
                    CancellationToken::new(),
                )
                .await
                .unwrap_err();
            assert!(denied.to_string().contains("blocked"), "{path}: {denied}");
        }
    }

    #[tokio::test]
    async fn registered_tools_support_approved_edit_and_sandbox_verification() {
        let project = tempfile::tempdir().unwrap();
        let config = project.path().join("config.toml");
        std::fs::write(&config, "api_key = 'secret'\n").unwrap();
        let database = project.path().join("sessions.db");
        let store = Store::open(&database).await.unwrap();
        let writer = store.create(project.path(), "test-model").await.unwrap();
        let (approvals, receiver) = orbit_tools::approval::channel();
        let tools = workspace_tools(
            project.path(),
            &config,
            &database,
            store,
            writer.id(),
            approvals,
            WorkspaceToolOptions::default(),
        )
        .unwrap();
        let workspace = project.path().to_path_buf();
        let approval_task = tokio::spawn(async move {
            crate::approval::serve(receiver, crate::approval::ApprovalMode::Auto, &workspace).await;
        });

        tools
            .execute(
                ToolCall::function(
                    "write-1",
                    "write_file",
                    json!({ "path": "main.txt", "content": "before\n" }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        tools
            .execute(
                ToolCall::function(
                    "edit-1",
                    "edit_file",
                    json!({
                        "path": "main.txt",
                        "old_text": "before",
                        "new_text": "after"
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        let output = tools
            .execute(
                ToolCall::function(
                    "command-1",
                    "run_command",
                    json!({ "command": "cat main.txt" }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        approval_task.abort();

        let output: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            output["exit_code"], 0,
            "stdout: {}; stderr: {}",
            output["stdout"], output["stderr"]
        );
        assert_eq!(
            output["stdout"], "after\n",
            "stdout: {}; stderr: {}",
            output["stdout"], output["stderr"]
        );
        assert_eq!(
            std::fs::read_to_string(project.path().join("main.txt")).unwrap(),
            "after\n"
        );
    }

    #[tokio::test]
    async fn long_command_output_can_be_paged_after_reopening_the_session() {
        let project = tempfile::tempdir().unwrap();
        let config = project.path().join("config.toml");
        std::fs::write(&config, "api_key = 'secret'\n").unwrap();
        let database = project.path().join("sessions.db");
        let store = Store::open(&database).await.unwrap();
        let writer = store.create(project.path(), "test-model").await.unwrap();
        let session_id = writer.id().to_owned();
        let (approvals, receiver) = orbit_tools::approval::channel();
        let tools = workspace_tools(
            project.path(),
            &config,
            &database,
            store.clone(),
            &session_id,
            approvals,
            WorkspaceToolOptions::default(),
        )
        .unwrap();
        let workspace = project.path().to_path_buf();
        let approval_task = tokio::spawn(async move {
            crate::approval::serve(receiver, crate::approval::ApprovalMode::Auto, &workspace).await;
        });

        let response = tools
            .execute(
                ToolCall::function(
                    "command-long",
                    "run_command",
                    json!({ "command": "seq 1 10000" }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        approval_task.abort();
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(
            response["exit_code"], 0,
            "stdout: {}; stderr: {}",
            response["stdout"], response["stderr"]
        );
        assert!(response["output_truncated"].as_bool().unwrap());
        assert!(response["stdout_total_bytes"].as_u64().unwrap() > 32 * 1024);
        let output_id = response["output_id"].as_str().unwrap();
        drop(tools);
        drop(writer);
        drop(store);

        let reopened = Store::open(&database).await.unwrap();
        let _resumed = reopened.acquire(&session_id).await.unwrap();
        let (approvals, _receiver) = orbit_tools::approval::channel();
        let resumed_tools = workspace_tools(
            project.path(),
            &config,
            &database,
            reopened,
            &session_id,
            approvals,
            WorkspaceToolOptions::default(),
        )
        .unwrap();
        let page = resumed_tools
            .execute(
                ToolCall::function(
                    "read-tail",
                    "read_command_output",
                    json!({
                        "output_id": output_id,
                        "stream": "stdout",
                        "offset": 48_000,
                        "limit": 8192
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let page: serde_json::Value = serde_json::from_str(&page).unwrap();
        assert!(
            page["content"].as_str().unwrap().contains("10000\n"),
            "{page}"
        );
        assert_eq!(page["has_more"], false);
    }

    #[tokio::test]
    async fn verification_status_marks_command_stale_after_persistent_edit() {
        let project = tempfile::tempdir().unwrap();
        let config = project.path().join("config.toml");
        std::fs::write(&config, "api_key = 'secret'\n").unwrap();
        let database = project.path().join("sessions.db");
        let store = Store::open(&database).await.unwrap();
        let writer = store.create(project.path(), "test-model").await.unwrap();
        let (approvals, receiver) = orbit_tools::approval::channel();
        let tools = workspace_tools(
            project.path(),
            &config,
            &database,
            store.clone(),
            writer.id(),
            approvals,
            WorkspaceToolOptions::default(),
        )
        .unwrap();
        let workspace = project.path().to_path_buf();
        let approval_task = tokio::spawn(async move {
            crate::approval::serve(receiver, crate::approval::ApprovalMode::Auto, &workspace).await;
        });

        tools
            .execute(
                ToolCall::function(
                    "write",
                    "write_file",
                    json!({ "path": "main.txt", "content": "before\n" }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let command = tools
            .execute(
                ToolCall::function("run", "run_command", json!({ "command": "cat main.txt" })),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let command: serde_json::Value = serde_json::from_str(&command).unwrap();
        assert_eq!(command["exit_code"], 0, "{command}");
        let before = tools
            .execute(
                ToolCall::function("verify-1", "verification_status", json!({})),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let before: serde_json::Value = serde_json::from_str(&before).unwrap();
        assert_eq!(before["commands"][0]["source_state"], "current", "{before}");
        assert_eq!(before["commands"][0]["output_id"], command["output_id"]);

        tools
            .execute(
                ToolCall::function(
                    "edit",
                    "edit_file",
                    json!({
                        "path": "main.txt", "old_text": "before", "new_text": "after"
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let after = tools
            .execute(
                ToolCall::function("verify-2", "verification_status", json!({})),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        approval_task.abort();
        let after: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert_eq!(after["commands"][0]["source_state"], "stale", "{after}");
        assert_eq!(after["commands"][0]["succeeded"], true);
        assert_ne!(before["current_snapshot_id"], after["current_snapshot_id"]);

        let session_id = writer.id().to_owned();
        drop(tools);
        drop(writer);
        drop(store);
        let reopened = Store::open(&database).await.unwrap();
        let records = reopened
            .verification_records(&session_id, 20)
            .await
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].output_id, command["output_id"]);
    }

    #[tokio::test]
    async fn coding_workflow_connects_overview_edit_command_verification_and_git() {
        use std::process::Command;

        let project = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = Command::new("/usr/bin/git")
                .current_dir(project.path())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?} failed; stdout: {}; stderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Orbit test"]);
        git(&["config", "user.email", "test@example.invalid"]);
        std::fs::write(
            project.path().join("Cargo.toml"),
            "[package]\nname = 'demo'\nversion = '0.1.0'\nedition = '2024'\n",
        )
        .unwrap();
        std::fs::create_dir(project.path().join("src")).unwrap();
        std::fs::write(
            project.path().join("src/lib.rs"),
            "pub fn answer() -> i32 { 1 }\n",
        )
        .unwrap();
        git(&["add", "Cargo.toml", "src/lib.rs"]);
        git(&["commit", "-qm", "initial"]);

        let config = project.path().join("config.toml");
        std::fs::write(&config, "api_key = 'private'\n").unwrap();
        let database = project.path().join("sessions.db");
        let store = Store::open(&database).await.unwrap();
        let writer = store.create(project.path(), "model").await.unwrap();
        let (approvals, receiver) = orbit_tools::approval::channel();
        let tools = workspace_tools(
            project.path(),
            &config,
            &database,
            store,
            writer.id(),
            approvals,
            WorkspaceToolOptions::default(),
        )
        .unwrap();
        let workspace = project.path().to_path_buf();
        let approval_task = tokio::spawn(async move {
            crate::approval::serve(receiver, crate::approval::ApprovalMode::Auto, &workspace).await;
        });
        let call =
            |id: &str, name: &str, args: serde_json::Value| ToolCall::function(id, name, args);

        let overview = tools
            .execute(
                call("overview", "project_overview", json!({ "query": "answer" })),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(overview.contains("Cargo.toml"), "{overview}");
        let source = tools
            .execute(
                call("read", "read_file", json!({ "path": "src/lib.rs" })),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(source.contains("answer"), "{source}");
        tools
            .execute(
                call(
                    "edit",
                    "edit_file",
                    json!({
                        "path": "src/lib.rs", "old_text": "{ 1 }", "new_text": "{ 2 }"
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let command = tools
            .execute(
                call(
                    "run",
                    "run_command",
                    json!({ "command": "grep -q '{ 2 }' src/lib.rs" }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        approval_task.abort();
        let command: serde_json::Value = serde_json::from_str(&command).unwrap();
        assert_eq!(
            command["exit_code"], 0,
            "stdout: {}; stderr: {}",
            command["stdout"], command["stderr"]
        );
        let verification = tools
            .execute(
                call("verify", "verification_status", json!({})),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let verification: serde_json::Value = serde_json::from_str(&verification).unwrap();
        assert_eq!(verification["commands"][0]["source_state"], "current");
        let diff = tools
            .execute(
                call(
                    "diff",
                    "git_inspect",
                    json!({ "view": "diff", "path": "src/lib.rs" }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(diff.contains("+pub fn answer() -> i32 { 2 }"), "{diff}");
        assert!(!project.path().join(".git/index.lock").exists());
    }

    #[tokio::test]
    async fn denied_approval_prevents_command_execution() {
        let project = tempfile::tempdir().unwrap();
        let config = project.path().join("config.toml");
        std::fs::write(&config, "api_key = 'secret'\n").unwrap();
        let database = project.path().join("sessions.db");
        let store = Store::open(&database).await.unwrap();
        let writer = store.create(project.path(), "test-model").await.unwrap();
        let (approvals, receiver) = orbit_tools::approval::channel();
        let tools = workspace_tools(
            project.path(),
            &config,
            &database,
            store,
            writer.id(),
            approvals,
            WorkspaceToolOptions::default(),
        )
        .unwrap();
        let workspace = project.path().to_path_buf();
        let approval_task = tokio::spawn(async move {
            crate::approval::serve(receiver, crate::approval::ApprovalMode::Deny, &workspace).await;
        });

        let denied = tools
            .execute(
                ToolCall::function(
                    "command-1",
                    "run_command",
                    json!({ "command": "touch /workspace/created" }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        approval_task.abort();

        assert!(denied.to_string().contains("denied"), "{denied}");
        assert!(!project.path().join("created").exists());
    }
}
