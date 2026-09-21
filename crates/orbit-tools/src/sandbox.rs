use std::{path::Path, time::Duration};

use orbit_agent::tool::ToolError;
use orbit_agent::verification::WorkspaceFingerprint;
pub use orbit_runtime::execution::process::*;
pub use orbit_runtime::execution::sandbox::{
    BwrapSandboxBackend, SandboxBackend, Snapshot, WorkspaceScanLimits, content_hash, fingerprint,
};
use orbit_runtime::filesystem::Workspace;
use tokio_util::sync::CancellationToken;

pub async fn fingerprint_configured(
    workspace: Workspace,
    cancellation: CancellationToken,
    limits: impl Into<WorkspaceScanLimits>,
) -> Result<WorkspaceFingerprint, ToolError> {
    orbit_runtime::execution::sandbox::fingerprint_configured(
        workspace,
        cancellation,
        limits.into(),
    )
    .await
}

pub async fn scan_workspace_summary(
    workspace: Workspace,
    cancellation: CancellationToken,
    limits: impl Into<WorkspaceScanLimits>,
) -> Result<(WorkspaceFingerprint, usize, usize), ToolError> {
    orbit_runtime::execution::sandbox::scan_workspace_summary(
        workspace,
        cancellation,
        limits.into(),
    )
    .await
}

pub async fn run(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
) -> Result<CommandResult, ToolError> {
    run_with_options(workspace, script, timeout, token, false).await
}

pub async fn run_with_options(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
) -> Result<CommandResult, ToolError> {
    run_with_toolchain(workspace, script, timeout, token, writable_snapshot, None).await
}

pub async fn run_with_toolchain(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
    rust_toolchain: Option<&Path>,
) -> Result<CommandResult, ToolError> {
    run_with_resources(
        workspace,
        script,
        timeout,
        token,
        writable_snapshot,
        rust_toolchain,
        None,
    )
    .await
}

pub async fn run_with_resources(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
    rust_toolchain: Option<&Path>,
    scratch: Option<&Path>,
) -> Result<CommandResult, ToolError> {
    run_with_resources_and_output(
        workspace,
        script,
        timeout,
        token,
        writable_snapshot,
        rust_toolchain,
        scratch,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_with_resources_and_output(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
    rust_toolchain: Option<&Path>,
    scratch: Option<&Path>,
    output_log: Option<OutputLog>,
) -> Result<CommandResult, ToolError> {
    let limits = crate::run_command::CommandLimits {
        rust_toolchain: rust_toolchain.map(Path::to_path_buf),
        ..Default::default()
    };
    run_configured(
        workspace,
        script,
        timeout,
        token,
        writable_snapshot,
        &limits,
        scratch,
        output_log,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_configured(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
    limits: &crate::run_command::CommandLimits,
    scratch: Option<&Path>,
    output_log: Option<OutputLog>,
) -> Result<CommandResult, ToolError> {
    let scan_limits = WorkspaceScanLimits {
        max_scan_mib: limits.max_scan_mib,
        max_scan_entries: limits.max_scan_entries,
    };
    orbit_runtime::execution::sandbox::run_configured(
        workspace,
        script,
        timeout,
        token,
        writable_snapshot,
        &scan_limits,
        &limits.process_limits,
        limits.rust_toolchain.as_deref(),
        &limits.dependency_caches,
        scratch,
        output_log,
    )
    .await
}
