pub mod command_output;
pub mod verification;

use std::path::Path;

use anyhow::Result;
use orbit_agent::tool::ToolRegistry;
use orbit_runtime::filesystem::Workspace;
use orbit_tools::{
    approval::ApprovalSender, edit_file::EditFile, image_assets::ImageAssets,
    list_files::ListFiles, read_command_output::ReadCommandOutput, read_document::ReadDocument,
    read_file::ReadFile, read_image::ReadImage, request_capability::RequestCapability,
    run_command::RunCommand, search_text::SearchText, write_file::WriteFile,
};

pub struct WorkspaceRuntime {
    pub tools: ToolRegistry,
    pub workspace: Workspace,
    pub command_limits: orbit_tools::run_command::CommandLimits,
    pub mcp_statuses: Vec<orbit_tools::mcp::McpServerStatus>,
    pub web_client: std::sync::Arc<orbit_tools::web_fetch::WebClient>,
}

#[allow(clippy::too_many_arguments)]
pub async fn workspace_tools(
    directory: &Path,
    _config: &Path,
    _auth_directory: &Path,
    database: Option<&Path>,
    approvals: Option<ApprovalSender>,
    command_limits: orbit_tools::run_command::CommandLimits,
    web: orbit_tools::web_fetch::WebConfig,
    mcp: orbit_tools::mcp::McpConfig,
    allow_mcp_start: bool,
    command_output: Option<orbit_runtime::execution::output::CommandOutputs>,
    snapshot_directory: Option<std::path::PathBuf>,
    permissions: Option<orbit_runtime::policy::WorkspacePermissions>,
    approval_policy: orbit_runtime::policy::ApprovalPolicy,
) -> Result<WorkspaceRuntime> {
    let approvals = approvals.map(|sender| sender.for_workspace(directory));
    let workspace = configured_workspace(
        directory,
        permissions,
        command_limits.network,
        approval_policy,
    )?;
    let mut tools = ToolRegistry::new();
    tools.register(
        ReadFile::new(workspace.clone()).with_scan_budget(command_limits.read_file_scan_bytes),
    );
    let document = ReadDocument::new(workspace.clone()).with_worker(std::env::current_exe()?);
    tools.register(document);

    if let Some(database) = database {
        tools.register(ReadImage::new(
            workspace.clone(),
            ImageAssets::for_database(database),
        ));
    }

    tools.register(ListFiles::new(workspace.clone()));
    tools.register(SearchText::new(workspace.clone()));
    let index = orbit_tools::repository::RepositoryIndex::default();
    tools.register(
        orbit_tools::project_overview::ProjectOverview::new(workspace.clone())
            .with_index(index.clone()),
    );
    tools.register(orbit_tools::project_overview::CodeSymbols::new(
        workspace.clone(),
        index,
    ));
    // Model-facing Git commands use Exec; GitInspect supplies internal snapshots.

    let web_client = std::sync::Arc::new(orbit_tools::web_fetch::WebClient::new(&web)?);
    let mut web_fetch = orbit_tools::web_fetch::WebFetch::with_client((*web_client).clone());
    if let Some(directory) = snapshot_directory {
        web_fetch = web_fetch.with_snapshot_directory(directory);
    }
    tools.register(web_fetch);
    tools.register(orbit_tools::web_fetch::WebSearch::with_client(
        (*web_client).clone(),
        web.search_providers,
    )?);
    let mcp_approvals = approvals.clone();

    if let Some(approvals) = approvals {
        let jobs = tools.background_jobs().clone();
        for action in [
            orbit_tools::command_jobs::CommandJobAction::Status,
            orbit_tools::command_jobs::CommandJobAction::Wait,
            orbit_tools::command_jobs::CommandJobAction::Cancel,
        ] {
            tools.register(orbit_tools::command_jobs::CommandJobs::new(
                jobs.clone(),
                action,
            ));
        }

        tools.register(WriteFile::new(workspace.clone(), approvals.clone()));
        tools.register(EditFile::new(workspace.clone(), approvals.clone()));
        tools.register(RequestCapability::new(workspace.clone(), approvals.clone()));
        let command = RunCommand::with_limits(workspace.clone(), approvals, command_limits.clone())
            .with_background_jobs(jobs);
        if let Some(command_output) = command_output {
            tools.register(ReadCommandOutput::new(command_output.clone()));
            tools.register(command.with_output_store(command_output));
        } else {
            tools.register(command);
        }
    }

    let mcp_statuses = orbit_tools::mcp::register(
        &mut tools,
        &mcp,
        workspace.root(),
        mcp_approvals,
        allow_mcp_start,
    )
    .await;

    Ok(WorkspaceRuntime {
        web_client,
        workspace,
        command_limits,
        tools,
        mcp_statuses,
    })
}

pub fn workspace_writer(
    directory: &Path,
    _config: &Path,
    _auth_directory: &Path,
    _database: &Path,
    approvals: ApprovalSender,
    permissions: Option<orbit_runtime::policy::WorkspacePermissions>,
) -> Result<WriteFile> {
    Ok(WriteFile::new(
        configured_workspace(
            directory,
            permissions,
            orbit_runtime::capability::NetworkPolicy::Allow,
            approvals.policy().clone(),
        )?,
        approvals.for_workspace(directory),
    ))
}

pub(crate) fn configured_workspace(
    directory: &Path,
    permissions: Option<orbit_runtime::policy::WorkspacePermissions>,
    network: orbit_runtime::capability::NetworkPolicy,
    approval_policy: orbit_runtime::policy::ApprovalPolicy,
) -> Result<Workspace> {
    let mut workspace = Workspace::new(directory)?;
    workspace.configure_policy(permissions.unwrap_or_default(), network)?;
    let mut policy = workspace.policy().clone();
    policy.set_approval_policy(approval_policy);
    workspace.set_policy(policy);
    Ok(workspace)
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_agent::tool::ToolError;
    use orbit_model::types::ToolCall;
    use orbit_tools::approval::ApprovalMode;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn executors_share_yolo_without_an_approval_consumer_or_extra_access_calls() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config, "synthetic configuration").unwrap();
        let (sender, receiver) = orbit_tools::approval::channel();
        sender
            .policy()
            .set_mode(ApprovalMode::Yolo);
        let runtime = workspace_tools(
            &project,
            &config,
            &root.path().join("auth"),
            None,
            Some(sender.clone()),
            Default::default(),
            Default::default(),
            Default::default(),
            false,
            None,
            None,
            None,
            sender.policy().clone(),
        )
        .await
        .unwrap();
        drop(receiver);
        for (name, arguments) in [
            ("ReadFile", json!({"path": config})),
            ("ListFiles", json!({"path": ".."})),
            ("Grep", json!({"path": config, "query": "synthetic"})),
            (
                "WriteFile",
                json!({"path": root.path().join("created.txt"), "content": "created"}),
            ),
            ("Exec", json!({"command": "printf executed"})),
        ] {
            let output = runtime
                .tools
                .execute(
                    ToolCall::function(name, name, arguments),
                    CancellationToken::new(),
                )
                .await
                .unwrap();
            if name == "Exec" {
                let result: serde_json::Value = serde_json::from_str(&output.content).unwrap();
                assert_eq!(result["exit_code"], 0);
                assert_eq!(result["stdout"], "executed");
            }
        }
        assert_eq!(
            std::fs::read_to_string(root.path().join("created.txt")).unwrap(),
            "created"
        );
        sender
            .policy()
            .set_mode(ApprovalMode::Auto);
        let error = runtime
            .tools
            .execute(
                ToolCall::function("blocked", "ReadFile", json!({"path": config})),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::AuthorizationRequired(_)));
        // Read-only delegates use the same mode handle while retaining their tool ceiling.
        let readonly = workspace_tools(
            &project,
            &config,
            &root.path().join("auth"),
            None,
            None,
            Default::default(),
            Default::default(),
            Default::default(),
            false,
            None,
            None,
            None,
            sender.policy().clone(),
        )
        .await
        .unwrap();
        assert!(!readonly.tools.contains("WriteFile"));
        sender
            .policy()
            .set_mode(ApprovalMode::Yolo);
        readonly
            .tools
            .execute(
                ToolCall::function("read", "ReadFile", json!({"path": config})),
                CancellationToken::new(),
            )
            .await
            .unwrap();
    }
}
