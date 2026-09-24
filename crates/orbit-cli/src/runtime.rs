use std::path::Path;

use anyhow::Result;
use orbit_agent::tool::ToolRegistry;
use orbit_tools::{ListFiles, ReadFile, SearchText, workspace::Workspace};

/// A one-shot chat has no approval receiver, so only read-only tools are
/// available. Every tool shares the same workspace and protected config path.
pub fn read_only_workspace_tools(
    root: &Path,
    config: &Path,
    database: &Path,
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
    tools.register(SearchText::new(workspace));
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
        let tools = read_only_workspace_tools(project.path(), &config, &database).unwrap();
        let names = tools
            .definitions()
            .into_iter()
            .map(|definition| definition.function.name)
            .collect::<Vec<_>>();
        assert_eq!(names, ["list_files", "read_file", "search_text"]);

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
}
