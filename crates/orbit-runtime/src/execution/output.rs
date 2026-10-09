mod temporary;
pub use temporary::{CommandOutputs, OutputLease, OutputRun};

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicU64, Ordering},
    },
};

use crate::error::{RuntimeError, failed};

pub const MAX_OUTPUT: usize = 32 * 1024;
pub const MAX_RETAINED_OUTPUT_PER_STREAM: u64 = 16 * 1024 * 1024;
pub const MAX_RETAINED_OUTPUT_PER_RUN: u64 = 128 * 1024 * 1024;

static OUTPUT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static OUTPUT_BUDGETS: OnceLock<Mutex<HashMap<PathBuf, Weak<OutputBudget>>>> = OnceLock::new();

#[derive(Default)]
struct OutputBudget {
    used: AtomicU64,
    active: Mutex<HashSet<String>>,
}

struct ActiveLog {
    id: String,
    budget: Arc<OutputBudget>,
    directory: Option<Arc<temporary::RunDirectory>>,
}

impl Drop for ActiveLog {
    fn drop(&mut self) {
        self.budget
            .active
            .lock()
            .expect("output log lock poisoned")
            .remove(&self.id);
    }
}

pub(crate) struct RetainedOutput {
    file: tokio::fs::File,
    lease: Arc<ActiveLog>,
    written: u64,
}

impl RetainedOutput {
    pub async fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        let requested =
            (bytes.len() as u64).min(MAX_RETAINED_OUTPUT_PER_STREAM.saturating_sub(self.written));
        let used = &self.lease.budget.used;
        let prior = used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |prior| {
                Some(prior + requested.min(MAX_RETAINED_OUTPUT_PER_RUN.saturating_sub(prior)))
            })
            .expect("output budget update always succeeds");
        let allowed = requested.min(MAX_RETAINED_OUTPUT_PER_RUN.saturating_sub(prior));
        if allowed != 0 {
            self.file.write_all(&bytes[..allowed as usize]).await?;
            self.written += allowed;
            self.file.flush().await?;
        }

        Ok(())
    }

    pub async fn flush(&mut self) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        self.file.flush().await
    }

    pub fn written(&self) -> u64 {
        self.written
    }
}

fn output_budget(directory: &Path) -> Result<Arc<OutputBudget>, RuntimeError> {
    let mut budgets = OUTPUT_BUDGETS
        .get_or_init(Default::default)
        .lock()
        .expect("output budgets lock poisoned");
    budgets.retain(|_, budget| budget.strong_count() != 0);
    if let Some(budget) = budgets.get(directory).and_then(Weak::upgrade) {
        return Ok(budget);
    }
    let mut used = 0u64;
    for entry in std::fs::read_dir(directory).map_err(failed)? {
        let entry = entry.map_err(failed)?;
        let metadata = std::fs::symlink_metadata(entry.path()).map_err(failed)?;
        if metadata.is_file() && entry.file_name().to_string_lossy().starts_with("cmd-") {
            used = used.saturating_add(metadata.len());
        }
    }

    let budget = Arc::new(OutputBudget {
        used: AtomicU64::new(used),
        ..Default::default()
    });
    budgets.insert(directory.into(), Arc::downgrade(&budget));
    Ok(budget)
}

/// Retained stdout/stderr files with bounded, paginated previews.
pub struct OutputLog {
    id: String,
    stdout: tokio::fs::File,
    stderr: tokio::fs::File,
    lease: Arc<ActiveLog>,
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

        let directory = directory.canonicalize().map_err(failed)?;
        let budget = output_budget(&directory)?;
        let mut active = budget.active.lock().expect("output log lock poisoned");
        prune_output_directory(&directory, &budget, &active)?;

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

        active.insert(id.clone());
        drop(active);
        let lease = Arc::new(ActiveLog {
            id: id.clone(),
            budget,
            directory: None,
        });
        Ok(Self {
            id,
            stdout: tokio::fs::File::from_std(stdout),
            stderr: tokio::fs::File::from_std(stderr),
            lease,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn split(self) -> (String, RetainedOutput, RetainedOutput) {
        (
            self.id,
            RetainedOutput {
                file: self.stdout,
                lease: self.lease.clone(),
                written: 0,
            },
            RetainedOutput {
                file: self.stderr,
                lease: self.lease,
                written: 0,
            },
        )
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

fn prune_output_directory(
    directory: &Path,
    budget: &OutputBudget,
    active: &HashSet<String>,
) -> Result<(), RuntimeError> {
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

    entries.sort_by_key(|(_, _, modified)| *modified);

    for (path, size, _) in entries {
        if budget.used.load(Ordering::Acquire)
            <= MAX_RETAINED_OUTPUT_PER_RUN.saturating_sub(2 * MAX_RETAINED_OUTPUT_PER_STREAM)
        {
            break;
        }

        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if active.contains(stem) {
            continue;
        }
        std::fs::remove_file(path).map_err(failed)?;
        budget.used.fetch_sub(size, Ordering::AcqRel);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_logs_share_the_task_byte_budget() {
        let directory = tempfile::tempdir().unwrap();
        let first = OutputLog::create(directory.path()).await.unwrap();
        let second = OutputLog::create(directory.path()).await.unwrap();
        assert!(Arc::ptr_eq(&first.lease.budget, &second.lease.budget));
        first
            .lease
            .budget
            .used
            .store(MAX_RETAINED_OUTPUT_PER_RUN - 10, Ordering::Release);
        let (_, mut first_stdout, first_stderr) = first.split();
        let (_, mut second_stdout, second_stderr) = second.split();
        let (first, second) = tokio::join!(
            first_stdout.append(b"orbit-first"),
            second_stdout.append(b"orbit-second")
        );
        first.unwrap();
        second.unwrap();
        assert_eq!(first_stdout.written() + second_stdout.written(), 10);
        assert_eq!(
            first_stdout.lease.budget.used.load(Ordering::Acquire),
            MAX_RETAINED_OUTPUT_PER_RUN
        );
        assert!(first_stdout.written() <= 10 && second_stdout.written() <= 10);
        drop((first_stdout, second_stdout, first_stderr, second_stderr));
    }

    #[tokio::test]
    async fn pruning_keeps_live_logs_pinned_until_both_streams_close() {
        let directory = tempfile::tempdir().unwrap();
        let live = OutputLog::create(directory.path()).await.unwrap();
        let live_path = directory.path().join(format!("{}.stdout", live.id()));
        std::fs::OpenOptions::new()
            .write(true)
            .open(&live_path)
            .unwrap()
            .set_len(100 * 1024 * 1024)
            .unwrap();
        live.lease
            .budget
            .used
            .store(100 * 1024 * 1024, Ordering::Release);
        let stale_path = directory.path().join("cmd-orbit-stale.stdout");
        std::fs::File::create(&stale_path)
            .unwrap()
            .set_len(10 * 1024 * 1024)
            .unwrap();
        live.lease
            .budget
            .used
            .fetch_add(10 * 1024 * 1024, Ordering::AcqRel);
        let next = OutputLog::create(directory.path()).await.unwrap();
        assert!(live_path.exists());
        assert!(!stale_path.exists());
        let id = live.id().to_owned();
        let (_, stdout, stderr) = live.split();
        drop(stdout);
        assert!(next.lease.budget.active.lock().unwrap().contains(&id));
        drop(stderr);
        assert!(!next.lease.budget.active.lock().unwrap().contains(&id));
    }
}

/// Bounded stdout/stderr slices for model responses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputPreview {
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub total_stdout_bytes: u64,
    pub total_stderr_bytes: u64,
}
