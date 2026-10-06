use orbit_agent::tool::{ClosureTool, ToolRegistry};
use tokio_util::sync::CancellationToken;

use super::*;
use crate::chat::{ChatCapability, disable_chat_capabilities, select_chat_model};
use crate::config::FileConfig;

#[tokio::test]
async fn project_resources_follow_the_destination_without_changing_process_cwd() {
    let directory = tempfile::tempdir().unwrap();
    let original_cwd = std::env::current_dir().unwrap();
    let old = directory.path().join("old");
    let new = directory.path().join("new");
    std::fs::create_dir(&old).unwrap();
    std::fs::create_dir(&new).unwrap();
    std::fs::write(new.join("AGENTS.md"), "destination-specific instructions").unwrap();
    std::fs::write(old.join("marker.txt"), "old project").unwrap();
    std::fs::write(new.join("marker.txt"), "new project").unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(
        &config,
        r#"default_provider = "test"
[providers.test]
base_url = "http://127.0.0.1:1/v1"
auto_refresh_models = false
api_key = "test-only"
[[providers.test.models]]
id = "test"
"#,
    )
    .unwrap();
    let options = WorkspaceOptions {
        initial_directory: old.clone(),
        auth_directory: directory.path().join("home/.config/orbit/auth"),
        config,
        database: directory.path().join("test.db"),
        allow_mcp_start: false,
        home: None,
        config_home: None,
        instruction_dir: None,
        editor_command: "vi".into(),
    };
    let prepared = prepare_workspace(&options, &old, None).await.unwrap();

    assert!(!options.auth_directory.exists());
    assert!(
        prepared
            .resources
            .workspace
            .protected()
            .contains(&options.auth_directory)
    );
    let id = prepared.session.id().to_owned();

    let resources = prepare_workspace_resources(&options, &new, &prepared.session)
        .await
        .unwrap();
    let output = resources
        .tools
        .execute(
            orbit_model::types::ToolCall::function(
                "read",
                "read_file",
                serde_json::json!({ "path": "marker.txt" }),
            ),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(output.content.contains("new project"), "{output:?}");
    assert!(
        resources
            .budget
            .instructions
            .content
            .contains("destination-specific instructions")
    );
    assert_eq!(prepared.session.id(), id);
    assert_eq!(
        prepared.session.snapshot.session.workspace,
        old.to_string_lossy()
    );
    assert_eq!(std::env::current_dir().unwrap(), original_cwd);
}

#[tokio::test]
async fn fresh_launch_uses_its_directory_instead_of_the_previous_session() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second/subdirectory");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(first.join("marker.txt"), "previous workspace").unwrap();
    std::fs::write(second.join("marker.txt"), "launch workspace").unwrap();
    let config = first.join("config.toml");
    std::fs::write(
        &config,
        r#"default_provider = "test"
[providers.test]
base_url = "http://127.0.0.1:1/v1"
auto_refresh_models = false
api_key = "test-only"
models = [{ id = "test" }]
"#,
    )
    .unwrap();
    let options = WorkspaceOptions {
        initial_directory: second.clone(),
        auth_directory: directory.path().join("auth"),
        config,
        database: directory.path().join("sessions.db"),
        allow_mcp_start: false,
        home: None,
        config_home: None,
        instruction_dir: None,
        editor_command: "vi".into(),
    };
    let previous = prepare_workspace(&options, &first, None).await.unwrap();
    let fresh = prepare_workspace(&options, &second, None).await.unwrap();
    assert_ne!(previous.session.id(), fresh.session.id());
    assert_ne!(
        previous.session.snapshot.session.project_id,
        fresh.session.snapshot.session.project_id,
    );
    assert_eq!(fresh.resources.workspace.root(), second);
    assert_eq!(
        fresh.session.snapshot.session.workspace,
        second.to_str().unwrap()
    );
    assert_eq!(
        fresh
            .session
            .store
            .session(fresh.session.id())
            .await
            .unwrap()
            .workspace,
        second.to_str().unwrap(),
    );
    let output = fresh
        .resources
        .tools
        .execute(
            orbit_model::types::ToolCall::function(
                "read",
                "read_file",
                serde_json::json!({"path": "marker.txt"}),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(output.content.contains("launch workspace"));
    assert!(!output.content.contains("previous workspace"));
}

#[test]
fn chat_capability_filter_removes_groups_before_request_assembly() {
    let mut tools = ToolRegistry::new();

    for name in [
        "read_file",
        "project_overview",
        "write_file",
        "request_capability",
        "web_search",
        "web_fetch",
        "mcp__test__lookup",
        "task_list",
        "read_history",
    ] {
        tools.register(ClosureTool::new(
            name,
            "test",
            serde_json::json!({}),
            |_, _| async { Ok::<_, orbit_agent::tool::ToolError>("ok".into()) },
        ));
    }

    let disabled = [
        ChatCapability::Files,
        ChatCapability::Web,
        ChatCapability::Mcp,
        ChatCapability::ProjectState,
        ChatCapability::History,
        ChatCapability::Capability,
    ]
    .into_iter()
    .collect();

    disable_chat_capabilities(&mut tools, &disabled);

    assert!(!tools.contains("read_file"));
    assert!(!tools.contains("project_overview"));
    assert!(!tools.contains("request_capability"));
    assert!(!tools.contains("web_search"));
    assert!(!tools.contains("web_fetch"));
    assert!(!tools.contains("mcp__test__lookup"));
    assert!(!tools.contains("task_list"));
    assert!(!tools.contains("read_history"));
    assert!(tools.contains("write_file"));
}

#[test]
fn disabled_commands_hide_audit_and_export_tools() {
    let mut tools = ToolRegistry::new();

    for name in [
        "run_command",
        "read_command_output",
        "verification_status",
        "export_artifact",
    ] {
        tools.register(ClosureTool::new(
            name,
            "",
            serde_json::json!({}),
            |_, _| async { Ok::<_, orbit_agent::tool::ToolError>("unused".into()) },
        ));
    }

    disable_chat_capabilities(&mut tools, &[ChatCapability::Command].into_iter().collect());

    assert!(tools.definitions().is_empty());
}

#[test]
fn disabled_git_hides_patch_export_as_well_as_inspection() {
    let mut tools = ToolRegistry::new();

    for name in ["git_inspect", "git_export_patch"] {
        tools.register(ClosureTool::new(
            name,
            "",
            serde_json::json!({}),
            |_, _| async { Ok::<_, orbit_agent::tool::ToolError>("unused".into()) },
        ));
    }

    disable_chat_capabilities(&mut tools, &[ChatCapability::Git].into_iter().collect());

    assert!(tools.definitions().is_empty());
}

#[test]
fn batch_model_selection_accepts_qualified_names_and_rejects_ambiguity() {
    let config: FileConfig = toml::from_str(
        r#"default_provider = "one"
[providers.one]
base_url = "https://one.example/v1"
api_key = "one-key"
[[providers.one.models]]
id = "shared"
[[providers.one.models]]
id = "one-only"

[providers.two]
base_url = "https://two.example/v1"
api_key = "two-key"
[[providers.two.models]]
id = "shared"
"#,
    )
    .unwrap();

    assert_eq!(
        select_chat_model(&config, Some("two::shared"))
            .unwrap()
            .qualified_name(),
        "two::shared"
    );
    assert_eq!(
        select_chat_model(&config, Some("one-only"))
            .unwrap()
            .qualified_name(),
        "one::one-only"
    );
    assert!(select_chat_model(&config, Some("shared")).is_err());
}
