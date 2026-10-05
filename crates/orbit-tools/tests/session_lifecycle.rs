use std::fs;

use orbit_runtime::{
    capability::{EnvironmentId, FilesystemCapability, NetworkPolicy},
    filesystem::{ResolvedPath, Workspace},
    policy::{FilesystemGrant, PolicyDecision, WorkspacePermissions},
};

#[tokio::test]
async fn session_grants_are_ephemeral_and_clearable() {
    let ws_temp = tempfile::tempdir().unwrap();
    let ext_temp = tempfile::tempdir().unwrap();
    let persistent_temp = tempfile::tempdir().unwrap();

    let mut workspace = Workspace::new(ws_temp.path()).unwrap();

    let permissions = WorkspacePermissions {
        read_templates: Vec::new(),
        write_templates: Vec::new(),
        read: vec![persistent_temp.path().to_path_buf()],
        read_write: vec![],
    };

    workspace
        .configure_policy(permissions, NetworkPolicy::Allow)
        .unwrap();

    let external_file = ext_temp.path().join("data.csv");
    fs::write(&external_file, "1,2,3\n").unwrap();
    let cap = FilesystemCapability::local_read(&external_file);

    assert!(matches!(
        workspace.policy().check_filesystem(&cap),
        PolicyDecision::NeedsApproval(_)
    ));

    let resolved_ext = ResolvedPath::resolve(&external_file, None).unwrap();
    workspace.policy().record_grant(FilesystemGrant::read_only(
        EnvironmentId::Local,
        resolved_ext,
    ));

    assert_eq!(
        workspace.policy().check_filesystem(&cap),
        PolicyDecision::Granted
    );

    workspace.policy().clear_session_grants();

    assert!(matches!(
        workspace.policy().check_filesystem(&cap),
        PolicyDecision::NeedsApproval(_)
    ));

    let persistent_file = persistent_temp.path().join("model.bin");
    fs::write(&persistent_file, "weights").unwrap();
    let persistent_cap = FilesystemCapability::local_read(&persistent_file);

    assert_eq!(
        workspace.policy().check_filesystem(&persistent_cap),
        PolicyDecision::Granted
    );

    let ws_cap = FilesystemCapability::local_read_write(ws_temp.path().join("src/lib.rs"));

    assert_eq!(
        workspace.policy().check_filesystem(&ws_cap),
        PolicyDecision::Granted
    );

    let fresh_workspace = Workspace::new(ws_temp.path()).unwrap();

    assert!(matches!(
        fresh_workspace.policy().check_filesystem(&cap),
        PolicyDecision::NeedsApproval(_)
    ));
    assert_eq!(
        fresh_workspace
            .policy()
            .session_grants()
            .read()
            .unwrap()
            .grants()
            .len(),
        0
    );
}
