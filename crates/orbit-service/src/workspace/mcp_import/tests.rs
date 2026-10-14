use super::*;
mod session;

#[test]
fn node_cli_uses_registry_stdio_arguments_without_running_source() {
    let (_root, _workspace, source) = fixture();
    fs::write(
        source.join("package.json"),
        r#"{"name":"fixture-tools","bin":{"fixture":"scripts/fixture.mjs","fixture-tools":"scripts/fixture.mjs"}}"#,
    )
    .unwrap();
    fs::write(source.join("server.json"), r#"{"packages":[{"registryType":"npm","identifier":"other","transport":{"type":"stdio"},"packageArguments":[{"type":"positional","value":"wrong"}]},{"registryType":"npm","identifier":"fixture-tools","transport":{"type":"stdio"},"packageArguments":[{"type":"positional","value":"mcp"}]}]}"#).unwrap();
    let entries = discover::discover(&source).unwrap();
    assert_eq!(entries.len(), 2);
    for entry in entries {
        assert!(entry.launch.args[0].ends_with("scripts/fixture.mjs"));
        assert_eq!(&entry.launch.args[1..], &["mcp"]);
        assert!(entry.description.contains("server.json"));
    }
    fs::write(source.join("server.json"), r#"{"packages":[{"registryType":"npm","identifier":"fixture-tools","transport":{"type":"streamable-http"},"packageArguments":[{"type":"positional","value":"mcp"}]}]}"#).unwrap();
    assert!(
        discover::discover(&source)
            .unwrap()
            .iter()
            .all(|entry| entry.launch.args.len() == 1)
    );
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let source = root.path().join("server");
    fs::create_dir(&workspace).unwrap();
    fs::create_dir(&source).unwrap();
    (root, workspace, source)
}
#[tokio::test]
async fn discovers_manifest_and_python_without_executing_source() {
    let (root, workspace, source) = fixture();
    fs::write(source.join(".mcp.json"), r#"{"mcpServers":{"echo":{"command":"python3","args":["./server.py"],"env":{"API_TOKEN":"fill-me"}},"remote":{"url":"https://example.com/mcp"}}}"#).unwrap();
    fs::write(
        source.join("server.py"),
        "raise Exception('must not run during scan')",
    )
    .unwrap();
    fs::write(
        source.join("pyproject.toml"),
        "[project]\nname='demo'\n[project.scripts]\ndemo='demo:main'\n",
    )
    .unwrap();
    let scan = Scan::acquire(
        "session".into(),
        workspace,
        source.to_string_lossy().into(),
        Some(root.path().into()),
    )
    .await
    .unwrap();
    assert_eq!(scan.candidates.len(), 2);
    let manifest = scan
        .candidates
        .iter()
        .find(|candidate| candidate.id.contains(".mcp.json"))
        .unwrap();
    assert!(Path::new(&manifest.launch.args[0]).is_absolute());
    assert_eq!(manifest.launch.env["API_TOKEN"], "fill-me");
    let python = scan
        .candidates
        .iter()
        .find(|candidate| candidate.id.contains("python:"))
        .unwrap();
    assert_eq!(python.install.len(), 2);
    assert!(!Path::new(&python.launch.command).exists());
}
#[tokio::test]
async fn installation_requires_exact_token_session_workspace_and_entry() {
    for invalid in ["token", "session", "workspace", "entry"] {
        let (root, workspace, source) = fixture();
        fs::write(
            source.join(".mcp.json"),
            r#"{"mcpServers":{"echo":{"command":"/bin/echo"}}}"#,
        )
        .unwrap();
        let scan = Scan::acquire(
            "session".into(),
            workspace.clone(),
            source.to_string_lossy().into(),
            Some(root.path().into()),
        )
        .await
        .unwrap();
        let token = if invalid == "token" {
            "forged".into()
        } else {
            scan.token()
        };
        let id = if invalid == "entry" {
            "./../../outside|bad".into()
        } else {
            scan.candidates[0].id.clone()
        };
        let session = if invalid == "session" {
            "another"
        } else {
            "session"
        };
        let workspace = if invalid == "workspace" {
            root.path()
        } else {
            &workspace
        };
        assert!(
            scan.install(&token, &id, session, workspace).await.is_err(),
            "{invalid}"
        );
    }
}
#[tokio::test]
async fn reviewed_node_build_is_installed_in_a_persistent_private_copy() {
    let (root, workspace, source) = fixture();
    fs::write(source.join("package.json"), r#"{"name":"demo-mcp","version":"1.0.0","bin":{"demo":"dist/server.js"},"scripts":{"build":"sh build.sh"}}"#).unwrap();
    fs::write(
        source.join("build.sh"),
        "mkdir dist; printf fixture > dist/server.js",
    )
    .unwrap();
    let mut scan = Scan::acquire(
        "session".into(),
        workspace.clone(),
        source.to_string_lossy().into(),
        Some(root.path().into()),
    )
    .await
    .unwrap();
    let entry = &scan.candidates[0];
    assert_eq!(entry.install.len(), 2);
    assert!(entry.install[0].contains(&"--ignore-scripts".into()));
    let built = PathBuf::from(&entry.launch.args[0]);
    assert!(!built.exists());
    let token = scan.token();
    let id = entry.id.clone();
    // Exercise the reviewed installer with a hermetic build command. Native QA
    // separately runs npm; unit tests must not require Node or network access.
    scan.candidates[0].install = vec![vec!["/bin/sh".into(), "build.sh".into()]];
    fs::write(
        source.join("build.sh"),
        "throw new Error('later source edits must not replace reviewed scripts');",
    )
    .unwrap();
    let result = scan
        .install(&token, &id, "session", &workspace)
        .await
        .unwrap();
    assert!(matches!(result, Snapshot::McpInstalled { .. }));
    assert_eq!(fs::read_to_string(built).unwrap(), "fixture");
    assert!(!source.join("dist").exists());
}
#[cfg(unix)]
#[tokio::test]
async fn source_symlinks_and_traversing_bin_paths_are_rejected() {
    let (root, workspace, source) = fixture();
    std::os::unix::fs::symlink(root.path().join("outside"), source.join("linked")).unwrap();
    assert!(
        Scan::acquire(
            "session".into(),
            workspace.clone(),
            source.to_string_lossy().into(),
            Some(root.path().into())
        )
        .await
        .is_err()
    );
    fs::remove_file(source.join("linked")).unwrap();
    fs::write(
        source.join("package.json"),
        r#"{"name":"bad","bin":"../../outside.js"}"#,
    )
    .unwrap();
    assert!(
        Scan::acquire(
            "session".into(),
            workspace,
            source.to_string_lossy().into(),
            Some(root.path().into())
        )
        .await
        .is_err()
    );
}
