use std::path::Path;

use super::*;

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

    let engine = PolicyEngine::new(workspace);
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

    store.grant(FilesystemGrant::read_only(
        EnvironmentId::Local,
        child.clone(),
    ));

    assert_eq!(store.grants().len(), 1);

    store.grant(FilesystemGrant::read_only(EnvironmentId::Local, parent));

    assert_eq!(store.grants().len(), 1);

    store.grant(FilesystemGrant::read_only(EnvironmentId::Local, child));

    assert_eq!(store.grants().len(), 1);
}

#[test]
fn rejects_ancestor_of_workspace_as_capability() {
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("proj");
    std::fs::create_dir_all(&proj).unwrap();

    let workspace = ResolvedPath::resolve(&proj, None).unwrap();
    let engine = PolicyEngine::new(workspace);

    let cap = FilesystemCapability::local_read(tmp.path());

    match engine.check_filesystem(&cap) {
        PolicyDecision::Denied(reason) => {
            assert!(reason.contains("ancestor of the workspace"));
        }
        other => panic!("expected Denied, got {other:?}"),
    }
}

#[test]
fn rejects_protected_path_and_its_ancestor_and_descendant() {
    let tmp_ws = tempfile::tempdir().unwrap();
    let tmp_ext = tempfile::tempdir().unwrap();
    let secret = tmp_ext
        .path()
        .join("nested")
        .join("orbit.db");
    std::fs::create_dir_all(secret.parent().unwrap()).unwrap();
    std::fs::write(&secret, "database").unwrap();

    let workspace = ResolvedPath::resolve(tmp_ws.path(), None).unwrap();
    let engine = PolicyEngine::new(workspace);
    engine.add_protected_path(secret.clone());

    let cap_direct = FilesystemCapability::local_read(&secret);

    match engine.check_filesystem(&cap_direct) {
        PolicyDecision::Denied(reason) => {
            assert!(reason.contains("inside protected path"));
        }
        other => panic!("expected Denied, got {other:?}"),
    }

    let cap_parent = FilesystemCapability::local_read(tmp_ext.path());

    match engine.check_filesystem(&cap_parent) {
        PolicyDecision::Denied(reason) => {
            assert!(reason.contains("contains protected path"));
        }
        other => panic!("expected Denied, got {other:?}"),
    }
}

#[test]
fn rejects_reserved_runtime_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ResolvedPath::resolve(tmp.path(), None).unwrap();
    let engine = PolicyEngine::new(workspace);

    for reserved in [
        "/",
        "/tmp",
        "/opt",
        "/proc/cpuinfo",
        "/dev/null",
        "/sys/kernel",
    ] {
        let cap = FilesystemCapability::local_read(Path::new(reserved));

        assert!(
            matches!(engine.check_filesystem(&cap), PolicyDecision::Denied(_)),
            "expected {reserved} to be denied"
        );
    }
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
            .is_err()
    );

    let perms_inside = WorkspacePermissions {
        read_templates: Vec::new(),
        write_templates: Vec::new(),
        read: vec![tmp_ws.path().join("src")],
        read_write: Vec::new(),
    };

    assert!(
        PolicyEngine::try_with_permissions(ws.clone(), perms_inside, NetworkPolicy::Allow).is_err()
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
        PolicyDecision::Denied(_)
    ));
}
