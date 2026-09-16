use std::path::Path;

use anyhow::Result;
use orbit_agent::tool::ToolRegistry;
use orbit_tools::{
    approval::ApprovalSender, edit_file::EditFile, read_file::ReadFile, run_command::RunCommand,
    workspace::Workspace, write_file::WriteFile,
};

pub fn workspace_tools(
    config: &Path,
    database: Option<&Path>,
    approvals: Option<ApprovalSender>,
) -> Result<ToolRegistry> {
    let mut workspace = Workspace::new(std::env::current_dir()?)?;
    if config.exists() {
        workspace.protect(config)?;
    }
    if let Some(database) = database {
        workspace.protect(database)?;
        workspace.protect(database.with_extension("session-locks"))?;
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut sidecar = database.as_os_str().to_os_string();
            sidecar.push(suffix);
            workspace.protect(std::path::PathBuf::from(sidecar))?;
        }
    }
    let mut tools = ToolRegistry::new();
    tools.register(ReadFile::new(workspace.clone()));
    if let Some(approvals) = approvals {
        tools.register(WriteFile::new(workspace.clone(), approvals.clone()));
        tools.register(EditFile::new(workspace.clone(), approvals.clone()));
        tools.register(RunCommand::new(workspace, approvals));
    }
    Ok(tools)
}
