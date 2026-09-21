use std::{fs, os::unix::fs::symlink};

use orbit_tools::{
    capability::{EnvironmentId, FilesystemAccess, FilesystemCapability, NetworkPolicy},
    path_security::ResolvedPath,
    policy::{FilesystemGrant, PolicyDecision, PolicyEngine, WorkspacePermissions},
};

#[test]
fn session_grants_are_isolated_and_ephemeral() {
    let tmp_ws = tempfile::tempdir().unwrap();
    let tmp_ext = tempfile::tempdir().unwrap();

    let workspace = ResolvedPath::resolve(tmp_ws.path(), None).unwrap();
    let external_file = tmp_ext.path().join("secrets.env");
    fs::write(&external_file, "API_KEY=12345").unwrap();

    let resolved_external = ResolvedPath::resolve(&external_file, None).unwrap();
    let cap = FilesystemCapability::local_read(&external_file);

    // 1. First runtime session
    let engine1 = PolicyEngine::new(workspace.clone());
    assert_eq!(
        engine1.check_filesystem(&cap),
        PolicyDecision::NeedsApproval(vec![cap.clone()])
    );

    // User approves in session 1
    engine1.record_grant(FilesystemGrant::read_only(
        EnvironmentId::Local,
        resolved_external,
    ));
    assert_eq!(engine1.check_filesystem(&cap), PolicyDecision::Granted);

    // 2. New runtime session started (e.g. restart / new session)
    // The previous SessionGrantStore is dropped. New engine must NOT inherit session 1 grants.
    let engine2 = PolicyEngine::new(workspace);
    assert_eq!(
        engine2.check_filesystem(&cap),
        PolicyDecision::NeedsApproval(vec![cap])
    );
}

#[test]
fn persistent_workspace_policy_grants_configured_external_paths() {
    let tmp_ws = tempfile::tempdir().unwrap();
    let tmp_cache = tempfile::tempdir().unwrap();
    let tmp_logs = tempfile::tempdir().unwrap();

    let workspace = ResolvedPath::resolve(tmp_ws.path(), None).unwrap();

    let permissions = WorkspacePermissions {
        read: vec![tmp_cache.path().to_path_buf()],
        read_write: vec![tmp_logs.path().to_path_buf()],
    };

    let engine = PolicyEngine::with_permissions(workspace, permissions, NetworkPolicy::Allow);

    // 1. Cache directory is persistent read-only
    let cache_file = tmp_cache.path().join("pkg.tar.gz");
    fs::write(&cache_file, "bytes").unwrap();

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

    // 2. Logs directory is persistent read-write
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
    fs::write(&outside_secret, "PRIVATE KEY").unwrap();

    // Create a symlink inside workspace pointing to outside secret
    let link_inside_ws = tmp_ws.path().join("stolen_key");
    symlink(&outside_secret, &link_inside_ws).unwrap();

    let workspace = ResolvedPath::resolve(tmp_ws.path(), None).unwrap();
    let engine = PolicyEngine::new(workspace);

    // Reading through the symlink resolves to the external canonical target
    let cap = FilesystemCapability::local_read(&link_inside_ws);
    let decision = engine.check_filesystem(&cap);

    // Must NOT be automatically granted as a workspace path!
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

    // Even if path looks like a workspace path, a Remote capability is not Local workspace
    let remote_cap =
        FilesystemCapability::new(remote_env.clone(), &local_path, FilesystemAccess::Read);
    assert!(matches!(
        engine.check_filesystem(&remote_cap),
        PolicyDecision::NeedsApproval(_)
    ));

    // Grant on remote environment
    let resolved = ResolvedPath::resolve(&local_path, None).unwrap();
    engine.record_grant(FilesystemGrant::read_only(remote_env, resolved));

    assert_eq!(
        engine.check_filesystem(&remote_cap),
        PolicyDecision::Granted
    );

    // Another remote server does not have access
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
