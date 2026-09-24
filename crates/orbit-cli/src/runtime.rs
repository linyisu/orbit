use std::path::{Path, PathBuf};

use anyhow::Result;
use orbit_agent::tool::ToolRegistry;
use orbit_tools::{
    EditFile, ListFiles, ReadFile, RunCommand, SearchText, WriteFile, approval::ApprovalSender,
    workspace::Workspace,
};

/// Every tool shares the same protected workspace and explicit approval channel.
pub fn workspace_tools(
    root: &Path,
    config: &Path,
    database: &Path,
    approvals: ApprovalSender,
    rust_toolchain: Option<PathBuf>,
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

    let mut tools = ToolRegistry::new();
    tools.register(ReadFile::new(workspace.clone()));
    tools.register(ListFiles::new(workspace.clone()));
    tools.register(SearchText::new(workspace.clone()));
    tools.register(WriteFile::new(workspace.clone(), approvals.clone()));
    tools.register(EditFile::new(workspace.clone(), approvals.clone()));
    tools.register(RunCommand::new(workspace, approvals).with_toolchain(rust_toolchain));
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
        let config = project.path().join("config.toml");
        std::fs::write(&config, "api_key = 'secret'\n").unwrap();

        let database = project.path().join("sessions.sqlite");
        let (approvals, _receiver) = orbit_tools::approval::channel();
        let tools = workspace_tools(project.path(), &config, &database, approvals, None).unwrap();
        let names = tools
            .definitions()
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "edit_file",
                "list_files",
                "read_file",
                "run_command",
                "search_text",
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
        let (approvals, receiver) = orbit_tools::approval::channel();
        let tools = workspace_tools(project.path(), &config, &database, approvals, None).unwrap();
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
    async fn denied_approval_prevents_command_execution() {
        let project = tempfile::tempdir().unwrap();
        let config = project.path().join("config.toml");
        std::fs::write(&config, "api_key = 'secret'\n").unwrap();
        let database = project.path().join("sessions.db");
        let (approvals, receiver) = orbit_tools::approval::channel();
        let tools = workspace_tools(project.path(), &config, &database, approvals, None).unwrap();
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
