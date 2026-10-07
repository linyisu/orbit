use super::failed;
use orbit_agent::tool::ToolError;
use std::path::Path;
use tokio_util::sync::CancellationToken;

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    worker: &Path,
    bytes: &[u8],
    extension: &str,
    sandbox: bool,
    entry: &str,
    arguments: &[String],
    cancellation: CancellationToken,
) -> Result<tempfile::TempDir, ToolError> {
    use orbit_runtime::execution::{
        ExecutionEnvironment, ExecutionPolicy, ExecutionRequest, MountSpec, ResourceLimits,
        execution_backend,
    };
    let input = tempfile::tempdir().map_err(failed)?;
    let scratch = tempfile::tempdir().map_err(failed)?;
    std::fs::write(input.path().join(format!("source.{extension}")), bytes).map_err(failed)?;
    let source = input.path().join(format!("source.{extension}"));
    let output = scratch.path().join("result.json");
    let executable = if sandbox {
        Path::new("/opt/orbit-document-worker")
    } else {
        worker
    };
    let source = if sandbox {
        std::path::PathBuf::from(format!("/workspace/source.{extension}"))
    } else {
        source
    };
    let output = if sandbox {
        std::path::PathBuf::from("/scratch/result.json")
    } else {
        output
    };
    let mut argv = vec![
        executable
            .to_str()
            .ok_or_else(|| failed("non-UTF-8 worker path"))?,
        entry,
        source
            .to_str()
            .ok_or_else(|| failed("non-UTF-8 source path"))?,
        output
            .to_str()
            .ok_or_else(|| failed("non-UTF-8 output path"))?,
    ];
    argv.extend(arguments.iter().map(String::as_str));
    let command = shlex::try_join(argv).map_err(failed)?;
    let request = ExecutionRequest::new(command, input.path(), std::time::Duration::from_secs(60))
        .with_scratch(scratch.path());
    let environment = ExecutionEnvironment::new(input.path(), input.path()).map_err(failed)?;
    let mut policy = ExecutionPolicy::read_only();
    policy.mounts = vec![MountSpec::read_only(worker, "/opt/orbit-document-worker")];
    policy.resources = ResourceLimits {
        max_memory_bytes: Some(1024 * 1024 * 1024),
        max_file_size_bytes: Some(64 * 1024 * 1024),
        max_processes: Some(64),
        max_open_files: Some(128),
        ..Default::default()
    };
    let result = execution_backend(sandbox)
        .map_err(failed)?
        .execute(&request, &policy, &environment, None, cancellation)
        .await
        .map_err(failed)?;
    if result.cancelled {
        return Err(ToolError::Cancelled);
    }
    if result.timed_out {
        return Err(failed("document worker timed out after 60 seconds"));
    }
    if result.exit_code != Some(0) {
        return Err(failed(format!("document worker failed: {}", result.stderr)));
    }

    Ok(scratch)
}
