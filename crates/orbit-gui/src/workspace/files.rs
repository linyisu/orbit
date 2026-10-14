//! Workspace-owned file browser and source editor. All remote IO uses the session service.
mod state;

use super::*;
use gpui_kit::component::{
    Selectable,
    input::{Editor, EditorState, Input, InputState},
    list::ListItem,
    resizable::v_resizable,
    spinner::Spinner,
};
use orbit_protocol::files::{Action, Entry, EntryKind, Snapshot};
use state::DraftState;

const EDIT_BYTES: usize = 1_024 * 1_024;

actions!(orbit_gui_files, [SaveFile, RefreshFiles]);

pub(super) fn bind_keys(cx: &mut App) {
    let save = if cfg!(target_os = "macos") {
        "cmd-s"
    } else {
        "ctrl-s"
    };
    cx.bind_keys([
        KeyBinding::new(save, SaveFile, Some("OrbitFiles")),
        KeyBinding::new(save, SaveFile, Some("OrbitFiles > Input")),
        KeyBinding::new("f5", RefreshFiles, Some("OrbitFiles")),
    ]);
}

struct Document {
    path: String,
    original: Option<String>,
    readonly: Option<String>,
    bytes: usize,
}

struct DiskVersion {
    text: String,
    editor: Entity<EditorState>,
}

#[derive(Clone)]
enum Operation {
    List { append: bool },
    Read { path: String },
    Compare { path: String },
    Save,
}

pub(super) struct Files {
    owner: WeakEntity<Workspace>,
    session: String,
    workspace: String,
    readonly: bool,
    focus: FocusHandle,
    path: Entity<InputState>,
    tree: Entity<TreeState>,
    entries: Vec<Entry>,
    directories: Arc<std::collections::HashSet<String>>,
    directory: String,
    next_offset: Option<usize>,
    hidden: bool,
    scan_truncated: bool,
    editor: Entity<EditorState>,
    document: Option<Document>,
    disk: Option<DiskVersion>,
    draft: DraftState,
    busy: bool,
    generation: u64,
    pending: Option<Operation>,
    error: Option<String>,
    status: Option<String>,
    width: Option<Pixels>,
    task: Task<()>,
    editor_events: Option<Subscription>,
}

impl Files {
    pub(super) fn new(
        owner: WeakEntity<Workspace>,
        session: String,
        workspace: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let entity = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = entity.update(cx, |files, cx| {
                if !files.busy && files.document.is_none() {
                    files.browse(".".into(), false, cx);
                }
            });
        });
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .line_number(true)
                .soft_wrap(false)
        });
        let events = Self::subscribe_editor(&editor, cx);
        Self {
            owner,
            session,
            workspace,
            readonly: false,
            focus: cx.focus_handle(),
            path: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(".")
                    .placeholder("工作区相对路径")
            }),
            tree: cx.new(|cx| TreeState::new(cx)),
            entries: vec![],
            directories: Arc::new(std::collections::HashSet::new()),
            directory: ".".into(),
            next_offset: None,
            hidden: false,
            scan_truncated: false,
            editor,
            document: None,
            disk: None,
            draft: DraftState::default(),
            busy: false,
            generation: 0,
            pending: None,
            error: None,
            status: None,
            width: None,
            task: Task::ready(()),
            editor_events: Some(events),
        }
    }

    pub(super) fn belongs_to(&self, session: &str, workspace: &str) -> bool {
        self.session == session && self.workspace == workspace
    }

    pub(super) fn belongs_to_workspace(&self, workspace: &str) -> bool {
        self.workspace == workspace
    }

    pub(super) fn has_unsaved_changes(&self) -> bool {
        self.draft.has_changes()
    }

    pub(super) fn is_saving(&self) -> bool {
        matches!(self.pending, Some(Operation::Save))
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &mut App) {
        if !self.focus.contains_focused(window, cx) {
            self.focus_document(window, cx);
        }
    }

    fn focus_document(&self, window: &mut Window, cx: &mut App) {
        if self
            .document
            .as_ref()
            .is_some_and(|document| document.original.is_some())
        {
            self.editor
                .focus_handle(cx)
                .focus(window, cx);
        } else {
            // Binary previews and an empty editor do not render EditorState.
            self.tree
                .update(cx, |tree, cx| tree.focus(window, cx));
        }
    }

    pub(super) fn bind_session(&mut self, session: String, cx: &mut Context<Self>) {
        if self.session == session {
            return;
        }
        self.invalidate_request("会话已切换，编辑内容已保留", cx);
        self.session = session;
        // Refresh policy and directory metadata without reopening the document or resending a save.
        self.browse(self.directory.clone(), false, cx);
    }

    pub(super) fn disconnected(&mut self, cx: &mut Context<Self>) {
        self.invalidate_request("工作区已断开，编辑内容已保留。重新连接后可继续。", cx);
    }

    fn cancel_read(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.pending,
            Some(Operation::List { .. } | Operation::Read { .. } | Operation::Compare { .. },)
        ) {
            self.invalidate_request("已取消等待，编辑内容已保留", cx);
        }
    }

    fn invalidate_request(&mut self, message: &str, cx: &mut Context<Self>) {
        let saving = matches!(self.pending, Some(Operation::Save));
        if saving {
            self.draft.unconfirm();
        }
        self.generation = self.generation.wrapping_add(1);
        self.task = Task::ready(());
        self.pending = None;
        self.busy = false;
        self.status = None;
        self.error = Some(if saving {
            "连接或会话已改变，保存结果尚未确认。编辑内容已保留，请重新连接后比较磁盘版本。".into()
        } else {
            message.into()
        });
        cx.notify();
    }

    fn subscribe_editor(editor: &Entity<EditorState>, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe(editor, |files, editor, event, cx| {
            if matches!(event, InputEvent::Change) {
                files.draft.edit(
                    files
                        .document
                        .as_ref()
                        .and_then(|document| document.original.as_deref()),
                    editor.read(cx).value().as_str(),
                );
                files.status = None;
                cx.notify();
            }
        })
    }

    fn is_editable(&self) -> bool {
        !self.readonly
            && self
                .document
                .as_ref()
                .is_some_and(|document| document.original.is_some() && document.readonly.is_none())
    }

    fn request(&mut self, request: Request, operation: Operation, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let owner = self.owner.clone();
        let session = self.session.clone();
        let workspace = self.workspace.clone();
        self.busy = true;
        self.pending = Some(operation.clone());
        let generation = self.generation;
        self.error = None;
        self.status = None;
        self.task = cx.spawn(async move |entity, cx| {
            // Callers can still be updating Workspace when they open a file or rebind a session.
            // Dispatch after that update returns so the parent is never borrowed recursively.
            let (result, receiver) = tokio::sync::oneshot::channel();
            let sent = owner
                .update(cx, |view, _| {
                    ensure_current(view, &session, &workspace)?;
                    view.backend
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("工作区已断开，请重新连接后重试"))?
                        .send(Command::Settings { request, result })
                })
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()));
            let dispatched = sent.is_ok();
            let response = match sent {
                Ok(()) => receiver
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|response| response),
                Err(error) => Err(error),
            };
            let _ = entity.update_in(cx, |files, window, cx| {
                if files.generation != generation {
                    return;
                }
                files.busy = false;
                files.pending = None;
                if dispatched && matches!(operation, Operation::Save) && response.is_err() {
                    files.draft.unconfirm();
                }
                let current = files
                    .owner
                    .update(cx, |view, _| ensure_current(view, &session, &workspace))
                    .is_ok_and(|result| result.is_ok());
                if !current {
                    files.error =
                        Some("工作区已切换。编辑内容已保留，请返回原工作区后继续。".into());
                    cx.notify();
                    return;
                }
                match (operation, response) {
                    (
                        Operation::List { append },
                        Ok(Response::Files(Snapshot::Directory {
                            path,
                            entries,
                            next_offset,
                            scan_truncated,
                            readonly,
                        })),
                    ) => {
                        if !append {
                            files.entries.clear();
                        }
                        files.entries.extend(entries);
                        files.directories = Arc::new(
                            files
                                .entries
                                .iter()
                                .filter(|entry| entry.kind == EntryKind::Directory)
                                .map(|entry| entry.path.clone())
                                .collect(),
                        );
                        files.directory = path.clone();
                        files.next_offset = next_offset;
                        files.scan_truncated = scan_truncated;
                        files.readonly = readonly;
                        files
                            .path
                            .update(cx, |input, cx| input.set_value(path, window, cx));
                        let items = files
                            .entries
                            .iter()
                            .map(|entry| {
                                let name = std::path::Path::new(&entry.path)
                                    .file_name()
                                    .and_then(|name| name.to_str())
                                    .unwrap_or(&entry.path);
                                TreeItem::new(entry.path.clone(), name.to_owned())
                            })
                            .collect::<Vec<_>>();
                        files
                            .tree
                            .update(cx, |tree, cx| tree.set_items(items, cx));
                    }
                    (Operation::Read { path }, Ok(Response::File(encoded))) => {
                        match orbit_client::protocol::file_bytes(encoded) {
                            Ok(bytes) => files.receive_file(path, bytes, window, cx),
                            Err(error) => files.error = Some(format!("无法解码文件：{error}")),
                        }
                    }
                    (Operation::Compare { path }, Ok(Response::File(encoded))) => {
                        match orbit_client::protocol::file_bytes(encoded)
                            .map_err(|error| error.to_string())
                            .and_then(|bytes| text_content(bytes).map_err(str::to_owned))
                        {
                            Ok(text) => {
                                if files.draft.is_unconfirmed() {
                                    if let Some(document) = files.document.as_mut() {
                                        document.original = Some(text.clone());
                                        document.bytes = text.len();
                                    }
                                    files
                                        .draft
                                        .confirm(&text, files.editor.read(cx).value().as_str());
                                    files.status = Some(
                                        if files.draft.is_edited() {
                                            "已确认磁盘版本，编辑内容尚未保存"
                                        } else {
                                            "已在磁盘确认"
                                        }
                                        .into(),
                                    );
                                }
                                let editor = cx.new(|cx| {
                                    EditorState::new(window, cx)
                                        .language(language(&path))
                                        .line_number(true)
                                        .soft_wrap(false)
                                        .default_value(text.clone())
                                });
                                files.disk = Some(DiskVersion { text, editor });
                            }
                            Err(error) => {
                                files.error =
                                    Some(format!("无法比较磁盘版本：{error}。编辑内容已保留。"))
                            }
                        }
                    }
                    (Operation::Save, Ok(Response::Files(Snapshot::Saved { path, content }))) => {
                        if let Some(document) = files
                            .document
                            .as_mut()
                            .filter(|document| document.path == path)
                        {
                            document.bytes = content.len();
                            files
                                .draft
                                .confirm(&content, files.editor.read(cx).value().as_str());
                            document.original = Some(content);
                            files.status = Some("已保存".into());
                            files.disk = None;
                        }
                    }
                    (_, Ok(Response::Error(error))) | (_, Err(error)) => files.error = Some(error),
                    _ => files.error = Some("服务没有返回文件数据，请更新工作区服务后重试".into()),
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    fn action(&mut self, action: Action, operation: Operation, cx: &mut Context<Self>) {
        self.request(
            Request::Files {
                session: self.session.clone(),
                expected_workspace: Some(self.workspace.clone()),
                action,
            },
            operation,
            cx,
        );
    }

    fn browse(&mut self, path: String, append: bool, cx: &mut Context<Self>) {
        let offset = if append {
            self.next_offset.unwrap_or(0)
        } else {
            0
        };
        self.action(
            Action::List {
                path,
                offset,
                include_hidden: self.hidden,
            },
            Operation::List { append },
            cx,
        );
    }

    pub(super) fn open(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            if matches!(self.pending, Some(Operation::Save)) {
                self.error = Some(format!("正在保存当前文件，请等待保存结束后再打开 {path}"));
                cx.notify();
                return;
            }
            self.cancel_read(cx);
        }
        if self.has_unsaved_changes() {
            let entity = cx.weak_entity();
            crate::dialog::open(window, cx, move |dialog, _, _| {
                let entity = entity.clone();
                let path = path.clone();
                dialog
                    .title("保留未保存的文件更改？")
                    .child("当前编辑内容尚未保存，或保存结果尚未确认。返回编辑器可比较磁盘版本并保存，也可以明确放弃并打开所选文件。")
                    .footer(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new("keep-file-edit")
                                    .label("返回编辑器")
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("discard-file-edit")
                                    .label("放弃更改并打开")
                                    .danger()
                                    .on_click(move |_, window, cx| {
                                        window.close_dialog(cx);
                                        let _ = entity.update(cx, |files, cx| {
                                            files.discard(window, cx);
                                            files.read(path.clone(), cx);
                                        });
                                    }),
                            ),
                    )
            });
            return;
        }
        self.read(path, cx);
    }

    fn read(&mut self, path: String, cx: &mut Context<Self>) {
        self.request(
            Request::ReadFile {
                session: self.session.clone(),
                path: path.clone(),
            },
            Operation::Read { path },
            cx,
        );
    }

    fn compare(&mut self, cx: &mut Context<Self>) {
        if let Some(document) = &self.document {
            let path = document.path.clone();
            self.request(
                Request::ReadFile {
                    session: self.session.clone(),
                    path: path.clone(),
                },
                Operation::Compare { path },
                cx,
            );
        }
    }

    fn discard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(original) = self
            .document
            .as_ref()
            .and_then(|document| document.original.clone())
        {
            self.editor
                .update(cx, |editor, cx| editor.set_value(original, window, cx));
        }
        self.draft.discard();
        cx.notify();
    }

    pub(super) fn receive_file(
        &mut self,
        path: String,
        bytes: Vec<u8>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.pending, Some(Operation::Save)) {
            self.error = Some("正在保存当前文件，收到的文件预览未替换编辑内容。".into());
            cx.notify();
            return;
        }
        if self.has_unsaved_changes() {
            self.error =
                Some("当前文件有未保存的更改，或保存结果尚未确认。请先比较并保存，或通过“打开文件”明确放弃。".into());
            cx.notify();
            return;
        }
        // A legacy file event can arrive while another read is pending. Its explicit path
        // supersedes that read, whose delayed response must not replace this document.
        self.cancel_read(cx);
        let byte_count = bytes.len();
        let content = text_content(bytes);
        let (original, readonly) = match content {
            Ok(content) => {
                self.editor = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(language(&path))
                        .line_number(true)
                        .soft_wrap(false)
                        .default_value(content.clone())
                });
                self.editor_events = Some(Self::subscribe_editor(&self.editor, cx));
                let readonly = if byte_count > EDIT_BYTES {
                    Some("文件超过 1 MiB 保存上限，当前为只读".into())
                } else {
                    None
                };
                (Some(content), readonly)
            }
            Err(reason) => (None, Some(reason.into())),
        };
        let parent = std::path::Path::new(&path)
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".into());
        self.document = Some(Document {
            path,
            original,
            readonly,
            bytes: byte_count,
        });
        self.disk = None;
        self.draft.discard();
        self.error = None;
        self.status = None;
        let editor = self.editor.entity_id();
        cx.defer_in(window, move |files, window, cx| {
            let current = files
                .owner
                .upgrade()
                .is_some_and(|owner| {
                    let owner = owner.read(cx);
                    owner.visible
                        && owner.page == MainPage::Files
                        && owner
                            .files
                            .as_ref()
                            .is_some_and(|current| current.entity_id() == cx.entity_id())
                });
            // A delayed read must not move focus back to a hidden page, an
            // obsolete editor, or behind a newly opened modal surface.
            if current
                && files.editor.entity_id() == editor
                && !window.has_active_dialog(cx)
                && !window.has_active_sheet(cx)
            {
                files.focus_document(window, cx);
            }
        });
        if self.entries.is_empty() {
            let entity = cx.weak_entity();
            cx.defer(move |cx| {
                let _ = entity.update(cx, |files, cx| files.browse(parent, false, cx));
            });
        }
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy || !self.draft.is_edited() || !self.is_editable() {
            return;
        }
        self.save_with(None, cx);
    }

    fn save_with(&mut self, original: Option<String>, cx: &mut Context<Self>) {
        if self.busy || !self.is_editable() {
            return;
        }
        if self.draft.is_unconfirmed() {
            self.error =
                Some("上次保存结果尚未确认，请先比较磁盘版本再保存。编辑内容已保留。".into());
            cx.notify();
            return;
        }
        let document = self.document.as_ref().unwrap();
        let content = self.editor.read(cx).value().to_string();
        if content.len() > EDIT_BYTES || content.contains('\0') {
            self.error = Some("文件内容不能含 NUL，且不能超过 1 MiB。编辑内容已保留。".into());
            cx.notify();
            return;
        }
        self.action(
            Action::Save {
                path: document.path.clone(),
                original: original.unwrap_or_else(|| document.original.clone().unwrap()),
                content,
            },
            Operation::Save,
            cx,
        );
    }

    fn selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self
            .tree
            .read(cx)
            .selected_item()
            .map(|item| item.id.to_string());
        if let Some(path) = path {
            self.activate(path, window, cx);
        }
    }

    fn activate(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .entries
            .iter()
            .any(|entry| entry.path == path && entry.kind == EntryKind::Directory)
        {
            self.browse(path, false, cx);
        } else {
            self.open(path, window, cx);
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let owner = self.owner.clone();
        if self.has_unsaved_changes() {
            let entity = cx.weak_entity();
            crate::dialog::open(window, cx, move |dialog, _, _| {
                let owner = owner.clone();
                let entity = entity.clone();
                dialog
                    .title("关闭未保存的文件？")
                    .child("当前编辑内容尚未保存，或保存结果尚未确认。返回编辑器可比较磁盘版本并保存。")
                    .footer(
                        h_flex()
                            .justify_end()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new("cancel-close-file")
                                    .label("返回编辑器")
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .child(
                                Button::new("discard-close-file")
                                    .label("放弃更改并关闭")
                                    .danger()
                                    .on_click(move |_, window, cx| {
                                        window.close_dialog(cx);
                                        let _ = entity
                                            .update(cx, |files, cx| files.discard(window, cx));
                                        let _ = owner
                                            .update(cx,
                                            |view,
                                            cx| view
                                            .close_files(window,
                                            cx));
                                    }),
                            ),
                    )
            });
        } else {
            let _ = owner.update(cx, |view, cx| view.close_files(window, cx));
        }
    }

    fn browser(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.weak_entity();
        let directories = self.directories.clone();
        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("files-parent")
                            .ghost()
                            .small()
                            .label("上级")
                            .disabled(self.busy || self.directory == ".")
                            .on_click(cx.listener(|files, _, _, cx| {
                                let parent = std::path::Path::new(&files.directory)
                                    .parent()
                                    .filter(|path| !path.as_os_str().is_empty())
                                    .map(|path| path.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| ".".into());
                                files.browse(parent, false, cx);
                            })),
                    )
                    .child(
                        Button::new("files-hidden")
                            .ghost()
                            .small()
                            .label("隐藏文件")
                            .selected(self.hidden)
                            .tooltip("显示或隐藏以点开头的文件")
                            .disabled(self.busy)
                            .on_click(cx.listener(|files, _, _, cx| {
                                files.hidden = !files.hidden;
                                files.browse(files.directory.clone(), false, cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .capture_key_down(cx.listener(|files, event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key == "enter" {
                            files.selected(window, cx);
                            cx.stop_propagation();
                        }
                    }))
                    .child(Tree::new(&self.tree, move |_, entry, _, _, _| {
                        let path = entry.item().id.to_string();
                        let directory = directories.contains(&path);
                        let entity = entity.clone();
                        ListItem::new(entry.item().id.clone())
                            .accessibility_label(entry.item().label.clone())
                            .child(
                                h_flex()
                                    .gap_2()
                                    .min_w_0()
                                    .child(
                                        Icon::new(if directory {
                                            IconName::Folder
                                        } else {
                                            IconName::File
                                        })
                                        .size_4(),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .text_ellipsis()
                                            .child(entry.item().label.clone()),
                                    ),
                            )
                            .on_click(move |_, window, cx| {
                                let _ = entity.update(cx, |files, cx| {
                                    files.activate(path.clone(), window, cx)
                                });
                            })
                    })),
            )
            .when(self.entries.is_empty() && !self.busy, |view| {
                view.child(
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("目录中没有可浏览的文件"),
                )
            })
            .when(self.scan_truncated, |view| {
                view.child(
                    div()
                        .p_3()
                        .text_sm()
                        .child("目录条目较多，请进入子目录继续浏览"),
                )
            })
            .when_some(self.next_offset, |view, _| {
                view.child(
                    Button::new("files-more")
                        .ghost()
                        .small()
                        .label("加载更多")
                        .disabled(self.busy)
                        .on_click(cx.listener(|files, _, _, cx| {
                            files.browse(files.directory.clone(), true, cx)
                        })),
                )
            })
    }

    fn editor_surface(&self, cx: &mut Context<Self>) -> AnyElement {
        if !self
            .document
            .as_ref()
            .is_some_and(|document| document.original.is_some())
        {
            return div()
                .flex_1()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(if self.document.is_some() {
                    "二进制或非 UTF-8 文件无法在文本编辑器中编辑"
                } else {
                    "选择文件开始编辑。支持 UTF-8 文本，保存上限为 1 MiB。"
                })
                .into_any_element();
        }
        let edited = Editor::new(&self.editor)
            .aria_label("文件内容")
            .bordered(false)
            .readonly(!self.is_editable() || self.busy)
            .size_full();
        let Some(disk) = &self.disk else {
            return div()
                .flex_1()
                .min_h_0()
                .child(edited)
                .into_any_element();
        };
        div()
            .flex_1()
            .min_h_0()
            .child(
                v_resizable("workspace-file-comparison")
                    .child(
                        resizable_panel().child(
                            v_flex()
                                .size_full()
                                .min_h_0()
                                .child(
                                    div()
                                        .px_3()
                                        .py_1()
                                        .text_sm()
                                        .child("编辑内容"),
                                )
                                .child(div().flex_1().min_h_0().child(edited)),
                        ),
                    )
                    .child(
                        resizable_panel().child(
                            v_flex()
                                .size_full()
                                .min_h_0()
                                .child(
                                    h_flex()
                                        .px_3()
                                        .py_1()
                                        .gap_2()
                                        .flex_wrap()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_sm()
                                                .child("磁盘版本 · 只读"),
                                        )
                                        .child(
                                            Button::new("save-compared-file")
                                                .small()
                                                .label("用编辑内容替换磁盘版本")
                                                .disabled(self.busy || !self.is_editable())
                                                .on_click(cx.listener(|files, _, _, cx| {
                                                    let original = files
                                                        .disk
                                                        .as_ref()
                                                        .map(|disk| disk.text.clone());
                                                    if let Some(original) = original {
                                                        files.save_with(Some(original), cx);
                                                    }
                                                })),
                                        )
                                        .child(
                                            Button::new("close-file-comparison")
                                                .ghost()
                                                .small()
                                                .label("关闭比较")
                                                .on_click(cx.listener(|files, _, _, cx| {
                                                    files.disk = None;
                                                    cx.notify();
                                                })),
                                        ),
                                )
                                .child(
                                    div().flex_1().min_h_0().child(
                                        Editor::new(&disk.editor)
                                            .aria_label("磁盘文件内容")
                                            .bordered(false)
                                            .readonly(true)
                                            .size_full(),
                                    ),
                                ),
                        ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for Files {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = self
            .width
            .unwrap_or(window.viewport_size().width)
            < window.rem_size() * 46.;
        let owner = cx.weak_entity();
        let measured = self.width;
        let title = self
            .document
            .as_ref()
            .map(|document| document.path.clone())
            .unwrap_or_else(|| "文件".into());
        let state = if self.document.is_none() && !self.busy {
            "选择文件"
        } else if self.busy {
            "正在读取或保存"
        } else if self.draft.is_unconfirmed() {
            "保存结果尚未确认"
        } else if self.draft.is_edited() {
            "未保存"
        } else {
            self.status
                .as_deref()
                .unwrap_or("与磁盘一致")
        };
        let editor = v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .when_some(
                self.document
                    .as_ref()
                    .and_then(|document| document.readonly.clone()),
                |view, reason| {
                    view.child(
                        div()
                            .px_3()
                            .py_2()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(reason),
                    )
                },
            )
            .child(self.editor_surface(cx));
        let panes = if narrow {
            v_resizable("workspace-files-vertical")
                .child(
                    resizable_panel()
                        .size((window.rem_size() * 8.).min(window.viewport_size().height * 0.25))
                        .size_range(window.rem_size() * 3. ..window.rem_size() * 16.)
                        .child(self.browser(cx)),
                )
                .child(resizable_panel().child(editor))
                .into_any_element()
        } else {
            h_resizable("workspace-files-horizontal")
                .child(
                    resizable_panel()
                        .size(window.rem_size() * 15.)
                        .size_range(window.rem_size() * 10. ..window.rem_size() * 24.)
                        .child(self.browser(cx)),
                )
                .child(
                    resizable_panel()
                        .min_w(rems(18.))
                        .child(editor),
                )
                .into_any_element()
        };
        v_flex()
            .size_full()
            .relative()
            .min_w_0()
            .min_h_0()
            .when(self.readonly, |view| {
                view.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("当前会话禁止文件写入，文件为只读"),
                )
            })
            .key_context("OrbitFiles")
            .track_focus(&self.focus)
            .on_action(cx.listener(|files, _: &SaveFile, _, cx| files.save(cx)))
            .on_action(cx.listener(|files, _: &RefreshFiles, _, cx| {
                files.browse(files.directory.clone(), false, cx)
            }))
            .border_l_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .min_h_9()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .flex_wrap()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_ellipsis()
                            .child(title),
                    )
                    .when(self.busy, |view| view.child(Spinner::new().small()))
                    .when(
                        self.busy && !matches!(self.pending, Some(Operation::Save)),
                        |view| {
                            view.child(
                                Button::new("cancel-file-read")
                                    .ghost()
                                    .small()
                                    .label("取消等待")
                                    .tooltip("取消读取或目录加载，保留当前编辑内容")
                                    .on_click(cx.listener(|files, _, _, cx| files.cancel_read(cx))),
                            )
                        },
                    )
                    .child(
                        Button::new("save-file")
                            .small()
                            .label("保存")
                            .disabled(
                                self.busy
                                    || !self.draft.is_edited()
                                    || !self.is_editable()
                                    || self.draft.is_unconfirmed(),
                            )
                            .tooltip("保存原始文件 · Ctrl+S")
                            .on_click(cx.listener(|files, _, _, cx| files.save(cx))),
                    )
                    .child(
                        Button::new("compare-file")
                            .ghost()
                            .small()
                            .label("比较磁盘版本")
                            .disabled(self.busy || self.document.is_none())
                            .on_click(cx.listener(|files, _, _, cx| files.compare(cx))),
                    )
                    .child(
                        Button::new("reload-file")
                            .ghost()
                            .small()
                            .label("重新读取…")
                            .disabled(self.busy || self.document.is_none())
                            .on_click(cx.listener(|files, _, window, cx| {
                                if let Some(document) = &files.document {
                                    files.open(document.path.clone(), window, cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("close-files")
                            .ghost()
                            .small()
                            .label("关闭")
                            .disabled(self.busy)
                            .on_click(cx.listener(|files, _, window, cx| files.close(window, cx))),
                    ),
            )
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        Input::new(&self.path)
                            .small()
                            .aria_label("文件或目录路径")
                            .flex_1()
                            .min_w_0(),
                    )
                    .child(
                        Button::new("browse-path")
                            .small()
                            .label("浏览目录")
                            .disabled(self.busy)
                            .on_click(cx.listener(|files, _, _, cx| {
                                files.browse(files.path.read(cx).value().to_string(), false, cx)
                            })),
                    )
                    .child(
                        Button::new("open-path")
                            .small()
                            .label("打开文件")
                            .disabled(self.busy)
                            .on_click(cx.listener(|files, _, window, cx| {
                                files.open(files.path.read(cx).value().to_string(), window, cx)
                            })),
                    ),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .when(self.draft.is_unconfirmed(), |view| {
                view.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(cx.theme().warning)
                        .child("保存结果尚未确认。编辑内容已保留，请比较磁盘版本后继续。"),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(panes),
            )
            .child(
                h_flex()
                    .px_3()
                    .py_1()
                    .gap_3()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(state.to_owned())
                    .when_some(self.document.as_ref(), |view, document| {
                        view.child(format!("{} 字节", document.bytes))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_ellipsis()
                            .child(self.workspace.clone()),
                    ),
            )
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let width = bounds.size.width;
                        if width > px(0.) && measured != Some(width) {
                            let owner = owner.clone();
                            cx.defer(move |cx| {
                                let _ = owner.update(cx, |files, cx| {
                                    if files.width != Some(width) {
                                        files.width = Some(width);
                                        cx.notify();
                                    }
                                });
                            });
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }
}

fn ensure_current(view: &Workspace, session: &str, workspace: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        view.info
            .as_ref()
            .is_some_and(|info| info.id == session && info.workspace == workspace),
        "工作区已切换，请在当前项目中重新打开文件"
    );
    Ok(())
}

fn text_content(bytes: Vec<u8>) -> Result<String, &'static str> {
    if bytes.contains(&0) {
        return Err("文件含二进制数据，无法编辑");
    }
    String::from_utf8(bytes).map_err(|_| "文件不是 UTF-8 文本，无法编辑")
}

use orbit_syntax::label_for_path as language;

#[cfg(test)]
mod tests {
    use super::*;
    #[::core::prelude::v1::test]
    fn editor_uses_exact_utf8_text_and_rejects_binary() {
        let text = "```rust\r\n你好\r\n```\r\n";
        assert_eq!(text_content(text.as_bytes().to_vec()).unwrap(), text);
        assert!(text_content(vec![0, 1, 2]).is_err());
        assert!(text_content(vec![0xff, 0xfe]).is_err());
    }
}
