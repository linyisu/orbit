use std::path::Path;
use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Debug)]
pub struct WritePreview {
    pub path: String,
    pub before: Option<String>,
    pub after: String,
}

#[derive(Clone, Debug)]
pub struct DeletePreview {
    pub path: String,
    pub before: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandWorkspaceAccess {
    #[default]
    Read,
    ReadWrite,
}

#[derive(Clone, Debug)]
pub struct CommandPreview {
    pub command: String,
    pub timeout_seconds: u64,
    pub workspace_access: CommandWorkspaceAccess,
    pub network_allowed: bool,
    pub external_capabilities: Vec<String>,
    /// The command can write outside the workspace through an existing grant.
    pub external_write: bool,
}

/// The file tool has already validated this path. Resolve its parent again for
/// the approval policy so a symlinked directory cannot make an outside write
/// look like a workspace-local edit.
pub fn workspace_local_file(path: &str, workspace: &Path) -> bool {
    let path = Path::new(path);
    let Some(parent) = path.parent() else {
        return false;
    };
    let Some(root) = workspace.canonicalize().ok() else {
        return false;
    };

    let candidate = if parent.is_absolute() {
        parent.to_path_buf()
    } else {
        root.join(parent)
    };

    candidate
        .canonicalize()
        .is_ok_and(|resolved| resolved.starts_with(root))
}

/// Auto mode permits read-only, offline commands and workspace-local file
/// edits. Shell commands with network, workspace writes, or external mounts
/// require approval because their effects cannot be inferred from the text.
pub fn auto_allows(preview: &ApprovalPreview, workspace: &Path) -> bool {
    match preview {
        ApprovalPreview::Command(command) => {
            !command.network_allowed
                && command.workspace_access == CommandWorkspaceAccess::Read
                && !command.external_write
                && command.external_capabilities.is_empty()
        }
        ApprovalPreview::Write(write) => workspace_local_file(&write.path, workspace),
        ApprovalPreview::Delete(delete) => workspace_local_file(&delete.path, workspace),
        ApprovalPreview::Mcp(_) | ApprovalPreview::Capability(_) => false,
    }
}

/// An MCP server is an external program with capabilities Orbit cannot
/// reliably infer from its advertised schema. Keep every invocation behind
/// the same explicit approval boundary as other side-effecting tools.
#[derive(Clone, Debug)]
pub struct McpPreview {
    pub server: String,
    pub tool: String,
    pub arguments: String,
}

#[derive(Clone, Debug)]
pub struct CapabilityPreview {
    pub environment: orbit_runtime::capability::EnvironmentId,
    pub path: std::path::PathBuf,
    pub access: orbit_runtime::capability::FilesystemAccess,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub enum ApprovalPreview {
    Write(WritePreview),
    Delete(DeletePreview),
    Command(CommandPreview),
    Mcp(McpPreview),
    Capability(CapabilityPreview),
}

pub struct ApprovalRequest {
    pub preview: ApprovalPreview,
    pub decision: oneshot::Sender<bool>,
}

pub type ApprovalSender = mpsc::Sender<ApprovalRequest>;

pub type ApprovalReceiver = mpsc::Receiver<ApprovalRequest>;

pub fn channel() -> (ApprovalSender, ApprovalReceiver) {
    mpsc::channel(1)
}

#[cfg(test)]
mod tests {
    use super::workspace_local_file;

    #[test]
    fn workspace_write_scope_resolves_parent_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();

        assert!(workspace_local_file("src/ui.rs", root.path()));
        assert!(workspace_local_file(
            root.path().join("src/ui.rs").to_str().unwrap(),
            root.path()
        ));
        assert!(!workspace_local_file("../outside.txt", root.path()));
        assert!(!workspace_local_file(
            outside.path().join("data.txt").to_str().unwrap(),
            root.path()
        ));

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();

            assert!(!workspace_local_file("linked/data.txt", root.path()));
        }
    }
}
