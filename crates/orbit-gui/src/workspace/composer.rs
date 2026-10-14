//! Submitted input belongs to this conversation, separately from editor undo.
use super::*;
use std::ops::Range;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Draft {
    text: String,
    selection: Range<usize>,
}

#[derive(Default)]
pub(super) struct History {
    entries: Vec<String>,
    index: Option<usize>,
    draft: Option<Draft>,
}

impl History {
    pub(super) fn seed(&mut self, entries: Vec<String>) {
        if self.entries.is_empty() {
            for entry in entries {
                if !entry.trim().is_empty() && self.entries.last() != Some(&entry) {
                    self.entries.push(entry);
                }
            }
        }
    }

    fn handles_arrow(&self, previous: bool, text: &str, selection: &Range<usize>) -> bool {
        !self.entries.is_empty()
            && selection.is_empty()
            && if previous {
                selection.start == 0 || (self.index.is_some() && selection.start == text.len())
            } else {
                self.index.is_some() && selection.start == text.len()
            }
    }

    pub(super) fn accepted(&mut self, text: &str) {
        if !text.trim().is_empty()
            && self
                .entries
                .last()
                .is_none_or(|last| last != text)
        {
            self.entries.push(text.to_owned());
        }
        self.index = None;
        self.draft = None;
    }

    pub(super) fn changed(&mut self, text: &str) {
        if self
            .index
            .is_some_and(|index| self.entries[index] != text)
        {
            self.index = None;
            self.draft = None;
        }
    }

    /// A history entry contains text only. Never pair an old prompt with a current image draft or
    /// resurrect previously uploaded attachments.
    fn navigate(&mut self, previous: bool, current: Draft, images: bool) -> Option<Draft> {
        if images || self.entries.is_empty() {
            return None;
        }
        self.changed(&current.text);
        let index = if previous {
            if self.index.is_none() {
                self.draft = Some(current);
            }
            self.index
                .unwrap_or(self.entries.len())
                .saturating_sub(1)
        } else {
            let index = self.index? + 1;
            if index == self.entries.len() {
                self.index = None;
                return self.draft.take();
            }
            index
        };
        self.index = Some(index);
        let text = self.entries[index].clone();
        Some(Draft {
            selection: text.len()..text.len(),
            text,
        })
    }
}

pub(super) use orbit_client::attachments::is_image_path as is_image;

const PASTED_TEXT_THRESHOLD_BYTES: usize = 4_096;
const PASTED_TEXT_THRESHOLD_LINES: usize = 32;

pub(super) fn large_paste(text: &str) -> bool {
    text.len() >= PASTED_TEXT_THRESHOLD_BYTES
        || text
            .lines()
            .take(PASTED_TEXT_THRESHOLD_LINES + 1)
            .count()
            > PASTED_TEXT_THRESHOLD_LINES
}

/// Some URI-list readers retain the CRLF terminator. Preserve real filenames before trimming it.
fn file_list_path(path: PathBuf) -> PathBuf {
    if path.is_file() {
        return path;
    }
    path.to_str()
        .and_then(|value| value.strip_suffix('\r'))
        .map(PathBuf::from)
        .filter(|candidate| candidate.is_file())
        .unwrap_or(path)
}

fn image_directory(workspace: &std::path::Path, remote: bool, desktop: PathBuf) -> PathBuf {
    if remote || workspace.as_os_str().is_empty() {
        desktop
    } else {
        workspace.into()
    }
}

impl Workspace {
    /// Release client-owned temporary files only after the draft stops referring to them.
    pub(super) fn prune_draft_attachments(&mut self) {
        self.attachment_names
            .retain(|path, _| self.paths.contains(path));
        self.clipboard_files.retain(|file| {
            self.paths
                .iter()
                .any(|path| path == file.path())
        });
    }

    pub(super) fn pasted_paths(&self, text: &str) -> Option<Vec<PathBuf>> {
        let directory = image_directory(
            std::path::Path::new(self.location.as_ref()),
            self.options.ssh.is_some() || self.options.wsl.is_some(),
            std::env::current_dir().ok()?,
        );
        orbit_client::attachments::pasted_files(text, &directory)
    }

    /// Recover native file-list offers that GPUI's image/text clipboard reader omits.
    pub(super) fn paste_file_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self
            .input
            .focus_handle(cx)
            .contains_focused(window, cx);
        let clipboard = cx.read_from_clipboard();
        if self.pending.is_some() || self.picker_pending || !focused || clipboard.is_some() {
            return;
        }
        cx.stop_propagation();
        let session = self
            .info
            .as_ref()
            .map(|info| info.id.clone());
        self.attachment_imports += 1;
        cx.notify();
        let job = self.runtime.spawn_blocking(|| {
            arboard::Clipboard::new()
                .ok()?
                .get()
                .file_list()
                .ok()
                .map(|paths| {
                    paths
                        .into_iter()
                        .map(file_list_path)
                        .collect::<Vec<_>>()
                })
        });
        let previous = std::mem::replace(&mut self.picker, Task::ready(()));
        self.picker = cx.spawn(async move |owner, cx| {
            previous.await;
            let paths = job.await;
            let _ = owner.update_in(cx, |view, window, cx| {
                view.attachment_imports = view
                    .attachment_imports
                    .saturating_sub(1);
                if view.info.as_ref().map(|info| &info.id) == session.as_ref()
                    && let Ok(Some(paths)) = paths
                {
                    view.add_paths(&paths, window, cx);
                }
                cx.notify();
            });
        });
    }

    pub(super) fn history_arrow(
        &mut self,
        previous: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_open
            || !self.paths.is_empty()
            || window.modifiers() != Modifiers::default()
            || !self
                .input
                .focus_handle(cx)
                .is_focused(window)
        {
            return;
        }
        let input = self.input.read(cx);
        if self
            .history
            .handles_arrow(previous, &input.value(), &input.selected_range())
        {
            self.input_history(previous, window, cx);
            cx.stop_propagation();
        }
    }

    pub(super) fn input_history(
        &mut self,
        previous: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending.is_some() || !self.visible || self.page != MainPage::Conversation {
            return;
        }
        let input = self.input.read(cx);
        let current = Draft {
            text: input.value().to_string(),
            selection: input.selected_range(),
        };
        let Some(draft) = self
            .history
            .navigate(previous, current, !self.paths.is_empty())
        else {
            return;
        };
        self.dismiss_completions(cx);
        self.input.update(cx, |input, cx| {
            input.set_value(draft.text, window, cx);
            input.set_selected_range(draft.selection, cx);
        });
        cx.emit(WorkspaceEvent::DraftChanged);
        cx.notify();
    }

    pub(super) fn add_paths(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pending.is_some() {
            return;
        }
        for path in paths {
            if !path.is_file() {
                continue;
            }
            if is_image(path)
                && self
                    .info
                    .as_ref()
                    .is_some_and(|info| !info.supports_vision)
            {
                self.report_error("当前模型不支持图片，请切换模型后添加。", cx);
                continue;
            }
            if !self.paths.contains(path) {
                self.paths.push(path.clone());
            }
        }
        if self.visible
            && self.page == MainPage::Conversation
            && !window.has_active_dialog(cx)
            && !window.has_active_sheet(cx)
        {
            self.input
                .focus_handle(cx)
                .focus(window, cx);
        }
        cx.emit(WorkspaceEvent::DraftChanged);
        cx.notify();
    }

    pub(crate) fn stop_task(&mut self, cx: &mut Context<Self>) {
        if self.busy
            && self.connected
            && let Some(info) = &self.info
        {
            self.command(
                Command::Request(Request::Cancel {
                    session: info.id.clone(),
                }),
                cx,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Draft, History, file_list_path, image_directory, large_paste};
    use std::{ops::Range, path::PathBuf};

    fn draft(text: &str, selection: Range<usize>) -> Draft {
        Draft {
            text: text.into(),
            selection,
        }
    }

    #[test]
    fn plain_arrows_recall_history_without_stealing_multiline_editing_or_selection() {
        let mut history = History::default();
        history.seed(vec!["old prompt".into(), "latest\nsecond line".into()]);
        assert!(history.handles_arrow(true, "", &(0..0)));
        assert!(!history.handles_arrow(true, "editing\nsecond", &(10..10)));
        assert!(!history.handles_arrow(true, "selected", &(0..4)));
        assert!(!history.handles_arrow(false, "", &(0..0)));
        let recalled = history
            .navigate(true, draft("", 0..0), false)
            .unwrap();
        assert!(history.handles_arrow(true, &recalled.text, &recalled.selection));
        assert!(history.handles_arrow(false, &recalled.text, &recalled.selection));
        assert_eq!(
            history.navigate(false, recalled, false),
            Some(draft("", 0..0))
        );
    }

    #[test]
    fn accepted_history_restores_multiline_draft_and_selection_and_detaches_on_edit() {
        let mut history = History::default();
        history.accepted("first prompt");
        history.accepted("第二条\n保留缩进");
        let original = draft("unfinished\n草稿", 11..17);
        let second = history
            .navigate(true, original.clone(), false)
            .unwrap();
        assert_eq!(second.text, "第二条\n保留缩进");
        // Owner-supplied synchronization is not an edit.
        history.changed(&second.text);
        let first = history
            .navigate(true, second.clone(), false)
            .unwrap();
        assert_eq!(first.text, "first prompt");
        assert_eq!(history.navigate(false, first, false), Some(second.clone()));
        assert_eq!(history.navigate(false, second, false), Some(original));
        let recalled = history
            .navigate(true, draft("new draft", 2..2), false)
            .unwrap();
        history.changed(&format!("{} edited", recalled.text));
        assert_eq!(history.navigate(false, draft("edited", 6..6), false), None);
    }

    #[test]
    fn image_only_sends_and_current_image_drafts_never_recall_an_old_prompt() {
        let mut history = History::default();
        history.accepted("");
        assert!(history.entries.is_empty());
        history.accepted("look at this");
        assert!(
            history
                .navigate(true, draft("new image", 0..0), true)
                .is_none()
        );
        assert!(history.index.is_none());
        assert!(history.draft.is_none());
        history.accepted("look at this");
        assert_eq!(history.entries.len(), 1);
    }

    #[test]
    fn native_file_lists_accept_crlf_without_changing_an_existing_filename() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sample.txt");
        std::fs::write(&path, b"generated file").unwrap();
        assert_eq!(
            file_list_path(PathBuf::from(format!("{}\r", path.display()))),
            path
        );
        assert_eq!(file_list_path(path.clone()), path);
        let special = root.path().join("literal\r");
        std::fs::write(&special, b"literal filename").unwrap();
        assert_eq!(file_list_path(special.clone()), special);
    }

    #[test]
    fn long_pastes_become_files_without_consuming_short_multiline_input() {
        assert!(!large_paste("alpha\nbeta\ngamma"));
        assert!(large_paste(&"z".repeat(4_096)));
        assert!(large_paste(&"line\n".repeat(33)));
        assert!(!large_paste(&"z".repeat(4_095)));
    }

    #[test]
    fn pasted_image_paths_use_the_client_directory_for_remote_workspaces() {
        let desktop = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let desktop_image = desktop.path().join("screen.png");
        let workspace_image = workspace.path().join("screen.png");
        for image in [&desktop_image, &workspace_image] {
            std::fs::write(image, b"validated when imported").unwrap();
        }
        for (remote, expected) in [(true, &desktop_image), (false, &workspace_image)] {
            let directory = image_directory(workspace.path(), remote, desktop.path().into());
            assert_eq!(
                orbit_client::attachments::pasted_paths("screen.png", &directory),
                Some(vec![expected.clone()])
            );
        }
        let directory = image_directory(
            std::path::Path::new("~/remote/project"),
            true,
            desktop.path().into(),
        );
        assert_eq!(directory, desktop.path());
        assert_eq!(
            orbit_client::attachments::pasted_paths("screen.png", &directory),
            Some(vec![desktop_image])
        );
    }
}
