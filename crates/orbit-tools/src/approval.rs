mod commands;

pub(crate) use commands::{local_workspace_command, pinned_local_command};

use std::path::Path;

use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct WritePreview {
    pub path: String,
    pub before: Option<String>,
    pub after: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CommandPreview {
    #[serde(default = "sandbox_default")]
    pub sandbox: bool,
    pub command: String,
    pub cwd: Option<String>,
    pub timeout_seconds: u64,
    pub workspace_access: CommandWorkspaceAccess,
    pub network_allowed: bool,
    pub external_capabilities: Vec<String>,
    /// The command can write outside the workspace through an existing grant.
    pub external_write: bool,
    pub git: Option<orbit_runtime::git::GitCommandAnalysis>,
}

fn sandbox_default() -> bool {
    true
}

impl CommandPreview {
    pub fn request_grant_key(&self, workspace: &Path) -> String {
        format!(
            "{}:{:?}:{}:{:?}:{}:{}:{:?}:{}",
            workspace.display(),
            self.cwd,
            self.command,
            self.workspace_access,
            self.network_allowed,
            self.external_write,
            self.external_capabilities,
            self.sandbox
        )
    }

    /// Bind reuse to the exact approved request and repository configuration.
    pub fn git_grant_key(&self, workspace: &Path) -> Option<String> {
        use std::io::Read;
        let root = workspace.canonicalize().ok()?;
        let cwd = self.cwd.as_deref().map(Path::new).unwrap_or(workspace);
        let cwd = if cwd.is_absolute() {
            cwd.to_owned()
        } else {
            root.join(cwd)
        };
        let repo = cwd
            .ancestors()
            .take_while(|path| path.starts_with(&root))
            .find_map(|path| {
                orbit_runtime::git::GitRepositoryInfo::discover(path)
                    .ok()
                    .flatten()
            })?;
        let mut bytes = Vec::new();
        for config in [repo.config_path(), repo.worktree_config_path()] {
            if config.exists() {
                use std::os::unix::fs::MetadataExt;
                let metadata = std::fs::symlink_metadata(&config).ok()?;
                if metadata.file_type().is_symlink() || metadata.nlink() != 1 {
                    return None;
                }
                std::fs::File::open(config)
                    .ok()?
                    .take(1024 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .ok()?;
                if bytes.len() > 1024 * 1024 {
                    return None;
                }
            }
        }

        Some(format!(
            "{}:{:?}:{}:{:?}:{}:{:?}:{}:{}",
            repo.git_dir.display(),
            self.cwd,
            self.command,
            self.workspace_access,
            self.network_allowed,
            self.external_capabilities,
            orbit_runtime::execution::content_hash(&bytes),
            self.sandbox
        ))
    }
}

/// Bind reusable file approvals to the actual target, including final symlinks.
pub fn file_grant_key(path: &str, workspace: &Path) -> Option<String> {
    let path = Path::new(path);
    let requested = if path.is_absolute() {
        path.to_owned()
    } else {
        workspace.join(path)
    };
    let canonical = match requested.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => requested
            .parent()?
            .canonicalize()
            .ok()?
            .join(requested.file_name()?),

        Err(_) => return None,
    };
    Some(canonical.to_str()?.to_owned())
}

pub fn workspace_local_file(path: &str, workspace: &Path) -> bool {
    let Some(root) = workspace.canonicalize().ok() else {
        return false;
    };
    file_grant_key(path, &root).is_some_and(|path| Path::new(&path).starts_with(root))
}

/// Auto permits scoped workspace operations and offline read-only commands.
/// Network access, external writes and other shell scripts require approval.
pub fn auto_allows(preview: &ApprovalPreview, workspace: &Path) -> bool {
    match preview {
        ApprovalPreview::Command(command) => {
            if command.git.as_ref().is_some_and(|git| {
                matches!(
                    git.kind,
                    orbit_runtime::git::GitOperationKind::Dangerous { .. }
                        | orbit_runtime::git::GitOperationKind::ConfigMutation { .. }
                )
            }) {
                return false;
            }

            !command.network_allowed
                && !command.external_write
                && ((command.sandbox
                    && command.workspace_access == CommandWorkspaceAccess::Read
                    && command.external_capabilities.is_empty())
                    || local_workspace_command(&command.command, command.cwd.as_deref(), workspace)
                        .is_some())
        }

        ApprovalPreview::Write(write) => workspace_local_file(&write.path, workspace),
        ApprovalPreview::Delete(delete) => workspace_local_file(&delete.path, workspace),
        ApprovalPreview::Mcp(_) | ApprovalPreview::Capability(_) => false,
    }
}

/// An MCP server is an external program with capabilities Orbit cannot
/// reliably infer from its advertised schema. Keep every invocation behind
/// the same explicit approval boundary as other side-effecting tools.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct McpPreview {
    pub server: String,
    pub tool: String,
    pub arguments: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct CapabilityPreview {
    pub environment: orbit_runtime::capability::EnvironmentId,
    pub path: std::path::PathBuf,
    pub access: orbit_runtime::capability::FilesystemAccess,
    pub reason: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
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
    use super::*;

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

    #[test]
    fn git_query_does_not_override_network_or_filesystem_permissions() {
        let root = tempfile::tempdir().unwrap();
        let mut command = CommandPreview {
            sandbox: true,
            command: "git status".into(),
            cwd: None,
            timeout_seconds: 30,
            workspace_access: CommandWorkspaceAccess::Read,
            network_allowed: false,
            external_write: false,
            external_capabilities: Vec::new(),
            git: orbit_runtime::git::GitCommandAnalysis::parse("git status"),
        };

        assert!(auto_allows(
            &ApprovalPreview::Command(command.clone()),
            root.path()
        ));
        command.network_allowed = true;
        assert!(!auto_allows(
            &ApprovalPreview::Command(command.clone()),
            root.path()
        ));
        command.network_allowed = false;
        command.workspace_access = CommandWorkspaceAccess::ReadWrite;
        assert!(!auto_allows(
            &ApprovalPreview::Command(command.clone()),
            root.path()
        ));
        command.workspace_access = CommandWorkspaceAccess::Read;
        command.external_capabilities.push("outside".into());
        assert!(!auto_allows(
            &ApprovalPreview::Command(command),
            root.path()
        ));
    }

    #[test]
    fn auto_directory_creation_checks_targets_and_keeps_other_permissions() {
        let root = tempfile::tempdir().unwrap();
        let mut command = CommandPreview {
            sandbox: true,
            command: "mkdir -p demo/nested".into(),
            cwd: None,
            timeout_seconds: 30,
            workspace_access: CommandWorkspaceAccess::ReadWrite,
            network_allowed: false,
            external_capabilities: Vec::new(),
            external_write: false,
            git: None,
        };
        let allowed = |command: &CommandPreview| {
            auto_allows(&ApprovalPreview::Command(command.clone()), root.path())
        };
        assert!(allowed(&command));
        command
            .external_capabilities
            .push("cache (ro, dependency cache)".into());
        assert!(allowed(&command));
        command.external_write = true;
        assert!(!allowed(&command));
        command.external_write = false;
        command.network_allowed = true;
        assert!(!allowed(&command));
        command.network_allowed = false;
        for script in [
            "mkdir -p demo && touch file",
            "mkdir -p /tmp/outside",
            "cargo build",
        ] {
            command.command = script.into();
            assert!(!allowed(&command), "{script}");
        }
    }

    #[test]
    fn final_link_target_controls_local_write_and_grant_reuse() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("inside"), "inside").unwrap();
        std::fs::write(outside.path().join("external"), "outside").unwrap();
        let alias = workspace.path().join("alias");
        std::os::unix::fs::symlink("inside", &alias).unwrap();
        let initial = file_grant_key("alias", workspace.path()).unwrap();
        assert!(workspace_local_file("alias", workspace.path()));
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(outside.path().join("external"), &alias).unwrap();
        assert!(!workspace_local_file("alias", workspace.path()));
        assert_ne!(file_grant_key("alias", workspace.path()).unwrap(), initial);
    }
}
