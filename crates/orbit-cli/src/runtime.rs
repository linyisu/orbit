use std::path::Path;

use anyhow::Result;
use orbit_agent::tool::ToolRegistry;
use orbit_tools::{read_file::ReadFile, workspace::Workspace};

pub fn workspace_tools(config: &Path, database: Option<&Path>) -> Result<ToolRegistry> {
    let mut workspace = Workspace::new(std::env::current_dir()?)?;
    if config.exists() {
        workspace.protect(config)?;
    }
    if let Some(database) = database {
        workspace.protect(database)?;
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut sidecar = database.as_os_str().to_os_string();
            sidecar.push(suffix);
            workspace.protect(std::path::PathBuf::from(sidecar))?;
        }
    }
    let mut tools = ToolRegistry::new();
    tools.register(ReadFile::new(workspace));
    Ok(tools)
}
