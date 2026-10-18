//! Scoped stdout/stderr storage. Persistent sessions retain bounded logs across runs;
//! temporary callers release them when the run and outstanding I/O leases end.
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[cfg(unix)]
use std::fs::OpenOptions;

use super::OutputLog;
use crate::error::{RuntimeError, failed};

#[derive(Default)]
struct State {
    generation: u64,
    active: Option<Active>,
    storage: Storage,
}

#[derive(Default)]
enum Storage {
    #[default]
    Temporary,
    Persistent(PathBuf),
}

struct Active {
    generation: u64,
    directory: Option<Arc<RunDirectory>>,
}

#[derive(Clone, Default)]
pub struct CommandOutputs(Arc<Mutex<State>>);

/// Ends log creation when dropped. Persistent stores remain readable; temporary
/// stores expire after outstanding I/O leases finish.
pub struct OutputRun {
    outputs: CommandOutputs,
    generation: u64,
}

#[derive(Clone)]
pub struct OutputLease(Arc<RunDirectory>);

pub(super) enum RunDirectory {
    Temporary {
        directory: tempfile::TempDir,
        _lock: File,
    },
    Persistent(PathBuf),
}

impl RunDirectory {
    fn path(&self) -> &Path {
        match self {
            Self::Temporary { directory, .. } => directory.path(),
            Self::Persistent(path) => path,
        }
    }
}

impl CommandOutputs {
    /// Bind logs to a service-authorized session directory. Allocation stays lazy;
    /// the owner must hold the session lease and remove logs when deleting the session.
    pub fn persistent(directory: PathBuf) -> Self {
        Self(Arc::new(Mutex::new(State {
            storage: Storage::Persistent(directory),
            ..State::default()
        })))
    }

    /// Use a per-user directory and validate its ownership before accepting output leases.
    pub fn root() -> PathBuf {
        #[cfg(unix)]
        {
            std::env::temp_dir().join(format!("orbit-command-output-{}", unsafe {
                libc::geteuid()
            }))
        }
        #[cfg(windows)]
        {
            std::env::temp_dir().join("orbit-command-output")
        }
    }

    pub fn begin_run(&self) -> Result<OutputRun, RuntimeError> {
        let mut state = self
            .0
            .lock()
            .expect("command output lock poisoned");
        if state.active.is_some() {
            return Err(failed("command output task is already active"));
        }
        state.generation += 1;
        let generation = state.generation;
        state.active = Some(Active {
            generation,
            directory: None,
        });
        Ok(OutputRun {
            outputs: self.clone(),
            generation,
        })
    }

    pub fn lease(&self) -> Option<OutputLease> {
        let state = self
            .0
            .lock()
            .expect("command output lock poisoned");
        if let Some(directory) = state
            .active
            .as_ref()
            .and_then(|active| active.directory.clone())
        {
            return Some(OutputLease(directory));
        }
        match &state.storage {
            Storage::Temporary => None,
            Storage::Persistent(path) => {
                validate_persistent(path).ok()?;
                Some(OutputLease(Arc::new(RunDirectory::Persistent(
                    path.clone(),
                ))))
            }
        }
    }

    pub async fn create_log(&self) -> Result<OutputLog, RuntimeError> {
        let generation = self
            .0
            .lock()
            .expect("command output lock poisoned")
            .active
            .as_ref()
            .ok_or_else(|| failed("no active command output task"))?
            .generation;
        let outputs = self.clone();
        let lease = tokio::task::spawn_blocking(move || {
            let mut state = outputs
                .0
                .lock()
                .expect("command output lock poisoned");
            let persistent = match &state.storage {
                Storage::Temporary => None,
                Storage::Persistent(path) => Some(path.clone()),
            };
            let active = state
                .active
                .as_mut()
                .filter(|active| active.generation == generation)
                .ok_or_else(|| {
                    failed("command output is available only during the current task")
                })?;
            if active.directory.is_none() {
                let directory = match persistent {
                    Some(path) => create_persistent(path)?,
                    None => create_directory(&Self::root())?,
                };
                active.directory = Some(Arc::new(directory));
            }
            Ok::<_, RuntimeError>(OutputLease(
                active
                    .directory
                    .as_ref()
                    .unwrap()
                    .clone(),
            ))
        })
        .await
        .map_err(failed)??;
        let mut log = OutputLog::create(lease.path()).await?;
        Arc::get_mut(&mut log.lease)
            .expect("new output lease is exclusive")
            .directory = Some(lease.0);
        Ok(log)
    }

    /// Save the final capture receipt atomically; failure does not change the command exit.
    pub async fn record_result(
        &self,
        result: &super::super::ExecutionResult,
    ) -> Result<(), RuntimeError> {
        let Some(id) = result.output_id.clone() else {
            return Ok(());
        };
        let lease = self
            .lease()
            .ok_or_else(|| failed("command output store is unavailable"))?;
        let bytes = serde_json::to_vec(&super::CaptureReceipt {
            stdout_bytes: result.stdout_total_bytes,
            stderr_bytes: result.stderr_total_bytes,
            truncated: result.retained_output_truncated,
            error: result.retained_output_error.clone(),
        })
        .map_err(failed)?;
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            let budget = super::output_budget(lease.path())?;
            let _active = budget
                .active
                .lock()
                .expect("output log lock poisoned");
            if !lease
                .path()
                .join(format!("{id}.stdout"))
                .is_file()
                || !lease
                    .path()
                    .join(format!("{id}.stderr"))
                    .is_file()
            {
                return Err(failed(
                    "command output was evicted before saving the capture receipt",
                ));
            }
            let mut file = tempfile::NamedTempFile::new_in(lease.path())?;
            file.write_all(&bytes)?;
            file.as_file().sync_all()?;
            file.persist_noclobber(lease.path().join(format!("{id}.json")))
                .map_err(failed)?;
            budget
                .used
                .fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::AcqRel);
            Ok::<_, RuntimeError>(())
        })
        .await
        .map_err(failed)?
    }
}

impl OutputLease {
    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for OutputRun {
    fn drop(&mut self) {
        let mut state = self
            .outputs
            .0
            .lock()
            .expect("command output lock poisoned");
        let retired = if state
            .active
            .as_ref()
            .is_some_and(|active| active.generation == self.generation)
        {
            state.active.take()
        } else {
            None
        };
        drop(state);
        drop(retired);
    }
}

fn create_directory(root: &Path) -> Result<RunDirectory, RuntimeError> {
    #[cfg(windows)]
    crate::filesystem::private::create_directory(root)?;
    #[cfg(unix)]
    match std::fs::DirBuilder::new()
        .mode(0o700)
        .create(root)
    {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    validate_private_directory(root)?;
    // Serialize creation against orphan collection in other Orbit processes.
    let maintenance = private_lock(&root.join(".maintenance.lock"), true)?;
    let started = std::time::Instant::now();
    loop {
        match maintenance.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock)
                if started.elapsed() < std::time::Duration::from_secs(3) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) => {
                return Err(failed(format!(
                    "command output maintenance is busy: {error}"
                )));
            }
        }
    }
    prune_orphans(root)?;
    let mut builder = tempfile::Builder::new();
    builder.prefix("run-");
    #[cfg(unix)]
    builder.permissions(std::fs::Permissions::from_mode(0o700));
    let directory = builder.tempdir_in(root)?;
    validate_private_directory(directory.path())?;
    let lock = private_lock(&directory.path().join("owner.lock"), true)?;
    lock.lock()?;
    Ok(RunDirectory::Temporary {
        directory,
        _lock: lock,
    })
}

#[cfg(windows)]
fn validate_private_directory(path: &Path) -> Result<(), RuntimeError> {
    crate::filesystem::private::validate_directory(path).map_err(Into::into)
}

#[cfg(unix)]
fn validate_private_directory(path: &Path) -> Result<(), RuntimeError> {
    let metadata = path.symlink_metadata()?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(failed(
            "command output must be a private directory owned by this user",
        ));
    }
    Ok(())
}

fn validate_persistent(path: &Path) -> Result<(), RuntimeError> {
    let parent = path
        .parent()
        .ok_or_else(|| failed("missing command output parent"))?;
    validate_private_directory(parent)?;
    validate_private_directory(path)
}

fn create_persistent(path: PathBuf) -> Result<RunDirectory, RuntimeError> {
    let parent = path
        .parent()
        .ok_or_else(|| failed("missing command output parent"))?;
    for directory in [parent, path.as_path()] {
        #[cfg(windows)]
        crate::filesystem::private::create_directory(directory)?;
        #[cfg(unix)]
        match std::fs::DirBuilder::new()
            .mode(0o700)
            .create(directory)
        {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
        validate_private_directory(directory)?;
    }
    Ok(RunDirectory::Persistent(path))
}

#[cfg(windows)]
fn private_lock(path: &Path, create: bool) -> Result<File, RuntimeError> {
    crate::filesystem::private::open_file(path, create, false).map_err(Into::into)
}

#[cfg(unix)]
fn private_lock(path: &Path, create: bool) -> Result<File, RuntimeError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(failed("invalid command output ownership lock"));
    }
    Ok(file)
}

/// Only collect our private run directories with an unlocked ownership file.
/// Missing locks, unknown entries and symlinks are never followed or removed.
fn prune_orphans(root: &Path) -> Result<(), RuntimeError> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if !entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(b"run-")
        {
            continue;
        }
        let metadata = match entry.path().symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        #[cfg(unix)]
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            continue;
        }
        #[cfg(windows)]
        match crate::filesystem::private::validate_directory(&entry.path()) {
            Ok(()) => (),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        }
        let Ok(lock) = private_lock(&entry.path().join("owner.lock"), false) else {
            continue;
        };
        if lock.try_lock().is_ok() {
            match std::fs::remove_dir_all(entry.path()) {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
