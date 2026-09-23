use std::path::Path;

use anyhow::Result;
use orbit_agent::tool::ToolRegistry;
use orbit_runtime::filesystem::Workspace;
use orbit_tools::{
    approval::ApprovalSender, edit_file::EditFile, git_inspect::GitInspect,
    image_assets::ImageAssets, list_files::ListFiles, read_command_output::ReadCommandOutput,
    read_document::ReadDocument, read_file::ReadFile, read_image::ReadImage,
    request_capability::RequestCapability, run_command::RunCommand, search_text::SearchText,
    write_file::WriteFile,
};

pub struct WorkspaceRuntime {
    pub tools: ToolRegistry,
    pub workspace: Workspace,
    pub command_limits: orbit_tools::run_command::CommandLimits,
    pub mcp_statuses: Vec<orbit_tools::mcp::McpServerStatus>,
}

#[allow(clippy::too_many_arguments)]
pub async fn workspace_tools(
    config: &Path,
    database: Option<&Path>,
    approvals: Option<ApprovalSender>,
    command_limits: orbit_tools::run_command::CommandLimits,
    web: orbit_tools::web_fetch::WebConfig,
    mcp: orbit_tools::mcp::McpConfig,
    scratch: Option<orbit_tools::run_command::ScratchDirectory>,
    command_output: Option<orbit_tools::run_command::CommandOutputDirectory>,
    permissions: Option<orbit_runtime::policy::WorkspacePermissions>,
) -> Result<WorkspaceRuntime> {
    let workspace = protected_workspace(config, database, permissions)?;
    let mut tools = ToolRegistry::new();
    tools.register(ReadFile::new(workspace.clone()));
    tools.register(ReadDocument::new(workspace.clone()));
    if let Some(database) = database {
        tools.register(ReadImage::new(
            workspace.clone(),
            ImageAssets::for_database(database),
        ));
    }
    tools.register(ListFiles::new(workspace.clone()));
    tools.register(SearchText::new(workspace.clone()));
    tools.register(orbit_tools::project_overview::ProjectOverview::new(
        workspace.clone(),
    ));
    tools.register(GitInspect::new(workspace.clone()));
    if let Some(database) = database {
        tools.register(orbit_tools::git_export_patch::GitExportPatch::new(
            workspace.clone(),
            database.with_extension("artifacts"),
        ));
    }
    tools.register(orbit_tools::web_fetch::WebFetch::new(web.clone())?);
    tools.register(orbit_tools::web_fetch::WebSearch::new(web)?);
    let mcp_approvals = approvals.clone();
    if let Some(approvals) = approvals {
        tools.register(WriteFile::new(workspace.clone(), approvals.clone()));
        tools.register(EditFile::new(workspace.clone(), approvals.clone()));
        tools.register(RequestCapability::new(workspace.clone(), approvals.clone()));
        let command = RunCommand::with_limits(workspace.clone(), approvals, command_limits.clone());
        let command = if let Some(scratch) = scratch {
            if let Some(database) = database {
                tools.register(orbit_tools::export_artifact::ExportArtifact::new(
                    scratch.clone(),
                    database.with_extension("artifacts"),
                ));
            }
            command.with_scratch(scratch)
        } else {
            command
        };
        if let Some(command_output) = command_output {
            tools.register(ReadCommandOutput::new(command_output.clone()));
            tools.register(command.with_output_directory(command_output));
        } else {
            tools.register(command);
        }
    }
    let mcp_statuses =
        orbit_tools::mcp::register(&mut tools, &mcp, workspace.root(), mcp_approvals).await;
    Ok(WorkspaceRuntime {
        workspace,
        command_limits,
        tools,
        mcp_statuses,
    })
}

pub fn workspace_writer(
    config: &Path,
    database: &Path,
    approvals: ApprovalSender,
    permissions: Option<orbit_runtime::policy::WorkspacePermissions>,
) -> Result<WriteFile> {
    Ok(WriteFile::new(
        protected_workspace(config, Some(database), permissions)?,
        approvals,
    ))
}

fn protected_workspace(
    config: &Path,
    database: Option<&Path>,
    permissions: Option<orbit_runtime::policy::WorkspacePermissions>,
) -> Result<Workspace> {
    let mut workspace = Workspace::new(std::env::current_dir()?)?;
    workspace.protect(config)?;
    if let Some(database) = database {
        workspace.protect(database)?;
        workspace.protect_opaque_runtime_root(database.with_extension("session-locks"))?;
        workspace.protect_opaque_runtime_root(database.with_extension("workspaces"))?;
        workspace.protect_opaque_runtime_root(database.with_extension("command-output"))?;
        workspace.protect_opaque_runtime_root(database.with_extension("artifacts"))?;
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut sidecar = database.as_os_str().to_os_string();
            sidecar.push(suffix);
            workspace.protect(std::path::PathBuf::from(sidecar))?;
        }
    }
    if let Some(perms) = permissions {
        workspace.configure_policy(perms, orbit_runtime::capability::NetworkPolicy::Allow)?;
    }
    Ok(workspace)
}
