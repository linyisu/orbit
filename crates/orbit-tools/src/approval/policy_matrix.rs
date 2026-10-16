use super::*;

#[test]
fn auto_trusts_workspace_command_cwd_instead_of_shell_syntax() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = workspace.path();
    std::fs::create_dir(root.join("src")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("external")).unwrap();
    std::fs::write(root.join("file"), "text").unwrap();
    let mut preview = CommandPreview {
        command: String::new(),
        cwd: None,
        timeout_seconds: 30,
        workspace_access: CommandWorkspaceAccess::ReadWrite,
        network_allowed: true,
        external_capabilities: vec![],
        external_write: false,
        git: None,
    };
    for command in [
        "pwd",
        "ls",
        "rg pattern .",
        "cargo build",
        "python3 script.py",
        "npm install",
        "git push",
        "git reset --hard",
        "for file in *.txt; do cat \"$file\"; done",
    ] {
        preview.command = command.into();
        preview.git = orbit_runtime::git::GitCommandAnalysis::parse(command);
        for cwd in [None, Some("."), Some("src"), root.to_str()] {
            preview.cwd = cwd.map(str::to_owned);
            assert!(
                auto_allows(&ApprovalPreview::Command(preview.clone()), root),
                "{command} {cwd:?}"
            );
        }
    }
    for cwd in [
        Some(".."),
        Some("external"),
        Some("missing"),
        Some("file"),
        outside.path().to_str(),
    ] {
        preview.cwd = cwd.map(str::to_owned);
        assert!(
            !auto_allows(&ApprovalPreview::Command(preview.clone()), root),
            "{cwd:?}"
        );
    }
    // Previously granted capabilities do not turn every subsequent command into
    // another permission request. New grants are approved separately below.
    preview.cwd = None;
    preview.external_write = true;
    preview
        .external_capabilities
        .push("previously authorized path (rw)".into());
    assert!(auto_allows(&ApprovalPreview::Command(preview), root));
}

#[test]
fn auto_keeps_file_targets_mcp_and_new_capabilities_separate_from_command_trust() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = workspace.path();
    for (path, allowed) in [
        (root.join("file"), true),
        (outside.path().join("file"), false),
    ] {
        let path = path.to_str().unwrap().to_owned();
        for preview in [
            ApprovalPreview::Write(WritePreview {
                path: path.clone(),
                before: None,
                after: "text".into(),
            }),
            ApprovalPreview::Delete(DeletePreview {
                path,
                before: "text".into(),
            }),
        ] {
            assert_eq!(auto_allows(&preview, root), allowed);
        }
    }
    assert!(!auto_allows(
        &ApprovalPreview::Mcp(McpPreview {
            server: "files".into(),
            tool: "ReadFile".into(),
            arguments: "{}".into(),
        }),
        root
    ));
    assert!(!auto_allows(
        &ApprovalPreview::Capability(CapabilityPreview {
            environment: orbit_runtime::capability::EnvironmentId::Local,
            path: outside.path().into(),
            access: orbit_runtime::capability::FilesystemAccess::ReadWrite,
            reason: "requested external access".into(),
        }),
        root
    ));
}
