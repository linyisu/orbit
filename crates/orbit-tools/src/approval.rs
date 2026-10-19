use std::path::Path;

pub use orbit_runtime::policy::{ApprovalMode, ApprovalPolicy};
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

impl CommandPreview {
    pub fn request_grant_key(&self, workspace: &Path) -> String {
        format!(
            "{}:{:?}:{}:{:?}:{}:{}:{:?}",
            workspace.display(),
            self.cwd,
            self.command,
            self.workspace_access,
            self.network_allowed,
            self.external_write,
            self.external_capabilities
        )
    }

    /// Bind reuse to the exact approved request and repository configuration.
    pub fn git_grant_key(&self, workspace: &Path) -> Option<String> {
        use std::io::Read;
        let root = workspace.canonicalize().ok()?;
        let cwd = self
            .cwd
            .as_deref()
            .map(Path::new)
            .unwrap_or(workspace);
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
                let file = orbit_runtime::filesystem::open_regular_file(&config).ok()?;
                if orbit_runtime::filesystem::hard_link_count(&file).ok()? != 1 {
                    return None;
                }
                file.take(1_024 * 1_024 + 1)
                    .read_to_end(&mut bytes)
                    .ok()?;
                if bytes.len() > 1_024 * 1_024 {
                    return None;
                }
            }
        }

        Some(format!(
            "{}:{:?}:{}:{:?}:{}:{:?}:{}",
            repo.git_dir.display(),
            self.cwd,
            self.command,
            self.workspace_access,
            self.network_allowed,
            self.external_capabilities,
            orbit_runtime::execution::content_hash(&bytes)
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

/// Auto trusts commands started in the workspace, including networked scripts.
/// This is an approval policy, not shell analysis or filesystem isolation;
/// approved commands execute with the target user’s ordinary OS access.
pub fn auto_allows(preview: &ApprovalPreview, workspace: &Path) -> bool {
    match preview {
        ApprovalPreview::Command(command) => {
            let Ok(root) = workspace.canonicalize() else {
                return false;
            };
            root.join(command.cwd.as_deref().unwrap_or("."))
                .canonicalize()
                .is_ok_and(|cwd| cwd.is_dir() && cwd.starts_with(root))
        }

        ApprovalPreview::Write(write) => workspace_local_file(&write.path, workspace),
        ApprovalPreview::Delete(delete) => workspace_local_file(&delete.path, workspace),
        ApprovalPreview::Mcp(_) => true,
        ApprovalPreview::Capability(_) => false,
    }
}

/// MCP invocations follow the same session approval mode as built-in tools.
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

#[derive(Debug)]
pub struct ApprovalRequest {
    pub preview: ApprovalPreview,
    pub decision: oneshot::Sender<bool>,
}

#[derive(Clone)]
pub struct ApprovalSender {
    sender: mpsc::Sender<ApprovalRequest>,
    policy: ApprovalPolicy,
    workspace: Option<std::sync::Arc<std::path::PathBuf>>,
}

impl ApprovalSender {
    /// Bind Auto decisions to this executor's workspace while sharing live mode changes.
    pub fn for_workspace(&self, workspace: &Path) -> Self {
        Self {
            workspace: Some(std::sync::Arc::new(workspace.to_owned())),
            ..self.clone()
        }
    }

    pub fn policy(&self) -> &ApprovalPolicy {
        &self.policy
    }

    /// Automatically allowed requests never enter the actor's approval queue.
    pub async fn send(
        &self,
        request: ApprovalRequest,
    ) -> Result<(), Box<mpsc::error::SendError<ApprovalRequest>>> {
        let allowed = match self.policy.mode() {
            ApprovalMode::Yolo => true,
            ApprovalMode::Auto => self
                .workspace
                .as_deref()
                .is_some_and(|root| auto_allows(&request.preview, root)),
            ApprovalMode::Ask => false,
        };
        if allowed {
            let _ = request.decision.send(true);
            Ok(())
        } else {
            self.sender
                .send(request)
                .await
                .map_err(Box::new)
        }
    }
}

pub type ApprovalReceiver = mpsc::Receiver<ApprovalRequest>;

pub fn channel() -> (ApprovalSender, ApprovalReceiver) {
    let (sender, receiver) = mpsc::channel(1);
    (
        ApprovalSender {
            sender,
            policy: ApprovalPolicy::default(),
            workspace: None,
        },
        receiver,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::{symlink as symlink_dir, symlink as symlink_file};
    #[cfg(windows)]
    use std::os::windows::fs::{symlink_dir, symlink_file};

    #[tokio::test]
    async fn live_modes_bypass_the_queue_only_for_automatically_allowed_operations() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let (sender, mut receiver) = channel();
        let sender = sender.for_workspace(root.path());
        let cloned = sender.clone();
        for (mode, preview, automatic) in [
            (
                ApprovalMode::Ask,
                ApprovalPreview::Mcp(McpPreview {
                    server: "fixture".into(),
                    tool: "Inspect".into(),
                    arguments: "{}".into(),
                }),
                false,
            ),
            (
                ApprovalMode::Auto,
                ApprovalPreview::Mcp(McpPreview {
                    server: "fixture".into(),
                    tool: "Inspect".into(),
                    arguments: "{}".into(),
                }),
                true,
            ),
            (
                ApprovalMode::Yolo,
                ApprovalPreview::Write(WritePreview {
                    path: outside
                        .path()
                        .join("config.toml")
                        .display()
                        .to_string(),
                    before: None,
                    after: "test".into(),
                }),
                true,
            ),
            (
                ApprovalMode::Auto,
                ApprovalPreview::Write(WritePreview {
                    path: outside
                        .path()
                        .join("config.toml")
                        .display()
                        .to_string(),
                    before: None,
                    after: "test".into(),
                }),
                false,
            ),
        ] {
            sender.policy().set_mode(mode);
            let (decision, answer) = oneshot::channel();
            cloned
                .send(ApprovalRequest { preview, decision })
                .await
                .unwrap();
            if automatic {
                assert!(receiver.try_recv().is_err());
            } else {
                receiver
                    .recv()
                    .await
                    .unwrap()
                    .decision
                    .send(true)
                    .unwrap();
            }
            assert!(answer.await.unwrap());
        }
        drop(receiver);
        sender
            .policy()
            .set_mode(ApprovalMode::Yolo);
        let (decision, answer) = oneshot::channel();
        sender
            .send(ApprovalRequest {
                preview: ApprovalPreview::Mcp(McpPreview {
                    server: "fixture".into(),
                    tool: "Inspect".into(),
                    arguments: "{}".into(),
                }),
                decision,
            })
            .await
            .unwrap();
        assert!(answer.await.unwrap());
    }

    #[test]
    fn workspace_write_scope_resolves_parent_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();

        assert!(workspace_local_file("src/ui.rs", root.path()));
        assert!(workspace_local_file(
            root.path()
                .join("src/ui.rs")
                .to_str()
                .unwrap(),
            root.path()
        ));
        assert!(!workspace_local_file("../outside.txt", root.path()));
        assert!(!workspace_local_file(
            outside
                .path()
                .join("data.txt")
                .to_str()
                .unwrap(),
            root.path()
        ));

        symlink_dir(outside.path(), root.path().join("linked")).unwrap();
        assert!(!workspace_local_file("linked/data.txt", root.path()));
    }

    #[test]
    fn final_link_target_controls_local_write_and_grant_reuse() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("inside"), "inside").unwrap();
        std::fs::write(outside.path().join("external"), "outside").unwrap();
        let alias = workspace.path().join("alias");
        symlink_file("inside", &alias).unwrap();
        let initial = file_grant_key("alias", workspace.path()).unwrap();
        assert!(workspace_local_file("alias", workspace.path()));
        std::fs::remove_file(&alias).unwrap();
        symlink_file(outside.path().join("external"), &alias).unwrap();
        assert!(!workspace_local_file("alias", workspace.path()));
        assert_ne!(file_grant_key("alias", workspace.path()).unwrap(), initial);
    }
}

#[cfg(test)]
mod policy_matrix;
