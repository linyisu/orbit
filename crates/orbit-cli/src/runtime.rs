use std::path::Path;

use anyhow::Result;
use orbit_agent::tool::ToolRegistry;
use orbit_tools::{read_file::ReadFile, workspace::Workspace};

pub fn workspace_tools(config: &Path) -> Result<ToolRegistry> {
    let mut workspace = Workspace::new(std::env::current_dir()?)?;
    if config.exists() {
        workspace.protect(config)?;
    }
    let mut tools = ToolRegistry::new();
    tools.register(ReadFile::new(workspace));
    Ok(tools)
}
