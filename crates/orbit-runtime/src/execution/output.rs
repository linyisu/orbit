use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use crate::error::{RuntimeError, failed};

pub const MAX_OUTPUT: usize = 32 * 1024;
pub const MAX_RETAINED_OUTPUT_PER_STREAM: u64 = 16 * 1024 * 1024;
pub const MAX_RETAINED_OUTPUT_PER_SESSION: u64 = 128 * 1024 * 1024;

static OUTPUT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// OutputLog manages persistent, paginatable stdout/stderr stream files on disk.
pub struct OutputLog {
    id: String,
    stdout: tokio::fs::File,
    stderr: tokio::fs::File,
}

impl OutputLog {
    pub async fn create(directory: &Path) -> Result<Self, RuntimeError> {
        use std::os::unix::fs::DirBuilderExt;

        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
            .map_err(failed)?;

        let metadata = std::fs::symlink_metadata(directory).map_err(failed)?;

        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(failed("command output directory must be a real directory"));
        }

        prune_output_directory(directory)?;

        let id = format!(
            "cmd-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(failed)?
                .as_nanos(),
            OUTPUT_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        );

        let stdout = create_output_file(&directory.join(format!("{id}.stdout")))?;
        let stderr = match create_output_file(&directory.join(format!("{id}.stderr"))) {
            Ok(file) => file,
            Err(error) => {
                let _ = std::fs::remove_file(directory.join(format!("{id}.stdout")));
                return Err(error);
            }
        };

        Ok(Self {
            id,
            stdout: tokio::fs::File::from_std(stdout),
            stderr: tokio::fs::File::from_std(stderr),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn split(self) -> (String, tokio::fs::File, tokio::fs::File) {
        (self.id, self.stdout, self.stderr)
    }
}

fn create_output_file(path: &Path) -> Result<std::fs::File, RuntimeError> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(failed)
}

fn prune_output_directory(directory: &Path) -> Result<(), RuntimeError> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(failed)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = std::fs::symlink_metadata(entry.path()).ok()?;

            (metadata.is_file() && entry.file_name().to_string_lossy().starts_with("cmd-"))
                .then_some((
                    entry.path(),
                    metadata.len(),
                    metadata
                        .modified()
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                ))
        })
        .collect::<Vec<_>>();

    let mut total = entries.iter().map(|(_, size, _)| *size).sum::<u64>();
    entries.sort_by_key(|(_, _, modified)| *modified);

    for (path, size, _) in entries {
        if total
            <= MAX_RETAINED_OUTPUT_PER_SESSION.saturating_sub(2 * MAX_RETAINED_OUTPUT_PER_STREAM)
        {
            break;
        }

        std::fs::remove_file(path).map_err(failed)?;
        total = total.saturating_sub(size);
    }

    Ok(())
}

/// OutputPreview contains bounded slices of stdout and stderr suitable for model responses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputPreview {
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub total_stdout_bytes: u64,
    pub total_stderr_bytes: u64,
}
