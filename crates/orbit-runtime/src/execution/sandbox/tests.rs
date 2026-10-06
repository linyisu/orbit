use super::*;
use crate::{
    capability::NetworkPolicy,
    execution::process::{is_reserved_mount_target, normalize_mount_target},
    filesystem::ResolvedPath,
    policy::BasePolicy,
};

#[tokio::test]
async fn absent_git_config_and_hooks_are_protected_in_a_writable_workspace() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".git/objects")).unwrap();
    std::fs::create_dir_all(dir.path().join(".git/refs")).unwrap();
    std::fs::write(dir.path().join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let policy = EffectivePolicy::new(BasePolicy::new(ResolvedPath::from_canonical(dir.path())));
    let mut spec = ProcessSpec::new(
        "set -eu; git status --porcelain; for path in .git/config .git/config.worktree .git/hooks/pre-commit; do if (printf injected > \"$path\") 2>/dev/null; then exit 42; fi; done; printf safe",
        Duration::from_secs(10),
    ).with_read_write_workspace(true);
    spec.unmask_git = true;
    let result = BwrapSandboxBackend::new()
        .unwrap()
        .run(spec, &policy, None, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(result.exit_code, Some(0), "{}", result.stderr);
    assert!(result.stdout.ends_with("safe"));
    for name in ["config", "config.worktree", "hooks"] {
        assert!(
            !dir.path().join(".git").join(name).exists(),
            "placeholder {name} must be cleaned up"
        );
    }

    assert!(dir.path().join(".git/HEAD").exists());
}

#[tokio::test]
async fn temporary_volume_enforces_an_aggregate_quota() {
    let dir = tempfile::tempdir().unwrap();
    let policy = EffectivePolicy::new(BasePolicy::new(ResolvedPath::from_canonical(dir.path())));
    let mut spec = ProcessSpec::new(
        "set -eu; dd if=/dev/zero of=/tmp/first bs=1024 count=768 2>/dev/null; if dd if=/dev/zero of=/tmp/second bs=1024 count=768 2>/dev/null; then exit 42; fi; printf bounded",
        Duration::from_secs(10),
    );
    spec.limits.max_tmp_bytes = 1024 * 1024;
    let result = BwrapSandboxBackend::new()
        .unwrap()
        .run(spec, &policy, None, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(result.exit_code, Some(0), "{}", result.stderr);
    assert_eq!(result.stdout, "bounded");
}

#[test]
fn browser_reader_mounts_only_system_files_and_a_disposable_profile() {
    let profile = tempfile::tempdir().unwrap();
    let command = browser_reader_command(profile.path()).unwrap();
    let args = command
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    assert!(args.contains(&"--unshare-pid".into()));
    assert!(args.contains(&"--die-with-parent".into()));
    assert!(
        args.windows(3)
            .any(|part| part == ["--bind", profile.path().to_str().unwrap(), "/orbit-browser"])
    );
    assert!(!args.iter().any(|arg| arg == "/workspace" || arg == "/home"));
    assert!(args.windows(2).any(|part| part == ["--seccomp", "0"]));
}

#[tokio::test]
#[ignore = "requires Firefox and Bubblewrap on the host"]
async fn isolated_firefox_starts_its_debugger_and_can_be_reaped() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let profile = tempfile::tempdir().unwrap();
    let mut child = browser_reader_command(profile.path())
        .unwrap()
        .args([
            "--headless",
            "--no-remote",
            "--profile",
            "/orbit-browser",
            "--remote-debugging-port",
            "0",
        ])
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let mut diagnostic = String::new();
    let ready = tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            if line.starts_with("WebDriver BiDi listening on ws://") {
                return true;
            }
            diagnostic.push_str(&line);
            diagnostic.push('\n');
        }

        false
    })
    .await
    .unwrap_or(false);
    let _ = child.kill().await;
    child.wait().await.unwrap();
    assert!(ready, "Firefox failed to start in isolation: {diagnostic}");
}

#[test]
fn test_normalize_mount_target() {
    assert_eq!(
        normalize_mount_target(Path::new("/opt/orbit-cache/packages")).unwrap(),
        PathBuf::from("/opt/orbit-cache/packages")
    );
    assert_eq!(
        normalize_mount_target(Path::new("/opt/orbit-cache/packages/")).unwrap(),
        PathBuf::from("/opt/orbit-cache/packages")
    );
    assert_eq!(
        normalize_mount_target(Path::new("/opt//orbit-cache///packages")).unwrap(),
        PathBuf::from("/opt/orbit-cache/packages")
    );
    assert!(normalize_mount_target(Path::new("relative/path")).is_err());
    assert!(normalize_mount_target(Path::new("/opt/../etc")).is_err());
    assert!(normalize_mount_target(Path::new("/opt/./cache")).is_err());
    assert!(normalize_mount_target(Path::new("/")).is_err());
    assert!(normalize_mount_target(Path::new("///")).is_err());
}

#[test]
fn test_is_reserved_mount_target() {
    assert!(is_reserved_mount_target(Path::new("/")));
    assert!(is_reserved_mount_target(Path::new("/opt")));
    assert!(is_reserved_mount_target(Path::new("/opt/orbit-cache")));

    assert!(is_reserved_mount_target(Path::new("/workspace")));
    assert!(is_reserved_mount_target(Path::new("/workspace/src")));
    assert!(is_reserved_mount_target(Path::new("/tmp")));
    assert!(is_reserved_mount_target(Path::new("/tmp/scratch")));
    assert!(is_reserved_mount_target(Path::new("/scratch")));
    assert!(is_reserved_mount_target(Path::new("/scratch/dir")));
    assert!(is_reserved_mount_target(Path::new("/opt/orbit-rust")));
    assert!(is_reserved_mount_target(Path::new("/opt/orbit-rust/bin")));
    assert!(is_reserved_mount_target(Path::new("/dev")));
    assert!(is_reserved_mount_target(Path::new("/dev/null")));
    assert!(is_reserved_mount_target(Path::new("/proc")));
    assert!(is_reserved_mount_target(Path::new("/sys")));
    assert!(is_reserved_mount_target(Path::new("/etc")));
    assert!(is_reserved_mount_target(Path::new("/etc/passwd")));
    assert!(is_reserved_mount_target(Path::new("/usr")));
    assert!(is_reserved_mount_target(Path::new("/bin")));
    assert!(is_reserved_mount_target(Path::new("/sbin")));
    assert!(is_reserved_mount_target(Path::new("/lib")));
    assert!(is_reserved_mount_target(Path::new("/lib64")));
    assert!(is_reserved_mount_target(Path::new("/orbit-command.sh")));
    assert!(is_reserved_mount_target(Path::new("/orbit-source")));

    assert!(!is_reserved_mount_target(Path::new(
        "/opt/orbit-cache/cargo"
    )));
    assert!(!is_reserved_mount_target(Path::new(
        "/opt/orbit-cache/packages"
    )));
    assert!(!is_reserved_mount_target(Path::new(
        "/opt/orbit-cache/nested/dir"
    )));
    assert!(!is_reserved_mount_target(Path::new("/mnt/data")));
    assert!(!is_reserved_mount_target(Path::new("/data")));
    assert!(!is_reserved_mount_target(Path::new("/var/cache")));
}

#[test]
fn test_mount_spec_validation() {
    let dir = tempfile::tempdir().unwrap();
    let valid_mount = MountSpec::read_only(dir.path(), "/opt/orbit-cache/packages");

    assert!(valid_mount.validate().is_ok());

    let bad_source = MountSpec::read_only(
        dir.path().join("does_not_exist"),
        "/opt/orbit-cache/packages",
    );

    assert!(bad_source.validate().is_err());

    let reserved_mount = MountSpec::read_only(dir.path(), "/workspace");

    assert!(reserved_mount.validate().is_err());

    let relative_mount = MountSpec::read_only(dir.path(), "opt/orbit-cache/packages");

    assert!(relative_mount.validate().is_err());
}

#[test]
fn test_validate_mounts_duplicate_and_overlapping() {
    let dir1 = tempfile::tempdir().unwrap();
    let dir2 = tempfile::tempdir().unwrap();
    let policy = EffectivePolicy::new(crate::policy::BasePolicy::new(
        crate::filesystem::ResolvedPath::from_canonical(dir1.path()),
    ));

    let duplicates = vec![
        MountSpec::read_only(dir1.path(), "/mnt/data"),
        MountSpec::read_write(dir2.path(), "/mnt/data"),
    ];
    let err = validate_mounts(&duplicates, &policy).unwrap_err();

    assert!(err.to_string().contains("duplicate mount target"));

    let overlapping = vec![
        MountSpec::read_only(dir1.path(), "/mnt/data"),
        MountSpec::read_write(dir2.path(), "/mnt/data/subdir"),
    ];
    let err = validate_mounts(&overlapping, &policy).unwrap_err();

    assert!(
        err.to_string()
            .contains("conflicting or overlapping mount targets")
    );

    let non_overlapping = vec![
        MountSpec::read_only(dir1.path(), "/mnt/data1"),
        MountSpec::read_write(dir2.path(), "/mnt/data2"),
    ];

    assert!(validate_mounts(&non_overlapping, &policy).is_ok());
}

#[test]
fn rejects_mount_sources_overlapping_protected_paths() {
    let workspace = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let secret = source.path().join("credential");
    std::fs::write(&secret, "secret").unwrap();
    let policy = EffectivePolicy::new(crate::policy::BasePolicy::new(
        crate::filesystem::ResolvedPath::from_canonical(workspace.path()),
    ));
    policy.protected_paths.write().unwrap().push(secret.clone());

    let ancestor = MountSpec::read_only(source.path(), "/opt/orbit-cache/source");
    let exact = MountSpec::read_only(&secret, "/opt/orbit-cache/secret");

    assert!(validate_mounts(&[ancestor], &policy).is_err());
    assert!(validate_mounts(&[exact], &policy).is_err());
}

#[test]
fn test_rejects_mount_source_hardlink_to_protected_path() {
    let workspace = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    let secret = source.path().join("secret.key");
    std::fs::write(&secret, "super_sensitive_secret").unwrap();

    let policy = EffectivePolicy::new(crate::policy::BasePolicy::new(
        crate::filesystem::ResolvedPath::from_canonical(workspace.path()),
    ));
    policy.protected_paths.write().unwrap().push(secret.clone());

    let cache = tempfile::tempdir().unwrap();
    let hardlink = cache.path().join("alias_secret.key");
    std::fs::hard_link(&secret, &hardlink).unwrap();

    let cache_mount = MountSpec::read_only(cache.path(), "/opt/orbit-cache/cache");
    let err = validate_mounts(&[cache_mount], &policy).unwrap_err();
    assert!(
        err.to_string()
            .contains("contains hardlink to protected path"),
        "unexpected error: {err}"
    );

    let direct_mount = MountSpec::read_only(&hardlink, "/opt/orbit-cache/alias");
    let err = validate_mounts(&[direct_mount], &policy).unwrap_err();
    assert!(
        err.to_string()
            .contains("is a hardlink alias of a protected path"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_validate_mounts_capability_collision() {
    let ws_dir = tempfile::tempdir().unwrap();
    let cap_canonical = PathBuf::from("/var/data/shared");
    let mount_src = tempfile::tempdir().unwrap();

    let mut policy = EffectivePolicy::new(crate::policy::BasePolicy::new(
        crate::filesystem::ResolvedPath::from_canonical(ws_dir.path()),
    ));
    policy
        .persistent_read
        .push(crate::filesystem::ResolvedPath::from_canonical(
            &cap_canonical,
        ));

    let collides = vec![MountSpec::read_only(mount_src.path(), &cap_canonical)];
    let err = validate_mounts(&collides, &policy).unwrap_err();

    assert!(
        err.to_string()
            .contains("conflicts or overlaps with active capability grant")
    );

    let subpath = vec![MountSpec::read_only(
        mount_src.path(),
        cap_canonical.join("sub"),
    )];
    let err = validate_mounts(&subpath, &policy).unwrap_err();

    assert!(
        err.to_string()
            .contains("conflicts or overlaps with active capability grant")
    );

    let valid = vec![MountSpec::read_only(
        mount_src.path(),
        "/opt/orbit-cache/cargo",
    )];

    assert!(validate_mounts(&valid, &policy).is_ok());
}

#[test]
fn test_verification_evidence_mounts_format() {
    let dir = tempfile::tempdir().unwrap();
    let spec = ProcessSpec {
        command: "echo test".into(),
        cwd: None,
        env: std::collections::BTreeMap::new(),
        timeout: Duration::from_secs(5),
        scratch: None,
        limits: ProcessLimits::default(),
        read_only_workspace: false,
        rust_toolchain: None,
        mounts: vec![
            MountSpec::read_only(dir.path(), "/opt/orbit-cache/packages/"),
            MountSpec::read_write(dir.path(), "/mnt/extra"),
        ],
        workspace_files: 0,
        skipped_entries: 0,
        fingerprint: Some(WorkspaceFingerprint::default()),
        protected_paths: Vec::new(),
        opaque_runtime_roots: Vec::new(),
        unmask_git: false,
        allow_git_config_write: false,
    };
    let formatted: Vec<String> = spec
        .mounts
        .iter()
        .map(|m| {
            let target = m.normalized_target().unwrap_or_else(|_| m.target.clone());
            let access = match m.access {
                MountAccess::ReadOnly => "ro",
                MountAccess::ReadWrite => "rw",
            };

            format!("{}: {access}", target.display())
        })
        .collect();

    assert_eq!(
        formatted,
        vec![
            "/opt/orbit-cache/packages: ro".to_string(),
            "/mnt/extra: rw".to_string(),
        ]
    );
    assert!(
        !formatted
            .iter()
            .any(|m| m.contains(dir.path().to_str().unwrap()))
    );
}

#[test]
fn test_collect_masked_paths_masks_deep_env_and_nonexistent_placeholders_and_hardlinks() {
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = ws_dir.path();

    let env_file = ws.join(".env");
    std::fs::write(&env_file, "SECRET=123").unwrap();
    let env_alias = ws.join("alias.env");
    std::fs::hard_link(&env_file, &env_alias).unwrap();

    let config_file = ws.join("config.toml");
    std::fs::write(&config_file, "key=456").unwrap();
    let config_alias = ws.join("alias.toml");
    std::fs::hard_link(&config_file, &config_alias).unwrap();

    let nonexistent_file = ws.join("secret.key");
    let nonexistent_dir = ws.join("orbit.db.workspaces");

    let protected_paths = vec![
        config_file.clone(),
        nonexistent_file.clone(),
        nonexistent_dir.clone(),
    ];
    let opaque_runtime_roots = vec![nonexistent_dir.clone()];

    {
        let mut guard = PlaceholderGuard::new();
        let (masked, _) = collect_masked_paths(
            ws,
            &protected_paths,
            &opaque_runtime_roots,
            true,
            false,
            &mut guard,
        )
        .unwrap();

        assert!(!nonexistent_file.exists());
        assert!(!nonexistent_dir.exists());

        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/config.toml") && !*is_dir)
        );
        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/alias.toml") && !*is_dir)
        );
        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/.env") && !*is_dir)
        );
        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/alias.env") && !*is_dir)
        );
    }

    {
        let mut guard = PlaceholderGuard::new();
        let (masked, _) = collect_masked_paths(
            ws,
            &protected_paths,
            &opaque_runtime_roots,
            false,
            false,
            &mut guard,
        )
        .unwrap();

        assert!(nonexistent_file.is_file());
        assert!(nonexistent_dir.is_dir());

        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/secret.key") && !*is_dir)
        );
        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/orbit.db.workspaces") && *is_dir)
        );

        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/alias.toml") && !*is_dir)
        );
        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/alias.env") && !*is_dir)
        );

        drop(guard);

        assert!(!nonexistent_file.exists());
        assert!(!nonexistent_dir.exists());
        assert!(config_file.exists());
        assert!(env_file.exists());
    }

    {
        let artifact_dir = ws.join("my.artifacts");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        let secret_file = artifact_dir.join("token.txt");
        std::fs::write(&secret_file, "supersecret").unwrap();
        let secret_alias = ws.join("leak_token.txt");
        std::fs::hard_link(&secret_file, &secret_alias).unwrap();

        let mut guard = PlaceholderGuard::new();
        let (masked, _) = collect_masked_paths(
            ws,
            std::slice::from_ref(&artifact_dir),
            &[],
            true,
            false,
            &mut guard,
        )
        .unwrap();

        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/my.artifacts") && *is_dir)
        );
        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/leak_token.txt") && !*is_dir)
        );
    }

    {
        let fresh_ws = tempfile::tempdir().unwrap();
        let mut guard = PlaceholderGuard::new();
        let (masked, _) =
            collect_masked_paths(fresh_ws.path(), &[], &[], false, false, &mut guard).unwrap();

        assert!(fresh_ws.path().join(".git").is_dir());
        assert!(fresh_ws.path().join(".env").is_file());
        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/.git") && *is_dir)
        );
        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/.env") && !*is_dir)
        );

        drop(guard);

        assert!(!fresh_ws.path().join(".git").exists());
        assert!(!fresh_ws.path().join(".env").exists());
    }

    {
        let git_ws = tempfile::tempdir().unwrap();
        let dot_git = git_ws.path().join(".git");
        std::fs::create_dir(&dot_git).unwrap();
        std::fs::write(git_ws.path().join(".env"), "SECRET=val").unwrap();

        let mut guard = PlaceholderGuard::new();
        let (masked, _) =
            collect_masked_paths(git_ws.path(), &[], &[], false, true, &mut guard).unwrap();

        assert!(
            !masked
                .iter()
                .any(|(p, _)| p == Path::new("/workspace/.git"))
        );
        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/.env") && !*is_dir)
        );
    }
}

#[test]
fn test_mount_capabilities_rejects_deep_nested_hardlinks() {
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = ws_dir.path();
    let config_file = ws.join("config.toml");
    std::fs::write(&config_file, "super_api_key").unwrap();

    let mut guard = PlaceholderGuard::new();
    let (_, protected_inodes) = collect_masked_paths(
        ws,
        std::slice::from_ref(&config_file),
        &[],
        true,
        false,
        &mut guard,
    )
    .unwrap();

    let cap_dir = tempfile::tempdir().unwrap();
    let nested_dir = cap_dir.path().join("sub1").join("sub2");
    std::fs::create_dir_all(&nested_dir).unwrap();
    let hardlink = nested_dir.join("aliased_config.txt");
    std::fs::hard_link(&config_file, &hardlink).unwrap();

    let mut policy = EffectivePolicy::new(crate::policy::BasePolicy::new(
        crate::filesystem::ResolvedPath::from_canonical(ws),
    ));
    let resolved_cap = crate::filesystem::ResolvedPath::from_canonical(cap_dir.path());
    policy.persistent_read.push(resolved_cap);

    let mut command = tokio::process::Command::new("/bin/true");
    let err = mount_capabilities(
        &mut command,
        &policy,
        ws,
        &[config_file],
        &protected_inodes,
        &[],
    )
    .unwrap_err();

    assert!(
        err.to_string()
            .contains("contains hardlink to protected path")
    );
}

#[test]
fn mount_capabilities_rejects_opaque_root_descendant_alias() {
    let ws_dir = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let opaque = storage.path().join("orbit.workspaces");
    std::fs::create_dir(&opaque).unwrap();
    let secret = opaque.join("secret.txt");
    std::fs::write(&secret, "private runtime data").unwrap();
    let alias = external.path().join("alias.txt");
    std::fs::hard_link(&secret, &alias).unwrap();

    let mut policy = EffectivePolicy::new(crate::policy::BasePolicy::new(
        crate::filesystem::ResolvedPath::from_canonical(ws_dir.path()),
    ));
    policy
        .persistent_read
        .push(crate::filesystem::ResolvedPath::from_canonical(&alias));
    let mut command = tokio::process::Command::new("/bin/true");
    let error = mount_capabilities(
        &mut command,
        &policy,
        ws_dir.path(),
        std::slice::from_ref(&opaque),
        &HashSet::new(),
        std::slice::from_ref(&opaque),
    )
    .unwrap_err();

    assert!(
        error
            .to_string()
            .contains("hardlink alias of a protected path")
    );
}

#[test]
fn test_collect_dir_inodes_rejects_depth_exceeding_limit() {
    let temp = tempfile::tempdir().unwrap();
    let mut curr = temp.path().to_path_buf();

    // Create 34 nested directory levels (exceeding MAX_PROTECTED_DIR_DEPTH = 32)
    for i in 0..34 {
        curr = curr.join(format!("dir_{i}"));
    }

    std::fs::create_dir_all(&curr).unwrap();
    let file = curr.join("secret.txt");
    std::fs::write(&file, "leak").unwrap();

    let mut inodes = HashSet::new();
    let err = collect_dir_inodes(temp.path(), &mut inodes).unwrap_err();

    assert!(
        err.to_string()
            .contains("depth exceeds maximum limit of 32")
    );
}

#[test]
fn test_mount_capabilities_rejects_depth_exceeding_limit() {
    let ws_dir = tempfile::tempdir().unwrap();
    let cap_dir = tempfile::tempdir().unwrap();
    let mut curr = cap_dir.path().to_path_buf();

    // Create 18 nested directory levels (exceeding MAX_CAP_SCAN_DEPTH = 16)
    for i in 0..18 {
        curr = curr.join(format!("sub_{i}"));
    }

    std::fs::create_dir_all(&curr).unwrap();
    let file = curr.join("data.txt");
    std::fs::write(&file, "hello").unwrap();

    let mut policy = EffectivePolicy::new(crate::policy::BasePolicy::new(
        crate::filesystem::ResolvedPath::from_canonical(ws_dir.path()),
    ));
    let resolved_cap = crate::filesystem::ResolvedPath::from_canonical(cap_dir.path());
    policy.persistent_read.push(resolved_cap);

    let mut command = tokio::process::Command::new("/bin/true");
    let protected_inodes = HashSet::new();
    let err = mount_capabilities(
        &mut command,
        &policy,
        ws_dir.path(),
        &[],
        &protected_inodes,
        &[],
    )
    .unwrap_err();

    assert!(
        err.to_string()
            .contains("depth exceeds maximum limit of 16")
    );
}

#[test]
fn opaque_roots_skip_idle_scans_and_fail_closed_for_ambiguous_hardlinks() {
    use std::os::unix::fs::MetadataExt;

    let ws_dir = tempfile::tempdir().unwrap();
    let ws = ws_dir.path();

    let storage_parent = tempfile::tempdir().unwrap();
    let external_storage = storage_parent.path().join("orbit.workspaces");
    let mut curr = external_storage.clone();

    for i in 0..35 {
        curr = curr.join(format!("level_{i}"));
    }

    std::fs::create_dir_all(&curr).unwrap();
    std::fs::write(curr.join("deep.txt"), "content").unwrap();

    let mut test_inodes = HashSet::new();

    assert!(collect_dir_inodes(&external_storage, &mut test_inodes).is_err());

    let mut guard = PlaceholderGuard::new();
    let (masked, inodes) = collect_masked_paths(
        ws,
        std::slice::from_ref(&external_storage),
        std::slice::from_ref(&external_storage),
        false,
        false,
        &mut guard,
    )
    .unwrap();
    let ext_meta = std::fs::symlink_metadata(&external_storage).unwrap();

    assert!(inodes.contains(&(ext_meta.dev(), ext_meta.ino())));
    assert!(
        masked
            .iter()
            .all(|(path, _)| !path.to_string_lossy().contains("level_"))
    );

    // An external hardlink on the same device makes the skipped root
    // ambiguous. Its deep scan cannot complete, so the command is denied.
    let external_file_dir = tempfile::tempdir().unwrap();
    let external_db = external_file_dir.path().join("orbit.db");
    std::fs::write(&external_db, "sqlite_header").unwrap();
    let ws_db_alias = ws.join("leaked_orbit.db");
    std::fs::hard_link(&external_db, &ws_db_alias).unwrap();

    let protected_paths = vec![external_storage.clone(), external_db.clone()];
    let opaque_runtime_roots = vec![external_storage.clone()];

    let mut guard = PlaceholderGuard::new();
    let error = collect_masked_paths(
        ws,
        &protected_paths,
        &opaque_runtime_roots,
        false,
        false,
        &mut guard,
    )
    .unwrap_err();

    assert!(error.to_string().contains("depth exceeds maximum limit"));
}

#[test]
fn test_external_protected_directory_descendant_hardlinks_are_masked() {
    use std::os::unix::fs::MetadataExt;

    let ws_dir = tempfile::tempdir().unwrap();
    let ws = ws_dir.path();

    let external_secrets = tempfile::tempdir().unwrap();
    let secret_file = external_secrets.path().join("api_key.txt");
    std::fs::write(&secret_file, "secret_content_12345").unwrap();

    let nested_secrets_dir = external_secrets.path().join("nested_folder");
    std::fs::create_dir_all(&nested_secrets_dir).unwrap();
    let nested_secret_file = nested_secrets_dir.join("token.json");
    std::fs::write(&nested_secret_file, r#"{"token": "xyz"}"#).unwrap();

    let ws_leak1 = ws.join("leaked_key.txt");
    std::fs::hard_link(&secret_file, &ws_leak1).unwrap();
    let ws_leak2 = ws.join("leaked_token.json");
    std::fs::hard_link(&nested_secret_file, &ws_leak2).unwrap();

    let protected_paths = vec![external_secrets.path().to_path_buf()];

    let mut guard = PlaceholderGuard::new();
    let (masked, inodes) =
        collect_masked_paths(ws, &protected_paths, &[], false, false, &mut guard).unwrap();

    let secret_meta1 = std::fs::symlink_metadata(&secret_file).unwrap();
    let secret_meta2 = std::fs::symlink_metadata(&nested_secret_file).unwrap();

    assert!(
        inodes.contains(&(secret_meta1.dev(), secret_meta1.ino())),
        "descendant file inside external protected directory must be tracked in protected_inodes"
    );
    assert!(
        inodes.contains(&(secret_meta2.dev(), secret_meta2.ino())),
        "nested descendant file inside external protected directory must be tracked in protected_inodes"
    );

    assert!(
        masked
            .iter()
            .any(|(p, is_dir)| p == Path::new("/workspace/leaked_key.txt") && !*is_dir),
        "hardlink alias inside workspace pointing to file in external protected directory must be masked"
    );
    assert!(
        masked
            .iter()
            .any(|(p, is_dir)| p == Path::new("/workspace/leaked_token.json") && !*is_dir),
        "nested hardlink alias inside workspace pointing to file in external protected directory must be masked"
    );
}

#[test]
fn test_external_directory_matching_heuristic_name_scans_descendants_when_not_in_opaque_roots() {
    use std::os::unix::fs::MetadataExt;

    let ws_dir = tempfile::tempdir().unwrap();
    let ws = ws_dir.path();

    // Directory named like the old heuristic suffix (e.g. .workspaces), but it is a user directory
    let external_dir = tempfile::tempdir().unwrap();
    let custom_workspaces = external_dir.path().join("my_project.workspaces");
    std::fs::create_dir_all(&custom_workspaces).unwrap();
    let secret = custom_workspaces.join("secret_data.csv");
    std::fs::write(&secret, "confidential").unwrap();

    let ws_link = ws.join("leak_data.csv");
    std::fs::hard_link(&secret, &ws_link).unwrap();

    let protected_paths = vec![custom_workspaces.clone()];
    let opaque_runtime_roots = vec![];

    let mut guard = PlaceholderGuard::new();
    let (masked, inodes) = collect_masked_paths(
        ws,
        &protected_paths,
        &opaque_runtime_roots,
        false,
        false,
        &mut guard,
    )
    .unwrap();

    let meta = std::fs::symlink_metadata(&secret).unwrap();

    assert!(
        inodes.contains(&(meta.dev(), meta.ino())),
        "descendant of user directory named *.workspaces must be scanned when not registered as opaque runtime root"
    );
    assert!(
        masked
            .iter()
            .any(|(p, is_dir)| p == Path::new("/workspace/leak_data.csv") && !*is_dir),
        "hardlink to secret inside user directory named *.workspaces must be masked"
    );
}

#[test]
fn test_task_tmp_disk_dir_and_optional_resource_limits() {
    let tmp_task = tempfile::tempdir().unwrap();
    let mut cmd = Command::new("/usr/bin/bwrap");

    mount_scratch_and_tmp(&mut cmd, None, Some(tmp_task.path()), 0).unwrap();
    let args = cmd
        .as_std()
        .get_args()
        .map(|s| s.to_string_lossy().to_string())
        .collect::<Vec<_>>();

    assert!(args.contains(&"--bind".to_string()));
    assert!(args.contains(&tmp_task.path().to_string_lossy().to_string()));
    assert!(args.contains(&"/tmp".to_string()));
    assert!(!args.contains(&"--tmpfs".to_string()));
    let mut bounded = Command::new("/usr/bin/bwrap");
    mount_scratch_and_tmp(&mut bounded, None, Some(tmp_task.path()), 1024 * 1024).unwrap();
    let args = bounded
        .as_std()
        .get_args()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect::<Vec<_>>();

    assert!(
        args.windows(4)
            .any(|args| args == ["--size", "1048576", "--tmpfs", "/tmp"])
    );
    assert!(!args.contains(&tmp_task.path().to_string_lossy().to_string()));

    let mut cmd_limits = Command::new("/usr/bin/bwrap");
    let unlimited_limits = ProcessLimits::default(); // 0 for memory and file size
    configure_limits_and_entrypoint(
        &mut cmd_limits,
        Path::new("/usr/bin/prlimit"),
        &unlimited_limits,
        false,
        false,
        false,
        &Default::default(),
        &[],
        None,
    );

    let limit_args = cmd_limits
        .as_std()
        .get_args()
        .map(|s| s.to_string_lossy().to_string())
        .collect::<Vec<_>>();

    assert!(!limit_args.iter().any(|a| a.starts_with("--as=")));
    assert!(!limit_args.iter().any(|a| a.starts_with("--fsize=")));
    assert!(limit_args.iter().any(|a| a.starts_with("--nofile=")));
    assert!(limit_args.iter().any(|a| a.starts_with("--nproc=")));

    let mut cmd_explicit = Command::new("/usr/bin/bwrap");
    let explicit_limits = ProcessLimits {
        max_memory_bytes: 64 * 1024 * 1024,
        max_file_size_bytes: 10 * 1024 * 1024,
        max_processes: 100,
        max_open_files: 200,
        ..Default::default()
    };

    configure_limits_and_entrypoint(
        &mut cmd_explicit,
        Path::new("/usr/bin/prlimit"),
        &explicit_limits,
        false,
        false,
        false,
        &Default::default(),
        &[],
        None,
    );

    let explicit_args = cmd_explicit
        .as_std()
        .get_args()
        .map(|s| s.to_string_lossy().to_string())
        .collect::<Vec<_>>();

    assert!(explicit_args.iter().any(|a| a == "--as=67108864:67108864"));
    assert!(
        explicit_args
            .iter()
            .any(|a| a == "--fsize=10485760:10485760")
    );
}

#[test]
fn test_resolve_container_cwd() {
    let ws = Path::new("/home/user/project");
    let mounts = vec![MountSpec::read_only("/host/cache", "/opt/cache")];

    assert_eq!(
        resolve_container_cwd(ws, &mounts, None),
        PathBuf::from("/workspace")
    );
    assert_eq!(
        resolve_container_cwd(ws, &mounts, Some(Path::new(""))),
        PathBuf::from("/workspace")
    );
    assert_eq!(
        resolve_container_cwd(ws, &mounts, Some(Path::new("."))),
        PathBuf::from("/workspace")
    );

    assert_eq!(
        resolve_container_cwd(ws, &mounts, Some(Path::new("/workspace/src"))),
        PathBuf::from("/workspace/src")
    );

    assert_eq!(
        resolve_container_cwd(
            ws,
            &mounts,
            Some(Path::new("/home/user/project/crates/sub"))
        ),
        PathBuf::from("/workspace/crates/sub")
    );

    assert_eq!(
        resolve_container_cwd(ws, &mounts, Some(Path::new("crates/sub"))),
        PathBuf::from("/workspace/crates/sub")
    );

    assert_eq!(
        resolve_container_cwd(ws, &mounts, Some(Path::new("/host/cache/packages"))),
        PathBuf::from("/opt/cache/packages")
    );
}

#[test]
fn test_configure_limits_and_entrypoint_env() {
    let mut cmd = Command::new("/usr/bin/bwrap");
    let limits = ProcessLimits::default();
    let mut custom_env = std::collections::BTreeMap::new();
    custom_env.insert("CUSTOM_VAR".to_string(), "my_value".to_string());
    custom_env.insert("OPENAI_API_KEY".to_string(), "leak_secret".to_string());
    custom_env.insert("PATH".to_string(), "/custom/bin:/usr/bin".to_string());
    custom_env.insert("CARGO_HOME".to_string(), "/home/user/.cargo".to_string());
    custom_env.insert("RUSTUP_HOME".to_string(), "/home/user/.rustup".to_string());

    configure_limits_and_entrypoint(
        &mut cmd,
        Path::new("/usr/bin/prlimit"),
        &limits,
        true, // has_rust
        false,
        false,
        &custom_env,
        &[],
        None,
    );

    let args = cmd
        .as_std()
        .get_args()
        .map(|s| s.to_string_lossy().to_string())
        .collect::<Vec<_>>();

    let custom_idx = args.iter().position(|a| a == "CUSTOM_VAR").unwrap();
    assert_eq!(args[custom_idx - 1], "--setenv");
    assert_eq!(args[custom_idx + 1], "my_value");

    assert!(!args.contains(&"OPENAI_API_KEY".to_string()));
    assert!(!args.contains(&"leak_secret".to_string()));

    let path_idx = args.iter().position(|a| a == "PATH").unwrap();
    assert_eq!(
        args[path_idx + 1],
        "/opt/orbit-rust/bin:/custom/bin:/usr/bin"
    );

    let cargo_idx = args.iter().position(|a| a == "CARGO_HOME").unwrap();
    assert_eq!(args[cargo_idx + 1], "/tmp/home/.cargo");

    let rustup_idx = args.iter().position(|a| a == "RUSTUP_HOME").unwrap();
    assert_eq!(args[rustup_idx + 1], "/tmp/home/.rustup");
}

#[test]
fn test_resolve_container_cwd_with_symlink_mounts() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path().join("workspace");
    std::fs::create_dir(&ws).unwrap();

    let real_cache = temp.path().join("real_cache");
    std::fs::create_dir_all(real_cache.join("sub")).unwrap();
    let real_cache_canon = real_cache.canonicalize().unwrap();

    let symlink_cache = temp.path().join("symlink_cache");
    std::os::unix::fs::symlink(&real_cache, &symlink_cache).unwrap();

    let mounts = vec![MountSpec::read_only(&symlink_cache, "/opt/cache")];

    let resolved = resolve_container_cwd(&ws, &mounts, Some(&real_cache_canon.join("sub")));
    assert_eq!(resolved, PathBuf::from("/opt/cache/sub"));

    let resolved_sym = resolve_container_cwd(&ws, &mounts, Some(&symlink_cache.join("sub")));
    assert_eq!(resolved_sym, PathBuf::from("/opt/cache/sub"));
}

#[test]
fn test_configure_limits_and_entrypoint_preserves_mounted_home() {
    let mut cmd = Command::new("/usr/bin/bwrap");
    let limits = ProcessLimits::default();
    let mut custom_env = std::collections::BTreeMap::new();
    custom_env.insert("HOME".to_string(), "/opt/review-home".to_string());
    custom_env.insert(
        "CARGO_HOME".to_string(),
        "/opt/review-home/.cargo".to_string(),
    );

    let mounts = vec![MountSpec::read_write("/host/home", "/opt/review-home")];

    configure_limits_and_entrypoint(
        &mut cmd,
        Path::new("/usr/bin/prlimit"),
        &limits,
        false,
        false,
        false,
        &custom_env,
        &mounts,
        None,
    );

    let args = cmd
        .as_std()
        .get_args()
        .map(|s| s.to_string_lossy().to_string())
        .collect::<Vec<_>>();

    let home_idx = args.iter().position(|a| a == "HOME").unwrap();
    assert_eq!(args[home_idx + 1], "/opt/review-home");

    let cargo_idx = args.iter().position(|a| a == "CARGO_HOME").unwrap();
    assert_eq!(args[cargo_idx + 1], "/opt/review-home/.cargo");
}

#[test]
fn test_rust_toolchain_mount_rejects_protected_path() {
    let temp = tempfile::tempdir().unwrap();
    let toolchain = temp.path().join("toolchain");
    std::fs::create_dir_all(toolchain.join("bin")).unwrap();
    std::fs::create_dir_all(toolchain.join("lib/rustlib")).unwrap();
    std::fs::write(toolchain.join("bin/rustc"), "#!/bin/sh\n").unwrap();
    std::fs::write(toolchain.join("bin/cargo"), "#!/bin/sh\n").unwrap();
    let secret = toolchain.join("secret_token.txt");
    std::fs::write(&secret, "super_secret").unwrap();

    let base = BasePolicy {
        workspace: ResolvedPath::resolve(temp.path(), None).unwrap(),
        network: NetworkPolicy::Deny,
    };
    let policy = EffectivePolicy::new(base);
    policy
        .protected_paths
        .write()
        .unwrap()
        .push(secret.canonicalize().unwrap());

    let mut cmd = Command::new("/usr/bin/bwrap");
    let result = mount_toolchain_and_mounts(&mut cmd, Some(&toolchain), &[], &policy);
    assert!(
        result.is_err(),
        "toolchain containing protected path must be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("overlaps protected path"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_mount_source_allows_more_than_10000_entries() {
    let temp = tempfile::tempdir().unwrap();
    let cache_dir = temp.path().join("large_cache");
    std::fs::create_dir(&cache_dir).unwrap();

    // Create 10,005 dummy files to exceed 10,000 entries
    for i in 0..10_005 {
        std::fs::write(cache_dir.join(format!("file_{i}.tmp")), b"").unwrap();
    }

    let base = BasePolicy {
        workspace: ResolvedPath::resolve(temp.path(), None).unwrap(),
        network: NetworkPolicy::Deny,
    };
    let policy = EffectivePolicy::new(base);

    let validated = validate_mount_source(&cache_dir, &policy);
    assert!(
        validated.is_ok(),
        "large mount source directory (> 10000 entries) must succeed, got: {:?}",
        validated.err()
    );
}

#[test]
fn test_mount_source_rejects_single_link_env_file() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path().join("workspace");
    std::fs::create_dir(&ws).unwrap();
    let env_file = ws.join(".env");
    std::fs::write(&env_file, "API_KEY=secret_val").unwrap();

    let base = BasePolicy {
        workspace: ResolvedPath::resolve(&ws, None).unwrap(),
        network: NetworkPolicy::Deny,
    };
    let policy = EffectivePolicy::new(base);

    // Mounting a review directory that points to the workspace containing single-link .env
    let mount = MountSpec::read_only(&ws, "/opt/review");
    let result = validate_mounts(&[mount], &policy);
    assert!(
        result.is_err(),
        "mount source containing single-link .env must be rejected"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("contains hardlink to protected path")
            || err.contains("overlaps protected path"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_toolchain_mount_succeeds_with_large_opaque_runtime_root() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path().join("workspace");
    std::fs::create_dir(&ws).unwrap();

    // Create an unrelated opaque root with 20,001 entries
    let opaque_root = temp.path().join("large_session_store");
    std::fs::create_dir(&opaque_root).unwrap();
    for i in 0..20_001 {
        std::fs::write(opaque_root.join(format!("session_{i}.json")), b"{}").unwrap();
    }

    let toolchain = temp.path().join("toolchain");
    std::fs::create_dir_all(toolchain.join("bin")).unwrap();
    std::fs::create_dir_all(toolchain.join("lib/rustlib")).unwrap();
    std::fs::write(toolchain.join("bin/rustc"), "#!/bin/sh\n").unwrap();
    std::fs::write(toolchain.join("bin/cargo"), "#!/bin/sh\n").unwrap();

    let base = BasePolicy {
        workspace: ResolvedPath::resolve(&ws, None).unwrap(),
        network: NetworkPolicy::Deny,
    };
    let policy = EffectivePolicy::new(base);
    policy
        .opaque_runtime_roots
        .write()
        .unwrap()
        .push(opaque_root.canonicalize().unwrap());
    policy
        .protected_paths
        .write()
        .unwrap()
        .push(opaque_root.canonicalize().unwrap());

    let mut cmd = Command::new("/usr/bin/bwrap");
    let result = mount_toolchain_and_mounts(&mut cmd, Some(&toolchain), &[], &policy);
    assert!(
        result.is_ok(),
        "toolchain validation must succeed even if unrelated opaque root has > 20000 entries, got: {:?}",
        result.err()
    );
}

#[test]
fn system_toolchains_and_scoped_sources_keep_sandbox_targets_reserved() {
    let temp = tempfile::tempdir().unwrap();
    let ws = temp.path().join("workspace");
    std::fs::create_dir(&ws).unwrap();

    let base = BasePolicy {
        workspace: ResolvedPath::resolve(&ws, None).unwrap(),
        network: NetworkPolicy::Deny,
    };
    let policy = EffectivePolicy::new(base);

    let source = std::fs::canonicalize("/etc/os-release").unwrap();
    assert!(validate_mount_source(&source, &policy).is_ok());
    assert!(is_reserved_mount_target(Path::new("/etc")));
    assert!(is_reserved_mount_target(Path::new("/usr")));

    let toolchain_res = validate_toolchain_source(Path::new("/usr"), &policy);
    assert!(
        toolchain_res.is_ok(),
        "validate_toolchain_source must allow /usr, got: {:?}",
        toolchain_res.err()
    );
}
