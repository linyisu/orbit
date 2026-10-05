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
    config: &Path,
    auth_directory: &Path,
    database: Option<&Path>,
    approvals: Option<ApprovalSender>,
    command_limits: orbit_tools::run_command::CommandLimits,
    web: orbit_tools::web_fetch::WebConfig,
    mcp: orbit_tools::mcp::McpConfig,
    allow_mcp_start: bool,
    scratch: Option<orbit_tools::run_command::ScratchDirectory>,
    command_output: Option<orbit_tools::run_command::CommandOutputDirectory>,
    permissions: Option<orbit_runtime::policy::WorkspacePermissions>,
) -> Result<WorkspaceRuntime> {
    let workspace = protected_workspace(
        directory,
        config,
        auth_directory,
        database,
        permissions,
        command_limits.network,
    )?;
    let mut tools = ToolRegistry::new();
    tools.register(
        ReadFile::new(workspace.clone()).with_scan_budget(command_limits.read_file_scan_bytes),
    );
    tools.register(ReadDocument::new(workspace.clone()).with_worker(std::env::current_exe()?));

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
    // Model-facing Git commands use run_command; GitInspect supplies internal snapshots.

    if let Some(database) = database {
        tools.register(orbit_tools::git_export_patch::GitExportPatch::new(
            workspace.clone(),
            database.with_extension("artifacts"),
        ));
    }

    let web_client = std::sync::Arc::new(orbit_tools::web_fetch::WebClient::new(&web)?);
    tools.register(orbit_tools::web_fetch::WebFetch::with_client(
        (*web_client).clone(),
    ));
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

        tools.register(
            orbit_tools::git_push::GitPush::new(workspace.clone(), approvals.clone())
                .with_limits(&command_limits),
        );
        tools.register(WriteFile::new(workspace.clone(), approvals.clone()));
        tools.register(EditFile::new(workspace.clone(), approvals.clone()));
        tools.register(RequestCapability::new(workspace.clone(), approvals.clone()));
        let command = RunCommand::with_limits(workspace.clone(), approvals, command_limits.clone())
            .with_background_jobs(jobs);
        let command = if let Some(scratch) = scratch {
            if let Some(database) = database {
                tools.register(
                    orbit_tools::export_artifact::ExportArtifact::new(
                        scratch.clone(),
                        database.with_extension("artifacts"),
                    )
                    .with_workspace(workspace.clone())
                    .with_limits(command_limits.artifacts.clone()),
                );
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
    config: &Path,
    auth_directory: &Path,
    database: &Path,
    approvals: ApprovalSender,
    permissions: Option<orbit_runtime::policy::WorkspacePermissions>,
) -> Result<WriteFile> {
    Ok(WriteFile::new(
        protected_workspace(
            directory,
            config,
            auth_directory,
            Some(database),
            permissions,
            orbit_runtime::capability::NetworkPolicy::Allow,
        )?,
        approvals,
    ))
}

fn protected_workspace(
    directory: &Path,
    config: &Path,
    auth_directory: &Path,
    database: Option<&Path>,
    permissions: Option<orbit_runtime::policy::WorkspacePermissions>,
    network: orbit_runtime::capability::NetworkPolicy,
) -> Result<Workspace> {
    let mut workspace = Workspace::new(directory)?;
    workspace.protect(config)?;
    workspace.protect(auth_directory)?;

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

    workspace.configure_policy(permissions.unwrap_or_default(), network)?;

    Ok(workspace)
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    #[test]
    fn first_launch_protects_credentials_with_missing_parent_directories() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();

        for auth in [
            root.path().join("home/.config/orbit/auth"),
            project.join("private/config/orbit/auth"),
        ] {
            let workspace = protected_workspace(
                &project,
                &project.join("config.toml"),
                &auth,
                None,
                None,
                orbit_runtime::capability::NetworkPolicy::Allow,
            )
            .unwrap();

            assert!(
                !auth.exists(),
                "startup must not create credential directories"
            );
            assert!(workspace.protected().contains(&auth));

            std::fs::create_dir_all(&auth).unwrap();
            std::fs::write(auth.join("test.json"), "secret").unwrap();

            assert!(workspace.authorize_read(&auth.join("test.json")).is_err());
            if let Ok(relative) = auth.strip_prefix(&project) {
                assert!(workspace.open_read(&relative.join("test.json")).is_err());
                assert!(workspace.write_parent(&relative.join("test.json")).is_err());
            }
        }
    }

    #[test]
    fn credential_directory_is_protected_inside_a_project() {
        let root = tempfile::tempdir().unwrap();
        let auth = root.path().join("auth");
        std::fs::create_dir(&auth).unwrap();
        std::fs::write(auth.join("test.json"), "secret").unwrap();
        let workspace = protected_workspace(
            root.path(),
            &root.path().join("config.toml"),
            &auth,
            None,
            None,
            orbit_runtime::capability::NetworkPolicy::Allow,
        )
        .unwrap();

        let read = workspace.open_read(Path::new("auth/test.json"));
        let write = workspace.write_parent(Path::new("auth/test.json"));

        assert!(read.is_err());
        assert!(write.is_err());
        assert!(workspace.protected().contains(&auth));
    }
}
