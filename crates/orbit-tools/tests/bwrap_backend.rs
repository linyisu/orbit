use std::{fs, path::Path, time::Duration};

use orbit_runtime::{
    capability::{EnvironmentId, NetworkPolicy},
    execution::{BwrapSandboxBackend, ProcessSpec, SandboxBackend},
    filesystem::ResolvedPath,
    policy::{BasePolicy, EffectivePolicy, FilesystemGrant},
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn backend_executes_in_workspace_directly_with_write_access() {
    let ws_temp = tempfile::tempdir().unwrap();
    let ws_path = ws_temp.path();

    let backend = match BwrapSandboxBackend::new() {
        Ok(b) => b,
        Err(_) => return, // Skip on systems without bwrap
    };

    let ws_resolved = ResolvedPath::resolve(ws_path, None).unwrap();
    let base = BasePolicy {
        workspace: ws_resolved,
        network: NetworkPolicy::Allow,
    };
    let policy = EffectivePolicy::new(base);

    let spec = ProcessSpec::new(
        "echo 'hello capability' > /workspace/result.txt",
        Duration::from_secs(5),
    )
    .with_read_write_workspace(true);
    let result = backend
        .run(spec, &policy, None, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(result.exit_code, Some(0));

    let created_file = ws_path.join("result.txt");
    assert!(created_file.is_file());
    assert_eq!(
        fs::read_to_string(created_file).unwrap().trim(),
        "hello capability"
    );
}

#[tokio::test]
async fn backend_isolates_unmounted_external_paths() {
    let ws_temp = tempfile::tempdir().unwrap();
    let outside_temp = tempfile::tempdir().unwrap();

    let backend = match BwrapSandboxBackend::new() {
        Ok(b) => b,
        Err(_) => return,
    };

    let outside_file = outside_temp.path().join("secret.txt");
    fs::write(&outside_file, "super_secret_token").unwrap();

    let ws_resolved = ResolvedPath::resolve(ws_temp.path(), None).unwrap();
    let base = BasePolicy {
        workspace: ws_resolved,
        network: NetworkPolicy::Allow,
    };
    let policy = EffectivePolicy::new(base);

    let script = format!(
        "test -e {} && echo 'ACCESSIBLE' || echo 'BLOCKED'",
        outside_file.display()
    );
    let spec = ProcessSpec::new(script, Duration::from_secs(5));
    let result = backend
        .run(spec, &policy, None, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout.trim(), "BLOCKED");
}

#[tokio::test]
async fn backend_mounts_granted_external_path_with_read_or_write() {
    let ws_temp = tempfile::tempdir().unwrap();
    let outside_temp = tempfile::tempdir().unwrap();

    let backend = match BwrapSandboxBackend::new() {
        Ok(b) => b,
        Err(_) => return,
    };

    let outside_file = outside_temp.path().join("shared.txt");
    fs::write(&outside_file, "original content\n").unwrap();

    let ws_resolved = ResolvedPath::resolve(ws_temp.path(), None).unwrap();
    let outside_resolved = ResolvedPath::resolve(&outside_file, None).unwrap();

    // 1. Read-only grant: reading works, writing fails
    let base = BasePolicy {
        workspace: ws_resolved.clone(),
        network: NetworkPolicy::Allow,
    };
    let policy = EffectivePolicy::new(base);

    let ro_grant = FilesystemGrant {
        environment: EnvironmentId::Local,
        path: outside_resolved.clone(),
        read: true,
        write: false,
    };
    policy.session_grants.write().unwrap().grant(ro_grant);

    let read_script = format!("cat {}", outside_file.display());
    let spec = ProcessSpec::new(read_script, Duration::from_secs(5));
    let result = backend
        .run(spec, &policy, None, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout.trim(), "original content");

    let write_fail_script = format!("echo 'overwrite' >> {}", outside_file.display());
    let spec_fail = ProcessSpec::new(write_fail_script, Duration::from_secs(5));
    let fail_result = backend
        .run(spec_fail, &policy, None, CancellationToken::new())
        .await
        .unwrap();

    assert_ne!(fail_result.exit_code, Some(0));

    // 2. Read-write grant: writing succeeds
    let base_rw = BasePolicy {
        workspace: ws_resolved,
        network: NetworkPolicy::Allow,
    };
    let policy_rw = EffectivePolicy::new(base_rw);

    let rw_grant = FilesystemGrant {
        environment: EnvironmentId::Local,
        path: outside_resolved,
        read: true,
        write: true,
    };
    policy_rw.session_grants.write().unwrap().grant(rw_grant);

    let write_script = format!("echo 'updated content' > {}", outside_file.display());
    let spec_rw = ProcessSpec::new(write_script, Duration::from_secs(5));
    let rw_result = backend
        .run(spec_rw, &policy_rw, None, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(rw_result.exit_code, Some(0));
    assert_eq!(
        fs::read_to_string(&outside_file).unwrap().trim(),
        "updated content"
    );
}

#[tokio::test]
async fn backend_respects_network_deny_policy() {
    let ws_temp = tempfile::tempdir().unwrap();

    let backend = match BwrapSandboxBackend::new() {
        Ok(b) => b,
        Err(_) => return,
    };

    let ws_resolved = ResolvedPath::resolve(ws_temp.path(), None).unwrap();

    // 1. NetworkPolicy::Deny should not mount /etc/resolv.conf
    let base_deny = BasePolicy {
        workspace: ws_resolved.clone(),
        network: NetworkPolicy::Deny,
    };
    let policy_deny = EffectivePolicy::new(base_deny);

    let spec_deny = ProcessSpec::new(
        "test -e /etc/resolv.conf && echo 'RESOLV_EXISTS' || echo 'NO_RESOLV'",
        Duration::from_secs(5),
    );
    let result_deny = backend
        .run(spec_deny, &policy_deny, None, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(result_deny.exit_code, Some(0));
    assert_eq!(result_deny.stdout.trim(), "NO_RESOLV");

    // 2. NetworkPolicy::Allow mounts /etc/resolv.conf if present on host
    if Path::new("/etc/resolv.conf").exists() {
        let base_allow = BasePolicy {
            workspace: ws_resolved,
            network: NetworkPolicy::Allow,
        };
        let policy_allow = EffectivePolicy::new(base_allow);

        let spec_allow = ProcessSpec::new(
            "test -e /etc/resolv.conf && echo 'RESOLV_EXISTS' || echo 'NO_RESOLV'",
            Duration::from_secs(5),
        );
        let result_allow = backend
            .run(spec_allow, &policy_allow, None, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result_allow.exit_code, Some(0));
        assert_eq!(result_allow.stdout.trim(), "RESOLV_EXISTS");
    }
}
