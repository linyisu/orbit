//! Task-scoped stdout/stderr. Drafts allocate nothing; the first command lazily
//! opens a private directory under the system temporary directory. Leases protect active readers/writers.
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use super::OutputLog;
use crate::error::{RuntimeError, failed};

#[derive(Default)]
struct State {
    generation: u64,
    active: Option<Active>,
}

struct Active {
    generation: u64,
    directory: Option<Arc<RunDirectory>>,
}

#[derive(Clone, Default)]
pub struct CommandOutputs(Arc<Mutex<State>>);

/// Ends API access when dropped. Outstanding I/O leases keep files alive only
/// until those operations finish; they cannot reopen an expired task's output.
pub struct OutputRun {
    outputs: CommandOutputs,
    generation: u64,
}

#[derive(Clone)]
pub struct OutputLease(Arc<RunDirectory>);

pub(super) struct RunDirectory {
    directory: tempfile::TempDir,
    _lock: File,
}

impl CommandOutputs {
    pub fn root() -> PathBuf {
        // Native command execution is Unix-only. Include the effective user so
        // separate OS accounts never share the maintenance directory.
        std::env::temp_dir().join(format!("orbit-command-output-{}", unsafe {
            libc::geteuid()
        }))
    }

    pub fn begin_run(&self) -> Result<OutputRun, RuntimeError> {
        let mut state = self.0.lock().expect("command output lock poisoned");
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
        self.0
            .lock()
            .expect("command output lock poisoned")
            .active
            .as_ref()?
            .directory
            .clone()
            .map(OutputLease)
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
            let mut state = outputs.0.lock().expect("command output lock poisoned");
            let active = state
                .active
                .as_mut()
                .filter(|active| active.generation == generation)
                .ok_or_else(|| {
                    failed("command output is available only during the current task")
                })?;
            if active.directory.is_none() {
                active.directory = Some(Arc::new(create_directory(&Self::root())?));
            }
            Ok::<_, RuntimeError>(OutputLease(active.directory.as_ref().unwrap().clone()))
        })
        .await
        .map_err(failed)??;
        let mut log = OutputLog::create(lease.path()).await?;
        Arc::get_mut(&mut log.lease)
            .expect("new output lease is exclusive")
            .directory = Some(lease.0);
        Ok(log)
    }
}

impl OutputLease {
    pub fn path(&self) -> &Path {
        self.0.directory.path()
    }
}

impl Drop for OutputRun {
    fn drop(&mut self) {
        let mut state = self.outputs.0.lock().expect("command output lock poisoned");
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
    match std::fs::DirBuilder::new().mode(0o700).create(root) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    let metadata = root.symlink_metadata()?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(failed(
            "command output root must be a private directory owned by this user",
        ));
    }
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
    let directory = tempfile::Builder::new()
        .prefix("run-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(root)?;
    let lock = private_lock(&directory.path().join("owner.lock"), true)?;
    lock.lock()?;
    Ok(RunDirectory {
        directory,
        _lock: lock,
    })
}

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
        if !entry.file_name().as_encoded_bytes().starts_with(b"run-") {
            continue;
        }
        let metadata = match entry.path().symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
        {
            continue;
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
