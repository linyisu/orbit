use std::path::PathBuf;

use crate::approval::{
    CapabilityPreview, CommandPreview, CommandWorkspaceAccess, DeletePreview, McpPreview,
    WritePreview,
};
use orbit_runtime::capability::{EnvironmentId, FilesystemAccess};

use super::*;

#[test]
fn auto_policy_matrix_for_files_commands_capabilities_and_mcp() {
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path();
    let cmd_read = ApprovalPreview::Command(CommandPreview {
        sandbox: true,
        native_workspace_scoped: false,
        command: "pwd".into(),
        cwd: None,
        timeout_seconds: 30,
        workspace_access: CommandWorkspaceAccess::Read,
        network_allowed: true,
        external_capabilities: Vec::new(),
        external_write: false,
        git: None,
    });
    let cmd_rw = ApprovalPreview::Command(CommandPreview {
        sandbox: true,
        native_workspace_scoped: false,
        command: "cargo build".into(),
        cwd: None,
        timeout_seconds: 30,
        workspace_access: CommandWorkspaceAccess::ReadWrite,
        network_allowed: true,
        external_capabilities: Vec::new(),
        external_write: false,
        git: None,
    });
    let write = ApprovalPreview::Write(WritePreview {
        path: "file".into(),
        before: None,
        after: "text".into(),
    });
    let delete = ApprovalPreview::Delete(DeletePreview {
        path: "file".into(),
        before: "text".into(),
    });
    let mcp = ApprovalPreview::Mcp(McpPreview {
        server: "files".into(),
        tool: "read_file".into(),
        arguments: r#"{\"path\":\"README.md\"}"#.into(),
    });
    let capability = ApprovalPreview::Capability(CapabilityPreview {
        environment: EnvironmentId::Local,
        path: PathBuf::from("/tmp/data"),
        access: FilesystemAccess::ReadWrite,
        reason: "testing".into(),
    });

    assert!(!auto_allows(&cmd_read, root));
    assert!(!auto_allows(&cmd_rw, root));

    let offline_read = ApprovalPreview::Command(CommandPreview {
        network_allowed: false,
        ..match cmd_read.clone() {
            ApprovalPreview::Command(command) => command,
            _ => unreachable!(),
        }
    });

    assert!(auto_allows(&offline_read, root));
    let mkdir = ApprovalPreview::Command(CommandPreview {
        sandbox: true,
        native_workspace_scoped: false,
        command: "mkdir -p demo".into(),
        workspace_access: CommandWorkspaceAccess::ReadWrite,
        ..match offline_read.clone() {
            ApprovalPreview::Command(command) => command,
            _ => unreachable!(),
        }
    });

    assert!(auto_allows(&mkdir, root));
    let text_write = ApprovalPreview::Command(CommandPreview {
        sandbox: true,
        native_workspace_scoped: false,
        command: "echo hello | tee demo.txt".into(),
        ..match mkdir.clone() {
            ApprovalPreview::Command(command) => command,
            _ => unreachable!(),
        }
    });

    assert!(auto_allows(&text_write, root));
    assert!(auto_allows(&write, root));
    assert!(auto_allows(&delete, root));
    assert!(!auto_allows(&mcp, root));
    assert!(!auto_allows(&capability, root));

    let outside = ApprovalPreview::Write(WritePreview {
        path: workspace
            .path()
            .parent()
            .unwrap()
            .join("outside.txt")
            .display()
            .to_string(),
        before: None,
        after: "text".into(),
    });

    assert!(!auto_allows(&outside, root));

    let external_command = ApprovalPreview::Command(CommandPreview {
        sandbox: true,
        native_workspace_scoped: false,
        command: "write outside".into(),
        cwd: None,
        timeout_seconds: 30,
        workspace_access: CommandWorkspaceAccess::ReadWrite,
        network_allowed: true,
        external_capabilities: Vec::new(),
        external_write: true,
        git: None,
    });

    assert!(!auto_allows(&external_command, root));

    let git_status = ApprovalPreview::Command(CommandPreview {
        sandbox: true,
        native_workspace_scoped: false,
        command: "git status".into(),
        cwd: None,
        timeout_seconds: 30,
        workspace_access: CommandWorkspaceAccess::Read,
        network_allowed: false,
        external_capabilities: Vec::new(),
        external_write: false,
        git: orbit_runtime::git::GitCommandAnalysis::parse("git status"),
    });

    assert!(auto_allows(&git_status, root));

    let git_push = ApprovalPreview::Command(CommandPreview {
        sandbox: true,
        native_workspace_scoped: false,
        command: "git push origin main".into(),
        cwd: None,
        timeout_seconds: 30,
        workspace_access: CommandWorkspaceAccess::ReadWrite,
        network_allowed: true,
        external_capabilities: Vec::new(),
        external_write: false,
        git: orbit_runtime::git::GitCommandAnalysis::parse("git push origin main"),
    });

    assert!(!auto_allows(&git_push, root));

    let git_reset_hard = ApprovalPreview::Command(CommandPreview {
        sandbox: true,
        native_workspace_scoped: false,
        command: "git reset --hard".into(),
        cwd: None,
        timeout_seconds: 30,
        workspace_access: CommandWorkspaceAccess::ReadWrite,
        network_allowed: false,
        external_capabilities: Vec::new(),
        external_write: false,
        git: orbit_runtime::git::GitCommandAnalysis::parse("git reset --hard"),
    });

    assert!(!auto_allows(&git_reset_hard, root));
}
