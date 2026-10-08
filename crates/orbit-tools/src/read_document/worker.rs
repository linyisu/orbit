use super::failed;
use orbit_agent::tool::ToolError;
use std::path::Path;
use tokio_util::sync::CancellationToken;

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    worker: &Path,
    bytes: &[u8],
    extension: &str,
    entry: &str,
    arguments: &[String],
    cancellation: CancellationToken,
) -> Result<tempfile::TempDir, ToolError> {
    use orbit_runtime::execution::{
        ExecutionEnvironment, ExecutionPolicy, ExecutionRequest, ResourceLimits, execution_backend,
    };
    let input = tempfile::tempdir().map_err(failed)?;
    let temporary_output = tempfile::tempdir().map_err(failed)?;
    std::fs::write(input.path().join(format!("source.{extension}")), bytes).map_err(failed)?;
    let source = input.path().join(format!("source.{extension}"));
    let output = temporary_output.path().join("result.json");
    let mut argv = vec![
        worker
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
    let request = ExecutionRequest::new(command, input.path(), std::time::Duration::from_secs(60));
    let environment = ExecutionEnvironment::new(input.path(), input.path()).map_err(failed)?;
    let policy = ExecutionPolicy {
        resources: ResourceLimits {
            max_memory_bytes: Some(1024 * 1024 * 1024),
            max_file_size_bytes: Some(64 * 1024 * 1024),
            max_open_files: Some(128),
        },
        ..Default::default()
    };
    let result = execution_backend()
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

    Ok(temporary_output)
}
