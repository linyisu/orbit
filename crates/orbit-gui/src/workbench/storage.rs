//! Durable window state. One background writer orders snapshots and retains
//! clipboard files until their private, durable copies have been written.
use crate::backend::Options;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    thread::JoinHandle,
};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SavedTask {
    pub(crate) key: String,
    pub(crate) options: Options,
    pub(crate) title: String,
    pub(crate) text: String,
    pub(crate) paths: Vec<PathBuf>,
    #[serde(default)]
    pub(crate) retry: Option<SavedSend>,
    #[serde(skip)]
    pub(crate) clipboard_files: Vec<Arc<tempfile::NamedTempFile>>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SavedSend {
    pub(crate) id: String,
    pub(crate) session: String,
    pub(crate) text: String,
    pub(crate) paths: Vec<PathBuf>,
    pub(crate) steer: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    version: u32,
    origin: Options,
    pub(crate) active: String,
    pub(crate) tasks: Vec<SavedTask>,
}
impl Snapshot {
    pub(crate) fn new(origin: Options, active: String, tasks: Vec<SavedTask>) -> Self {
        Self {
            version: 1,
            origin,
            active,
            tasks,
        }
    }

    fn matches(&self, options: &Options) -> bool {
        // SSH/WSL and directories belong to individual tabs. Service and data
        // overrides identify the launch profile; fixture runs must stay isolated.
        self.origin.config == options.config
            && self.origin.server == options.server
            && self.origin.database == options.database
            && self.origin.auth_dir == options.auth_dir
    }
}

enum WriteRequest {
    Save(Box<Snapshot>),
    Stop,
}

#[derive(Clone)]
pub(crate) struct Storage {
    sender: mpsc::Sender<WriteRequest>,
}
pub(crate) struct Writer {
    sender: mpsc::Sender<WriteRequest>,
    thread: Option<JoinHandle<()>>,
}
pub(crate) struct Loaded {
    pub(crate) storage: Storage,
    pub(crate) saved: Option<Snapshot>,
    pub(crate) errors: tokio::sync::watch::Receiver<Option<String>>,
}
impl Drop for Writer {
    fn drop(&mut self) {
        // main retains this guard until the application event loop exits. Drain
        // the last queued edit even if the window has already been destroyed.
        let _ = self.sender.send(WriteRequest::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Storage {
    pub(crate) fn load(options: &Options) -> Result<(Loaded, Writer)> {
        let path = state_path(options)?;
        let saved: Option<Snapshot> = match std::fs::read(&path) {
            Ok(bytes) => {
                let snapshot: Snapshot = serde_json::from_slice(&bytes)
                    .with_context(|| format!("无法读取任务恢复文件 {}", path.display()))?;
                anyhow::ensure!(snapshot.version == 1, "任务恢复文件版本不受支持");
                snapshot.matches(options).then_some(snapshot)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let (sender, receiver) = mpsc::channel();
        let (failures, errors) = tokio::sync::watch::channel(None);
        let thread = std::thread::Builder::new()
            .name("orbit-task-storage".into())
            .spawn(move || {
                let mut previous = None;
                while let Ok(request) = receiver.recv() {
                    let WriteRequest::Save(mut snapshot) = request else {
                        break;
                    };
                    let mut stopping = false;
                    // Coalesce bursts of edits, while retaining the newest clipboard
                    // guards. No debounce timer can lose an edit on ordinary exit.
                    while let Ok(request) = receiver.try_recv() {
                        match request {
                            WriteRequest::Save(next) => snapshot = next,
                            WriteRequest::Stop => {
                                stopping = true;
                                break;
                            }
                        }
                    }
                    let result = save(&path, *snapshot, &mut previous);
                    failures.send_if_modified(|current| {
                        let next = result
                            .err()
                            .map(|error| format!("无法保存任务和草稿：{error:#}"));
                        if *current == next {
                            return false;
                        }
                        *current = next;
                        true
                    });
                    if stopping {
                        break;
                    }
                }
            })?;
        Ok((
            Loaded {
                storage: Self {
                    sender: sender.clone(),
                },
                saved,
                errors,
            },
            Writer {
                sender,
                thread: Some(thread),
            },
        ))
    }

    pub(crate) fn save(&self, snapshot: Snapshot) {
        let _ = self.sender.send(WriteRequest::Save(Box::new(snapshot)));
    }
}

fn state_path(options: &Options) -> Result<PathBuf> {
    if let Some(preferences) = &options.preferences {
        return Ok(preferences.with_extension("tasks.json"));
    }
    Ok(std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .context("找不到任务保存目录，请指定 --preferences")?
        .join("orbit/gui-tasks.json"))
}

fn save(path: &Path, mut snapshot: Snapshot, previous: &mut Option<Vec<u8>>) -> Result<()> {
    let directory = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(directory)?;
    let assets = path.with_extension("assets");
    for task in &mut snapshot.tasks {
        for file in &task.clipboard_files {
            if !task.paths.iter().any(|path| path == file.path()) {
                continue;
            }
            std::fs::create_dir_all(&assets)?;
            let destination = assets.join(file.path().file_name().context("图片缺少文件名")?);
            if !destination.exists() {
                let mut copy = tempfile::NamedTempFile::new_in(&assets)?;
                std::io::copy(&mut file.reopen()?, &mut copy)?;
                copy.as_file().sync_all()?;
                copy.persist(&destination)?;
            }
            for path in &mut task.paths {
                if path == file.path() {
                    *path = destination.clone();
                }
            }
            if let Some(retry) = &mut task.retry {
                for path in &mut retry.paths {
                    if path == file.path() {
                        *path = destination.clone();
                    }
                }
            }
        }
    }
    let bytes = serde_json::to_vec_pretty(&snapshot)?;
    if previous.as_ref() == Some(&bytes) {
        return Ok(());
    }
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    *previous = Some(bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn options(directory: &Path) -> Options {
        let mut options = Options::parse_from(["orbit-gui"]);
        options.preferences = Some(directory.join("gui.json"));
        options.workspace = Some(directory.join("project"));
        options
    }

    fn task(options: Options, key: &str) -> SavedTask {
        SavedTask {
            key: key.into(),
            options,
            title: "未发送的任务".into(),
            text: "草稿\n第二行".into(),
            paths: Vec::new(),
            retry: None,
            clipboard_files: Vec::new(),
        }
    }

    #[test]
    fn exit_flushes_latest_tabs_active_selection_and_unsent_text() {
        let directory = tempfile::tempdir().unwrap();
        let options = options(directory.path());
        let (loaded, writer) = Storage::load(&options).unwrap();
        let first = task(options.clone(), "first");
        let mut second = task(options.clone(), "second");
        second.options.resume = Some("saved-session".into());
        second.options.relink_on_resume = true;
        loaded.storage.save(Snapshot::new(
            options.clone(),
            "first".into(),
            vec![first.clone()],
        ));
        loaded.storage.save(Snapshot::new(
            options.clone(),
            "second".into(),
            vec![first, second],
        ));
        drop(writer);
        let (loaded, _writer) = Storage::load(&options).unwrap();
        let restored = loaded.saved.unwrap();
        assert_eq!(restored.active, "second");
        assert_eq!(restored.tasks.len(), 2);
        assert_eq!(restored.tasks[0].text, "草稿\n第二行");
        assert!(restored.tasks[0].options.resume.is_none());
        assert_eq!(
            restored.tasks[1].options.resume.as_deref(),
            Some("saved-session")
        );
        assert!(!restored.tasks[1].options.relink_on_resume);
    }

    #[test]
    fn clipboard_images_and_ambiguous_start_receipts_survive_window_destruction() {
        let directory = tempfile::tempdir().unwrap();
        let options = options(directory.path());
        let (loaded, writer) = Storage::load(&options).unwrap();
        let mut image = tempfile::Builder::new()
            .prefix("orbit-image-")
            .suffix(".png")
            .tempfile()
            .unwrap();
        image.write_all(b"fixture image bytes").unwrap();
        let temporary_path = image.path().to_path_buf();
        let mut draft = task(options.clone(), "one");
        draft.paths = vec![temporary_path.clone()];
        draft.clipboard_files = vec![Arc::new(image)];
        draft.retry = Some(SavedSend {
            id: "accepted-but-not-acknowledged".into(),
            session: "saved-session".into(),
            text: draft.text.clone(),
            paths: draft.paths.clone(),
            steer: false,
        });
        loaded
            .storage
            .save(Snapshot::new(options.clone(), "one".into(), vec![draft]));
        drop(writer);
        assert!(!temporary_path.exists());
        let (loaded, _writer) = Storage::load(&options).unwrap();
        let saved = loaded.saved.unwrap();
        let draft = &saved.tasks[0];
        assert_ne!(draft.paths[0], temporary_path);
        assert_eq!(
            std::fs::read(&draft.paths[0]).unwrap(),
            b"fixture image bytes"
        );
        let retry = draft.retry.as_ref().unwrap();
        assert_eq!(retry.id, "accepted-but-not-acknowledged");
        assert_eq!(retry.paths, draft.paths);
        assert!(!retry.steer);
    }

    #[test]
    fn removed_tabs_stay_closed_and_other_data_profiles_do_not_resume_them() {
        let directory = tempfile::tempdir().unwrap();
        let options = options(directory.path());
        let (loaded, writer) = Storage::load(&options).unwrap();
        loaded.storage.save(Snapshot::new(
            options.clone(),
            "one".into(),
            vec![task(options.clone(), "one"), task(options.clone(), "two")],
        ));
        loaded.storage.save(Snapshot::new(
            options.clone(),
            "two".into(),
            vec![task(options.clone(), "two")],
        ));
        drop(writer);
        let (loaded, writer) = Storage::load(&options).unwrap();
        assert_eq!(
            loaded
                .saved
                .unwrap()
                .tasks
                .iter()
                .map(|task| task.key.as_str())
                .collect::<Vec<_>>(),
            vec!["two"]
        );
        drop(writer);
        let mut other = options.clone();
        other.database = Some(directory.path().join("another.db"));
        let (loaded, _writer) = Storage::load(&other).unwrap();
        assert!(loaded.saved.is_none());
    }

    #[test]
    fn failed_write_preserves_previous_snapshot_and_reports_to_ui() {
        let directory = tempfile::tempdir().unwrap();
        let options = options(directory.path());
        let path = state_path(&options).unwrap();
        let initial = Snapshot::new(
            options.clone(),
            "one".into(),
            vec![task(options.clone(), "one")],
        );
        save(&path, initial.clone(), &mut None).unwrap();
        let previous = std::fs::read(&path).unwrap();
        // Block the clipboard destination with a file to force a deterministic
        // write failure, including when tests are run by a privileged user.
        std::fs::write(path.with_extension("assets"), "blocked").unwrap();
        let (loaded, writer) = Storage::load(&options).unwrap();
        let image = Arc::new(tempfile::NamedTempFile::new().unwrap());
        let mut failed = initial;
        failed.tasks[0].paths.push(image.path().to_path_buf());
        failed.tasks[0].clipboard_files.push(image);
        loaded.storage.save(failed);
        drop(writer);
        assert!(
            loaded
                .errors
                .borrow()
                .as_ref()
                .unwrap()
                .contains("无法保存")
        );
        assert_eq!(std::fs::read(path).unwrap(), previous);
    }
}
