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

/// Maximum in-memory tail bytes captured for one command output stream.
pub const MAX_OUTPUT: usize = 32 * 1_024;
/// Maximum raw log bytes retained for one stdout or stderr stream.
pub const MAX_RETAINED_OUTPUT_PER_STREAM: u64 = 16 * 1_024 * 1_024;
/// Combined retained-log byte budget for one output store (one session in the service).
pub const MAX_RETAINED_OUTPUT_PER_RUN: u64 = 128 * 1_024 * 1_024;

static OUTPUT_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static OUTPUT_BUDGETS: OnceLock<Mutex<HashMap<PathBuf, Weak<OutputBudget>>>> = OnceLock::new();

/// Final capture evidence, separate from the bounded model preview.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct CaptureReceipt {
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub truncated: bool,
    pub error: Option<String>,
}

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
            self.file
                .write_all(&bytes[..allowed as usize])
                .await?;
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
    let directory = directory
        .canonicalize()
        .map_err(failed)?;
    let mut budgets = OUTPUT_BUDGETS
        .get_or_init(Default::default)
        .lock()
        .expect("output budgets lock poisoned");
    budgets.retain(|_, budget| budget.strong_count() != 0);
    if let Some(budget) = budgets
        .get(&directory)
        .and_then(Weak::upgrade)
    {
        return Ok(budget);
    }
    let mut used = 0u64;
    for entry in std::fs::read_dir(&directory).map_err(failed)? {
        let entry = entry.map_err(failed)?;
        let metadata = std::fs::symlink_metadata(entry.path()).map_err(failed)?;
        if metadata.is_file()
            && entry
                .file_name()
                .to_string_lossy()
                .starts_with("cmd-")
        {
            used = used.saturating_add(metadata.len());
        }
    }

    let budget = Arc::new(OutputBudget {
        used: AtomicU64::new(used),
        ..Default::default()
    });
    budgets.insert(directory, Arc::downgrade(&budget));
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
        #[cfg(unix)]
        use std::os::unix::fs::DirBuilderExt;

        #[cfg(windows)]
        crate::filesystem::private::create_directory_all(directory).map_err(failed)?;

        #[cfg(unix)]
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
            .map_err(failed)?;

        let metadata = std::fs::symlink_metadata(directory).map_err(failed)?;

        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(failed("command output directory must be a real directory"));
        }

        let directory = directory
            .canonicalize()
            .map_err(failed)?;
        let budget = output_budget(&directory)?;
        let mut active = budget
            .active
            .lock()
            .expect("output log lock poisoned");
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

#[cfg(windows)]
fn create_output_file(path: &Path) -> Result<std::fs::File, RuntimeError> {
    crate::filesystem::private::open_file(path, true, true).map_err(Into::into)
}

/// Open a retained log or receipt without following a final link or blocking on a FIFO.
/// The caller must hold the owning session's output lease and validate its output identifier.
/// Windows additionally verifies private ownership, DACL and absence of hard links.
pub fn open_retained_file(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    let file = crate::filesystem::private::open_file(path, false, false)?;
    #[cfg(unix)]
    let file = crate::filesystem::open_regular_file(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "command output file is invalid",
        ));
    }
    Ok(file)
}

#[cfg(unix)]
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
    let entries = std::fs::read_dir(directory)
        .map_err(failed)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = std::fs::symlink_metadata(entry.path()).ok()?;

            (metadata.is_file()
                && entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("cmd-"))
            .then_some((
                entry.path(),
                metadata.len(),
                metadata
                    .modified()
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
            ))
        })
        .collect::<Vec<_>>();

    let mut groups = HashMap::<String, Vec<_>>::new();
    for (path, size, modified) in entries {
        let Some(stem) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
        else {
            continue;
        };
        if !active.contains(stem) {
            groups
                .entry(stem.to_owned())
                .or_default()
                .push((path, size, modified));
        }
    }
    let mut groups = groups.into_values().collect::<Vec<_>>();
    groups.sort_by_key(|entries| {
        entries
            .iter()
            .map(|(_, _, modified)| *modified)
            .max()
    });

    for entries in groups {
        if budget.used.load(Ordering::Acquire)
            <= MAX_RETAINED_OUTPUT_PER_RUN.saturating_sub(2 * MAX_RETAINED_OUTPUT_PER_STREAM)
        {
            break;
        }

        // Evict both streams and their capture receipt as one command.
        for (path, size, _) in entries {
            std::fs::remove_file(path).map_err(failed)?;
            let _ = budget
                .used
                .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                    Some(used.saturating_sub(size))
                });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_reads_reject_directories_and_link_aliases() {
        let root = crate::filesystem::temporary_directory("orbit-output-read-").unwrap();
        let source = root.path().join("source");
        let mut file = create_output_file(&source).unwrap();
        std::io::Write::write_all(&mut file, b"captured").unwrap();
        drop(file);
        assert_eq!(
            open_retained_file(&source)
                .unwrap()
                .metadata()
                .unwrap()
                .len(),
            8
        );
        assert!(open_retained_file(root.path()).is_err());
        let alias = root.path().join("alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&source, &alias).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&source, &alias).unwrap();
        assert!(open_retained_file(&alias).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn directory_aliases_share_budget_and_live_log_ownership() {
        let root = crate::filesystem::private_test_directory().unwrap();
        let actual = root.path().join("actual");
        std::fs::create_dir(&actual).unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&actual, &alias).unwrap();
        let first = OutputLog::create(&actual.join("logs"))
            .await
            .unwrap();
        let second = OutputLog::create(&alias.join("logs"))
            .await
            .unwrap();
        let budget = output_budget(&alias.join("logs")).unwrap();
        assert!(Arc::ptr_eq(&budget, &first.lease.budget));
        assert!(Arc::ptr_eq(&budget, &second.lease.budget));
        let active = budget.active.lock().unwrap();
        assert!(active.contains(first.id()));
        assert!(active.contains(second.id()));
    }

    #[tokio::test]
    async fn failed_log_writes_preserve_exit_status_and_drain_large_output() {
        let directory = crate::filesystem::private_test_directory().unwrap();
        let mut log = OutputLog::create(directory.path())
            .await
            .unwrap();
        // A real read-only descriptor makes retention fail independently of
        // the pipe, without relying on disk exhaustion or device files.
        log.stdout = tokio::fs::File::open(
            directory
                .path()
                .join(format!("{}.stdout", log.id())),
        )
        .await
        .unwrap();
        #[cfg(unix)]
        let (shell, args) = (
            "/bin/sh",
            vec![
                "-c",
                "i=0; while [ $i -lt 20000 ]; do printf 'sample output\\n'; i=$((i+1)); done; printf 'FINAL'; printf 'stderr intact' >&2",
            ],
        );
        #[cfg(windows)]
        let (shell, args) = (
            "powershell.exe",
            vec![
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "for ($i=0; $i -lt 20000; $i++) { [Console]::Write(\"sample output`n\") }; [Console]::Write('FINAL'); [Console]::Error.Write('stderr intact')",
            ],
        );
        let child = tokio::process::Command::new(shell)
            .args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let result = super::super::supervisor::run_child_process(
            child,
            std::time::Duration::from_secs(10),
            tokio_util::sync::CancellationToken::new(),
            Some(log),
            "native",
            0,
            0,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.stdout_total_bytes, 280_005);
        assert!(result.stdout.ends_with("FINAL") && result.stdout.len() <= MAX_OUTPUT);
        assert_eq!(result.stderr, "stderr intact");
        assert!(result.retained_output_truncated && result.retained_output_error.is_some());
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn concurrent_logs_share_the_task_byte_budget() {
        let directory = crate::filesystem::private_test_directory().unwrap();
        let first = OutputLog::create(directory.path())
            .await
            .unwrap();
        let second = OutputLog::create(directory.path())
            .await
            .unwrap();
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
            first_stdout
                .lease
                .budget
                .used
                .load(Ordering::Acquire),
            MAX_RETAINED_OUTPUT_PER_RUN
        );
        assert!(first_stdout.written() <= 10 && second_stdout.written() <= 10);
        drop((first_stdout, second_stdout, first_stderr, second_stderr));
    }

    #[tokio::test]
    async fn pruning_removes_complete_command_groups_including_the_capture_receipt() {
        let directory = crate::filesystem::private_test_directory().unwrap();
        let budget = output_budget(directory.path()).unwrap();
        for (suffix, size) in [
            ("stdout", 40_000_000),
            ("stderr", 40_000_000),
            ("json", 100),
        ] {
            std::fs::File::create(
                directory
                    .path()
                    .join(format!("cmd-stale.{suffix}")),
            )
            .unwrap()
            .set_len(size)
            .unwrap();
        }
        budget
            .used
            .store(MAX_RETAINED_OUTPUT_PER_RUN, Ordering::Release);
        let log = OutputLog::create(directory.path())
            .await
            .unwrap();
        for suffix in ["stdout", "stderr", "json"] {
            assert!(
                !directory
                    .path()
                    .join(format!("cmd-stale.{suffix}"))
                    .exists()
            );
        }
        assert!(
            directory
                .path()
                .join(format!("{}.stdout", log.id()))
                .exists()
        );
    }

    #[tokio::test]
    async fn pruning_keeps_live_logs_pinned_until_both_streams_close() {
        let directory = crate::filesystem::private_test_directory().unwrap();
        let live = OutputLog::create(directory.path())
            .await
            .unwrap();
        let live_path = directory
            .path()
            .join(format!("{}.stdout", live.id()));
        std::fs::OpenOptions::new()
            .write(true)
            .open(&live_path)
            .unwrap()
            .set_len(100 * 1_024 * 1_024)
            .unwrap();
        live.lease
            .budget
            .used
            .store(100 * 1_024 * 1_024, Ordering::Release);
        let stale_path = directory
            .path()
            .join("cmd-orbit-stale.stdout");
        std::fs::File::create(&stale_path)
            .unwrap()
            .set_len(10 * 1_024 * 1_024)
            .unwrap();
        live.lease
            .budget
            .used
            .fetch_add(10 * 1_024 * 1_024, Ordering::AcqRel);
        let next = OutputLog::create(directory.path())
            .await
            .unwrap();
        assert!(live_path.exists());
        assert!(!stale_path.exists());
        let id = live.id().to_owned();
        let (_, stdout, stderr) = live.split();
        drop(stdout);
        assert!(
            next.lease
                .budget
                .active
                .lock()
                .unwrap()
                .contains(&id)
        );
        drop(stderr);
        assert!(
            !next
                .lease
                .budget
                .active
                .lock()
                .unwrap()
                .contains(&id)
        );
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
