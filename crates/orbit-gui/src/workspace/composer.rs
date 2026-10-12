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
    pub(super) fn accepted(&mut self, text: &str) {
        if !text.trim().is_empty() && self.entries.last().is_none_or(|last| last != text) {
            self.entries.push(text.to_owned());
        }
        self.index = None;
        self.draft = None;
    }

    pub(super) fn changed(&mut self, text: &str) {
        if self.index.is_some_and(|index| self.entries[index] != text) {
            self.index = None;
            self.draft = None;
        }
    }

    fn navigate(&mut self, previous: bool, current: Draft, images: bool) -> Option<Draft> {
        // A history entry contains text only. Never pair an old prompt with a
        // current image draft or resurrect previously uploaded attachments.
        if images || self.entries.is_empty() {
            return None;
        }
        self.changed(&current.text);
        let index = if previous {
            if self.index.is_none() {
                self.draft = Some(current);
            }
            self.index.unwrap_or(self.entries.len()).saturating_sub(1)
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

pub(super) fn is_image(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif"
            )
        })
}

fn references(paths: &[PathBuf]) -> (Vec<PathBuf>, String) {
    let (images, files): (Vec<_>, Vec<_>) = paths.iter().cloned().partition(|path| is_image(path));
    let references = files
        .iter()
        .map(|path| format!("[文件：{}]", path.display()))
        .collect::<Vec<_>>()
        .join("\n");
    (images, references)
}

fn image_directory(workspace: &std::path::Path, remote: bool, desktop: PathBuf) -> PathBuf {
    if remote || workspace.as_os_str().is_empty() {
        desktop
    } else {
        workspace.into()
    }
}

impl Workspace {
    pub(super) fn pasted_paths(&self, text: &str) -> Option<Vec<PathBuf>> {
        let directory = image_directory(
            std::path::Path::new(self.location.as_ref()),
            self.options.ssh.is_some() || self.options.wsl.is_some(),
            std::env::current_dir().ok()?,
        );
        orbit_client::attachments::pasted_paths(text, &directory)
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
        let (images, references) = references(paths);
        if !references.is_empty() {
            self.input
                .update(cx, |input, cx| input.replace(references, window, cx));
            let draft = self.input.read(cx).value();
            self.history.changed(&draft);
            self.update_completions(&draft, window, cx);
        }
        self.add_images(&images, cx);
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
    use super::{Draft, History, image_directory, references};
    use std::{ops::Range, path::PathBuf};

    fn draft(text: &str, selection: Range<usize>) -> Draft {
        Draft {
            text: text.into(),
            selection,
        }
    }

    #[test]
    fn accepted_history_restores_multiline_draft_and_selection_and_detaches_on_edit() {
        let mut history = History::default();
        history.accepted("first prompt");
        history.accepted("第二条\n保留缩进");
        let original = draft("unfinished\n草稿", 11..17);
        let second = history.navigate(true, original.clone(), false).unwrap();
        assert_eq!(second.text, "第二条\n保留缩进");
        history.changed(&second.text); // Owner-supplied synchronization is not an edit.
        let first = history.navigate(true, second.clone(), false).unwrap();
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
    fn mixed_clipboard_paths_keep_regular_files_as_text_and_images_as_attachments() {
        let (images, text) = references(&[
            "/tmp/screen.PNG".into(),
            "/tmp/design notes.md".into(),
            "/tmp/项目/main.rs".into(),
            "/tmp/diagram.webp".into(),
        ]);
        assert_eq!(
            images,
            [
                PathBuf::from("/tmp/screen.PNG"),
                PathBuf::from("/tmp/diagram.webp")
            ]
        );
        assert_eq!(
            text,
            "[文件：/tmp/design notes.md]\n[文件：/tmp/项目/main.rs]"
        );
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
