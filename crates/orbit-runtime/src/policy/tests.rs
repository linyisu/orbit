use std::path::Path;

use super::*;

#[test]
fn all_paths_are_grantable_and_yolo_is_live_and_environment_scoped() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let engine = PolicyEngine::new(ResolvedPath::resolve(&project, None).unwrap());
    for path in ["/", "/tmp", "/opt", "/proc", "/dev", "/sys"] {
        let capability = FilesystemCapability::local_read(Path::new(path));
        assert!(matches!(
            engine.check_filesystem(&capability),
            PolicyDecision::NeedsApproval(_)
        ));
        engine.record_grant(FilesystemGrant::read_only(
            EnvironmentId::Local,
            ResolvedPath::resolve(path, None).unwrap(),
        ));
        assert_eq!(
            engine.check_filesystem(&capability),
            PolicyDecision::Granted
        );
        engine.clear_session_grants();
    }
    let capability = FilesystemCapability::local_read(root.path());
    engine
        .approval_policy()
        .set_mode(ApprovalMode::Yolo);
    assert_eq!(
        engine.check_filesystem(&capability),
        PolicyDecision::Granted
    );
    let remote = FilesystemCapability::new(
        EnvironmentId::Remote("synthetic-target".into()),
        root.path(),
        FilesystemAccess::Read,
    );
    assert!(matches!(
        engine.check_filesystem(&remote),
        PolicyDecision::NeedsApproval(_)
    ));
    engine
        .approval_policy()
        .set_mode(ApprovalMode::Auto);
    assert!(matches!(
        engine.check_filesystem(&capability),
        PolicyDecision::NeedsApproval(_)
    ));
}

#[test]
fn workspace_paths_are_always_granted() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ResolvedPath::resolve(tmp.path(), None).unwrap();
    let engine = PolicyEngine::new(workspace);

    let inside_read = FilesystemCapability::local_read(tmp.path().join("src/main.rs"));

    assert_eq!(
        engine.check_filesystem(&inside_read),
        PolicyDecision::Granted
    );

    let inside_write = FilesystemCapability::local_write(tmp.path().join("target/output.bin"));

    assert_eq!(
        engine.check_filesystem(&inside_write),
        PolicyDecision::Granted
    );
}

#[test]
fn outside_paths_require_approval_until_granted() {
    let tmp_ws = tempfile::tempdir().unwrap();
    let tmp_external = tempfile::tempdir().unwrap();

    let workspace = ResolvedPath::resolve(tmp_ws.path(), None).unwrap();
    let external_file = tmp_external.path().join("dataset.csv");
    std::fs::write(&external_file, "data").unwrap();

    let engine = PolicyEngine::new(workspace.clone());
    let cap_read = FilesystemCapability::local_read(&external_file);

    assert!(matches!(
        engine.check_filesystem(&cap_read),
        PolicyDecision::NeedsApproval(_)
    ));

    let resolved_external = ResolvedPath::resolve(&external_file, None).unwrap();
    engine.record_grant(FilesystemGrant::read_only(
        EnvironmentId::Local,
        resolved_external,
    ));

    assert_eq!(engine.check_filesystem(&cap_read), PolicyDecision::Granted);

    let cap_write = FilesystemCapability::local_write(&external_file);

    assert!(matches!(
        engine.check_filesystem(&cap_write),
        PolicyDecision::NeedsApproval(_)
    ));
    assert!(matches!(
        PolicyEngine::new(workspace).check_filesystem(&cap_read),
        PolicyDecision::NeedsApproval(_)
    ));
}

#[test]
fn directory_grant_inherits_to_children_and_merges() {
    let tmp_ws = tempfile::tempdir().unwrap();
    let tmp_models = tempfile::tempdir().unwrap();

    let workspace = ResolvedPath::resolve(tmp_ws.path(), None).unwrap();
    let engine = PolicyEngine::new(workspace);

    let resolved_models = ResolvedPath::resolve(tmp_models.path(), None).unwrap();
    engine.record_grant(FilesystemGrant::read_only(
        EnvironmentId::Local,
        resolved_models.clone(),
    ));

    let child_read = FilesystemCapability::local_read(
        tmp_models
            .path()
            .join("llama")
            .join("weights.bin"),
    );

    assert_eq!(
        engine.check_filesystem(&child_read),
        PolicyDecision::Granted
    );

    let child_write = FilesystemCapability::local_write(
        tmp_models
            .path()
            .join("llama")
            .join("weights.bin"),
    );

    assert!(matches!(
        engine.check_filesystem(&child_write),
        PolicyDecision::NeedsApproval(_)
    ));

    engine.record_grant(FilesystemGrant::read_write(
        EnvironmentId::Local,
        resolved_models,
    ));

    assert_eq!(
        engine.check_filesystem(&child_write),
        PolicyDecision::Granted
    );
}

#[test]
fn session_store_prunes_redundant_subpaths() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = ResolvedPath::resolve(tmp.path(), None).unwrap();
    let child = ResolvedPath::resolve(tmp.path().join("sub"), None).unwrap();

    let mut store = SessionGrantStore::new();

    let child_grant = FilesystemGrant::read_only(EnvironmentId::Local, child);
    let parent_grant = FilesystemGrant::read_only(EnvironmentId::Local, parent);
    store.grant(child_grant.clone());
    assert_eq!(store.grants(), std::slice::from_ref(&child_grant));

    store.grant(parent_grant.clone());
    assert_eq!(store.grants(), std::slice::from_ref(&parent_grant));

    store.grant(child_grant);
    assert_eq!(store.grants(), &[parent_grant]);
}

#[test]
fn persistent_permissions_upfront_validation_and_overlap_rejection() {
    let tmp_ws = tempfile::tempdir().unwrap();
    let tmp_ext = tempfile::tempdir().unwrap();
    let ext_dir = tmp_ext.path().join("data");
    std::fs::create_dir_all(&ext_dir).unwrap();

    let ws = ResolvedPath::resolve(tmp_ws.path(), None).unwrap();

    let perms_nonexistent = WorkspacePermissions {
        read_templates: Vec::new(),
        write_templates: Vec::new(),
        read: vec![tmp_ext.path().join("nonexistent")],
        read_write: Vec::new(),
    };

    assert!(
        PolicyEngine::try_with_permissions(ws.clone(), perms_nonexistent, NetworkPolicy::Allow)
            .is_err()
    );

    let perms_ancestor = WorkspacePermissions {
        read_templates: Vec::new(),
        write_templates: Vec::new(),
        read: vec![
            tmp_ws
                .path()
                .parent()
                .unwrap()
                .to_path_buf(),
        ],
        read_write: Vec::new(),
    };

    assert!(
        PolicyEngine::try_with_permissions(ws.clone(), perms_ancestor, NetworkPolicy::Allow)
            .is_ok()
    );

    let inside = tmp_ws.path().join("src");
    std::fs::create_dir(&inside).unwrap();
    let perms_inside = WorkspacePermissions {
        read_templates: Vec::new(),
        write_templates: Vec::new(),
        read: vec![inside],
        read_write: Vec::new(),
    };

    assert!(
        PolicyEngine::try_with_permissions(ws.clone(), perms_inside, NetworkPolicy::Allow).is_ok()
    );

    let sub_dir = ext_dir.join("secret");
    std::fs::create_dir_all(&sub_dir).unwrap();
    let perms_overlap = WorkspacePermissions {
        read_templates: Vec::new(),
        write_templates: Vec::new(),
        read_write: vec![ext_dir.clone()],
        read: vec![sub_dir.clone()],
    };
    let err = PolicyEngine::try_with_permissions(ws.clone(), perms_overlap, NetworkPolicy::Allow)
        .unwrap_err()
        .to_string();

    assert!(err.contains("overlaps with read path"));

    let sub_rw = ext_dir.join("sub_rw");
    std::fs::create_dir_all(&sub_rw).unwrap();
    let perms_redundant = WorkspacePermissions {
        read_templates: Vec::new(),
        write_templates: Vec::new(),
        read_write: vec![ext_dir.clone(), sub_rw],
        read: Vec::new(),
    };
    let engine =
        PolicyEngine::try_with_permissions(ws, perms_redundant, NetworkPolicy::Allow).unwrap();

    assert_eq!(
        engine
            .effective_policy()
            .persistent_read_write
            .len(),
        1
    );
}

#[cfg(target_os = "linux")]
#[test]
fn ordinary_system_files_can_receive_scoped_user_grants() {
    let tmp = tempfile::tempdir().unwrap();
    let engine = PolicyEngine::new(ResolvedPath::resolve(tmp.path(), None).unwrap());
    let path = std::fs::canonicalize("/etc/os-release").unwrap();
    let read = FilesystemCapability::local_read(&path);
    assert!(matches!(
        engine.check_filesystem(&read),
        PolicyDecision::NeedsApproval(_)
    ));
    engine.record_grant(FilesystemGrant {
        environment: EnvironmentId::Local,
        path: ResolvedPath::resolve(&path, None).unwrap(),
        read: true,
        write: false,
    });
    assert!(matches!(
        engine.check_filesystem(&read),
        PolicyDecision::Granted
    ));
    assert!(matches!(
        engine.check_filesystem(&FilesystemCapability::local_read_write(&path)),
        PolicyDecision::NeedsApproval(_)
    ));
    assert!(matches!(
        engine.check_filesystem(&FilesystemCapability::local_read(Path::new(
            "/proc/self/status"
        ))),
        PolicyDecision::NeedsApproval(_)
    ));
}

#[test]
fn persistent_workspace_policy_grants_configured_external_paths() {
    let tmp_ws = tempfile::tempdir().unwrap();
    let tmp_cache = tempfile::tempdir().unwrap();
    let tmp_logs = tempfile::tempdir().unwrap();

    let workspace = ResolvedPath::resolve(tmp_ws.path(), None).unwrap();

    let permissions = WorkspacePermissions {
        read_templates: Vec::new(),
        write_templates: Vec::new(),
        read: vec![tmp_cache.path().to_path_buf()],
        read_write: vec![tmp_logs.path().to_path_buf()],
    };

    let engine = PolicyEngine::with_permissions(workspace, permissions, NetworkPolicy::Allow);

    let cache_file = tmp_cache.path().join("pkg.tar.gz");
    std::fs::write(&cache_file, "bytes").unwrap();

    let cache_read = FilesystemCapability::local_read(&cache_file);

    assert_eq!(
        engine.check_filesystem(&cache_read),
        PolicyDecision::Granted
    );

    let cache_write = FilesystemCapability::local_write(&cache_file);

    assert!(matches!(
        engine.check_filesystem(&cache_write),
        PolicyDecision::NeedsApproval(_)
    ));

    let log_file = tmp_logs.path().join("app.log");
    let log_read = FilesystemCapability::local_read(&log_file);
    let log_write = FilesystemCapability::local_write(&log_file);

    assert_eq!(engine.check_filesystem(&log_read), PolicyDecision::Granted);
    assert_eq!(engine.check_filesystem(&log_write), PolicyDecision::Granted);
}

#[test]
fn symlink_pointing_outside_workspace_is_evaluated_against_target() {
    let tmp_ws = tempfile::tempdir().unwrap();
    let tmp_outside = tempfile::tempdir().unwrap();

    let outside_secret = tmp_outside.path().join("id_rsa");
    std::fs::write(&outside_secret, "PRIVATE KEY").unwrap();

    let link_inside_ws = tmp_ws.path().join("stolen_key");
    std::os::unix::fs::symlink(&outside_secret, &link_inside_ws).unwrap();

    let workspace = ResolvedPath::resolve(tmp_ws.path(), None).unwrap();
    let engine = PolicyEngine::new(workspace);

    let cap = FilesystemCapability::local_read(&link_inside_ws);
    let decision = engine.check_filesystem(&cap);

    assert!(
        matches!(decision, PolicyDecision::NeedsApproval(_)),
        "symlink to outside file must require approval: {decision:?}"
    );
}

#[test]
fn remote_environment_capabilities_are_isolated_from_local() {
    let tmp_ws = tempfile::tempdir().unwrap();
    let workspace = ResolvedPath::resolve(tmp_ws.path(), None).unwrap();
    let engine = PolicyEngine::new(workspace);

    let local_path = tmp_ws.path().join("file.txt");
    let remote_env = EnvironmentId::Remote("gpu-server".into());

    let remote_cap =
        FilesystemCapability::new(remote_env.clone(), &local_path, FilesystemAccess::Read);

    assert!(matches!(
        engine.check_filesystem(&remote_cap),
        PolicyDecision::NeedsApproval(_)
    ));

    let resolved = ResolvedPath::resolve(&local_path, None).unwrap();
    engine.record_grant(FilesystemGrant::read_only(remote_env, resolved));

    assert_eq!(
        engine.check_filesystem(&remote_cap),
        PolicyDecision::Granted
    );

    let other_remote_cap = FilesystemCapability::new(
        EnvironmentId::Remote("other-server".into()),
        &local_path,
        FilesystemAccess::Read,
    );

    assert!(matches!(
        engine.check_filesystem(&other_remote_cap),
        PolicyDecision::NeedsApproval(_)
    ));
}
