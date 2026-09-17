use std::path::Path;

use anyhow::Result;
use orbit_agent::tool::ToolRegistry;
use orbit_tools::{
    approval::ApprovalSender, edit_file::EditFile, list_files::ListFiles, read_file::ReadFile,
    run_command::RunCommand, workspace::Workspace, write_file::WriteFile,
};

pub fn workspace_tools(
    config: &Path,
    database: Option<&Path>,
    approvals: Option<ApprovalSender>,
    command_limits: orbit_tools::run_command::CommandLimits,
    web: orbit_tools::web_fetch::WebConfig,
    scratch: Option<orbit_tools::run_command::ScratchDirectory>,
) -> Result<ToolRegistry> {
    let mut workspace = Workspace::new(std::env::current_dir()?)?;
    if config.exists() {
        workspace.protect(config)?;
    }
    if let Some(database) = database {
        workspace.protect(database)?;
        workspace.protect(database.with_extension("session-locks"))?;
        workspace.protect(database.with_extension("workspaces"))?;
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut sidecar = database.as_os_str().to_os_string();
            sidecar.push(suffix);
            workspace.protect(std::path::PathBuf::from(sidecar))?;
        }
    }
    let mut tools = ToolRegistry::new();
    tools.register(ReadFile::new(workspace.clone()));
    tools.register(ListFiles::new(workspace.clone()));
    tools.register(orbit_tools::web_fetch::WebFetch::new(web)?);
    if let Some(approvals) = approvals {
        tools.register(WriteFile::new(workspace.clone(), approvals.clone()));
        tools.register(EditFile::new(workspace.clone(), approvals.clone()));
        let command = RunCommand::with_limits(workspace, approvals, command_limits);
        tools.register(if let Some(scratch) = scratch {
            command.with_scratch(scratch)
        } else {
            command
        });
    }
    Ok(tools)
}
