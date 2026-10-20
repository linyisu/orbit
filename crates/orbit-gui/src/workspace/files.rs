//! Read-only project tree and disk preview. Reads always use the session service.
use super::*;
use gpui_kit::component::{
    input::{Editor, EditorState},
    list::ListItem,
    resizable::ResizableState,
};
use orbit_client::attachments::FilePreview;
use orbit_protocol::files::{Action, Entry, EntryKind, Snapshot};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

const MAX_BROWSED_ENTRIES: usize = 20_000;
const REFRESH_INTERVAL: Duration = Duration::from_millis(500);

actions!(orbit_gui_files, [RefreshFiles]);
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("f5", RefreshFiles, Some("OrbitFiles"))]);
}

#[derive(Clone)]
enum Operation {
    Directory { path: String, offset: usize },
    Read { path: String },
}

struct Document {
    path: String,
    preview: Result<FilePreview, String>,
}

pub(super) struct Files {
    owner: WeakEntity<Workspace>,
    session: String,
    workspace: String,
    focus: FocusHandle,
    tree: Entity<TreeState>,
    listings: BTreeMap<String, Vec<Entry>>,
    items: HashMap<String, TreeItem>,
    directories: Arc<HashSet<String>>,
    queue: VecDeque<(String, usize)>,
    incoming: Vec<Entry>,
    editor: Entity<EditorState>,
    document: Option<Document>,
    pending: Option<Operation>,
    error: Option<String>,
    generation: u64,
    refresh_cursor: usize,
    panes: Entity<ResizableState>,
    task: Task<()>,
    _poll: Task<()>,
    _tree_events: Subscription,
}

impl Files {
    pub(super) fn new(
        owner: WeakEntity<Workspace>,
        session: String,
        workspace: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tree = cx.new(|cx| TreeState::new(cx));
        let events = cx.subscribe(&tree, |view, _, event, cx| {
            if let gpui_kit::component::tree::TreeEvent::Expanded(path) = event
                && !view
                    .listings
                    .contains_key(path.as_ref())
            {
                view.enqueue(path.to_string(), 0, cx);
            }
        });
        let poll = cx.spawn(async move |entity, cx| {
            loop {
                cx.background_executor()
                    .timer(REFRESH_INTERVAL)
                    .await;
                if entity
                    .update(cx, |view, cx| view.refresh(cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let entity = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = entity.update(cx, |view, cx| view.enqueue(".".into(), 0, cx));
        });
        Self {
            owner,
            session,
            workspace,
            focus: cx.focus_handle(),
            tree,
            listings: BTreeMap::new(),
            items: HashMap::new(),
            directories: Arc::new(HashSet::new()),
            queue: VecDeque::new(),
            incoming: vec![],
            editor: cx.new(|cx| {
                EditorState::new(window, cx)
                    .line_number(true)
                    .soft_wrap(true)
                    .scroll_beyond_last_line(Some(0))
            }),
            document: None,
            pending: None,
            error: None,
            generation: 0,
            refresh_cursor: 0,
            panes: cx.new(|_| ResizableState::default()),
            task: Task::ready(()),
            _poll: poll,
            _tree_events: events,
        }
    }

    pub(super) fn belongs_to(&self, session: &str, workspace: &str) -> bool {
        self.session == session && self.workspace == workspace
    }

    pub(super) fn belongs_to_workspace(&self, workspace: &str) -> bool {
        self.workspace == workspace
    }

    pub(super) fn has_unsaved_changes(&self) -> bool {
        false
    }

    pub(super) fn is_saving(&self) -> bool {
        false
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &mut App) {
        if self.document.is_some() {
            self.editor
                .focus_handle(cx)
                .focus(window, cx);
        } else {
            self.tree
                .update(cx, |tree, cx| tree.focus(window, cx));
        }
    }

    pub(super) fn bind_session(&mut self, session: String, cx: &mut Context<Self>) {
        self.disconnected(cx);
        self.session = session;
        self.document = None;
        self.listings.clear();
        self.items.clear();
        self.enqueue(".".into(), 0, cx);
    }

    pub(super) fn disconnected(&mut self, cx: &mut Context<Self>) {
        self.generation = self.generation.wrapping_add(1);
        self.task = Task::ready(());
        self.pending = None;
        self.queue.clear();
        cx.notify();
    }

    fn current(&self, cx: &App) -> bool {
        self.owner
            .upgrade()
            .is_some_and(|owner| {
                let owner = owner.read(cx);
                owner.connected
                    && owner.visible
                    && owner.inspector == Some(inspector::Section::ProjectFiles)
                    && owner.info.as_ref().is_some_and(|info| {
                        info.id == self.session && info.workspace == self.workspace
                    })
            })
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.pending.is_some() || !self.current(cx) {
            return;
        }
        self.refresh_cursor = self.refresh_cursor.wrapping_add(1);
        if self.refresh_cursor.is_multiple_of(2)
            && let Some(document) = &self.document
        {
            self.request(
                Operation::Read {
                    path: document.path.clone(),
                },
                cx,
            );
        } else {
            let directories = self
                .listings
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            let path = directories
                .get((self.refresh_cursor / 2) % directories.len().max(1))
                .cloned()
                .unwrap_or_else(|| ".".into());
            self.enqueue(path, 0, cx);
        }
    }

    fn enqueue(&mut self, path: String, offset: usize, cx: &mut Context<Self>) {
        if !self
            .queue
            .iter()
            .any(|entry| entry == &(path.clone(), offset))
        {
            self.queue.push_back((path, offset));
        }
        self.next(cx);
    }

    fn next(&mut self, cx: &mut Context<Self>) {
        if self.pending.is_none()
            && let Some((path, offset)) = self.queue.pop_front()
        {
            self.request(Operation::Directory { path, offset }, cx);
        }
    }

    pub(super) fn open(&mut self, path: String, _: &mut Window, cx: &mut Context<Self>) {
        self.disconnected(cx);
        self.document = None;
        self.request(Operation::Read { path }, cx);
    }

    fn request(&mut self, operation: Operation, cx: &mut Context<Self>) {
        self.pending = Some(operation.clone());
        let request = match &operation {
            Operation::Directory { path, offset } => Request::Files {
                session: self.session.clone(),
                expected_workspace: Some(self.workspace.clone()),
                action: Action::List {
                    path: path.clone(),
                    offset: *offset,
                    include_hidden: true,
                },
            },
            Operation::Read { path } => Request::ReadFile {
                session: self.session.clone(),
                path: path.clone(),
                allow_missing: false,
            },
        };
        let owner = self.owner.clone();
        let session = self.session.clone();
        let workspace = self.workspace.clone();
        let generation = self.generation;
        self.task = cx.spawn(async move |entity, cx| {
            let (result, response) = tokio::sync::oneshot::channel();
            let sent = owner
                .update(cx, |owner, _| {
                    owner
                        .info
                        .as_ref()
                        .is_some_and(|info| info.id == session && info.workspace == workspace)
                        && owner
                            .backend
                            .as_ref()
                            .is_some_and(|backend| {
                                backend
                                    .send(Command::Settings { request, result })
                                    .is_ok()
                            })
                })
                .unwrap_or(false);
            let result = if sent {
                response
                    .await
                    .unwrap_or_else(|_| Err("读取已中断".into()))
            } else {
                Err("项目已切换，请重新打开文件".into())
            };
            let _ = entity.update_in(cx, |view, window, cx| {
                if view.generation != generation {
                    return;
                }
                view.pending = None;
                match (operation, result) {
                    (
                        Operation::Directory { offset, .. },
                        Ok(Response::Files(Snapshot::Directory {
                            path,
                            entries,
                            next_offset,
                            ..
                        })),
                    ) => {
                        if offset == 0 {
                            view.incoming.clear();
                        }
                        view.incoming.extend(entries);
                        let count = view
                            .listings
                            .iter()
                            .filter(|(cached, _)| **cached != path)
                            .map(|(_, entries)| entries.len())
                            .sum::<usize>()
                            + view.incoming.len();
                        if count > MAX_BROWSED_ENTRIES {
                            view.error = Some("目录较大，已达到浏览缓存上限".into());
                        } else if let Some(next) = next_offset.filter(|next| *next > offset) {
                            view.queue.push_front((path, next));
                        } else {
                            view.listings
                                .insert(path, view.incoming.clone());
                            view.directories = Arc::new(
                                view.listings
                                    .values()
                                    .flatten()
                                    .filter(|entry| entry.kind == EntryKind::Directory)
                                    .map(|entry| entry.path.clone())
                                    .collect(),
                            );
                            let items = directory_items(".", &view.listings, &mut view.items);
                            view.tree
                                .update(cx, |tree, cx| tree.set_items(items, cx));
                            view.error = None;
                        }
                    }
                    (Operation::Read { path }, Ok(Response::File(encoded))) => {
                        match orbit_client::protocol::file_bytes(encoded) {
                            Ok(bytes) => view.receive_file(path, bytes, window, cx),
                            Err(error) => view.error = Some(error.to_string()),
                        }
                    }
                    (Operation::Read { path }, Ok(Response::Error(error)) | Err(error)) => {
                        view.document = Some(Document {
                            path,
                            preview: Err(error),
                        });
                        view.error = None;
                    }
                    (_, Ok(Response::Error(error))) | (_, Err(error)) => view.error = Some(error),
                    _ => view.error = Some("服务未返回文件内容".into()),
                }
                let entity = cx.weak_entity();
                cx.defer(move |cx| {
                    let _ = entity.update(cx, |view, cx| view.next(cx));
                });
                cx.notify();
            });
        });
        cx.notify();
    }

    pub(super) fn receive_file(
        &mut self,
        path: String,
        bytes: Vec<u8>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let preview = FilePreview::from_bytes(bytes);
        if let FilePreview::Text { content, .. } = &preview {
            let same_file = self
                .document
                .as_ref()
                .is_some_and(|document| document.path == path);
            if same_file {
                if self.editor.read(cx).value().as_str() != content {
                    self.editor.update(cx, |editor, cx| {
                        editor.set_value(content.clone(), window, cx)
                    });
                }
            } else {
                self.editor = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(orbit_syntax::label_for_path(&path))
                        .line_number(true)
                        .soft_wrap(true)
                        .scroll_beyond_last_line(Some(0))
                        .default_value(content.clone())
                });
            }
        }
        self.document = Some(Document {
            path,
            preview: Ok(preview),
        });
        self.error = None;
        cx.notify();
    }

    #[cfg(feature = "test-support")]
    pub(super) fn check_project(&self, document: bool, cx: &App) -> anyhow::Result<()> {
        if document {
            anyhow::ensure!(
                self.document
                    .as_ref()
                    .is_some_and(|document| document.path == "src/main.rs"),
                "missing project file"
            );
            anyhow::ensure!(
                self.editor
                    .read(cx)
                    .value()
                    .contains("Changed project"),
                "preview must read disk"
            );
        } else {
            anyhow::ensure!(self.listings.contains_key("."), "missing project root");
        }
        Ok(())
    }

    #[cfg(feature = "test-support")]
    pub(super) fn start_disk_probe(&self, cx: &mut Context<Self>) {
        let root = PathBuf::from(&self.workspace);
        cx.background_spawn(async move {
            std::fs::write(
                root.join("src/main.rs"),
                "// Generated disk update\nfn main() { println!(\"Updated from disk\"); }\n",
            )
            .expect("generated disk write");
            std::fs::write(root.join("generated-new.txt"), "Generated new file\n")
                .expect("generated tree update");
        })
        .detach();
    }

    #[cfg(feature = "test-support")]
    pub(super) fn check_disk_probe(&self, cx: &App) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.editor
                .read(cx)
                .value()
                .contains("Updated from disk"),
            "file did not refresh from disk"
        );
        anyhow::ensure!(
            self.listings
                .get(".")
                .is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|entry| entry.path == "generated-new.txt")
                }),
            "file tree did not refresh from disk"
        );
        Ok(())
    }

    fn browser(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let directories = self.directories.clone();
        let owner = cx.weak_entity();
        div()
            .size_full()
            .py_2()
            .capture_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "enter" {
                    let path = view
                        .tree
                        .read(cx)
                        .selected_item()
                        .map(|item| item.id.to_string());
                    if let Some(path) = path.filter(|path| !view.directories.contains(path)) {
                        view.open(path, window, cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .child(Tree::new(&self.tree, move |_, entry, selected, _, _| {
                let path = entry.item().id.to_string();
                let directory = directories.contains(&path);
                let owner = owner.clone();
                ListItem::new(entry.item().id.clone())
                    .selected(selected)
                    .pl(rems(0.75 + entry.depth() as f32))
                    .pr_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .min_w_0()
                            .child(
                                Icon::new(if directory && entry.is_expanded() {
                                    IconName::FolderOpen
                                } else if directory {
                                    IconName::Folder
                                } else {
                                    IconName::File
                                })
                                .small(),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_ellipsis()
                                    .child(entry.item().label.clone()),
                            ),
                    )
                    .on_click(move |_, window, cx| {
                        if !directory {
                            let _ =
                                owner.update(cx, |view, cx| view.open(path.clone(), window, cx));
                        }
                    })
            }))
    }
}

impl Render for Files {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = self
            .document
            .as_ref()
            .map(|document| orbit_client::directories::display_path(&document.path).into_owned())
            .unwrap_or_else(|| "选择文件以预览".into());
        let content = match self
            .document
            .as_ref()
            .map(|document| &document.preview)
        {
            Some(Ok(FilePreview::Text { .. })) => Editor::new(&self.editor)
                .readonly(true)
                .bordered(false)
                .size_full()
                .into_any_element(),
            Some(Ok(FilePreview::Binary { .. })) => div()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("此文件无法作为文本预览")
                .into_any_element(),
            Some(Err(error)) => div()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(orbit_client::directories::display_diagnostic(error).into_owned())
                .into_any_element(),
            None => div()
                .p_4()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("从文件树选择文件，内容将随磁盘更新")
                .into_any_element(),
        };
        let preview = v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_sm()
                    .text_ellipsis()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(title),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .px_3()
                        .py_2()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(orbit_client::directories::display_diagnostic(&error).into_owned()),
                )
            })
            .when(
                self.document
                    .as_ref()
                    .is_some_and(|document| {
                        matches!(
                            document.preview,
                            Ok(FilePreview::Text {
                                truncated: true,
                                ..
                            })
                        )
                    }),
                |view| {
                    view.child(
                        div()
                            .px_3()
                            .text_xs()
                            .child("文件较大，仅显示文本预览范围"),
                    )
                },
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(content),
            );
        div()
            .size_full()
            .key_context("OrbitFiles")
            .track_focus(&self.focus)
            .on_action(cx.listener(|view, _: &RefreshFiles, _, cx| view.refresh(cx)))
            .child(
                h_resizable("project-file-panes")
                    .with_state(&self.panes)
                    .child(
                        resizable_panel()
                            .size(window.rem_size() * 15.)
                            .size_range(window.rem_size() * 10. ..window.rem_size() * 28.)
                            .child(self.browser(cx)),
                    )
                    .child(resizable_panel().child(preview)),
            )
    }
}

fn directory_items(
    directory: &str,
    listings: &std::collections::BTreeMap<String, Vec<Entry>>,
    retained: &mut std::collections::HashMap<String, TreeItem>,
) -> Vec<TreeItem> {
    listings
        .get(directory)
        .into_iter()
        .flatten()
        .map(|entry| {
            let label = entry
                .path
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&entry.path);
            let mut item = retained
                .get(&entry.path)
                .cloned()
                .unwrap_or_else(|| TreeItem::new(entry.path.clone(), label.to_owned()));
            if entry.kind == EntryKind::Directory {
                item.children = if listings.contains_key(&entry.path) {
                    directory_items(&entry.path, listings, retained)
                } else {
                    vec![
                        TreeItem::new(format!("{}/.pending", entry.path), "加载目录…")
                            .disabled(true),
                    ]
                };
            }
            retained.insert(entry.path.clone(), item.clone());
            item
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[::core::prelude::v1::test]
    fn lazy_directory_updates_preserve_expansion_and_other_loaded_children() {
        let mut listings = std::collections::BTreeMap::from([(
            ".".into(),
            vec![Entry {
                path: "source".into(),
                kind: EntryKind::Directory,
            }],
        )]);
        let mut retained = std::collections::HashMap::new();
        let initial = directory_items(".", &listings, &mut retained);
        assert_eq!(initial[0].children.len(), 1);
        let expanded = initial[0].clone().expanded(true);
        listings.insert(
            "source".into(),
            vec![Entry {
                path: "source/example.rs".into(),
                kind: EntryKind::File,
            }],
        );
        let loaded = directory_items(".", &listings, &mut retained);
        assert!(expanded.is_expanded());
        assert!(loaded[0].is_expanded());
        assert_eq!(loaded[0].children[0].id.as_ref(), "source/example.rs");
        loaded[0].clone().expanded(false);
        let refreshed = directory_items(".", &listings, &mut retained);
        assert!(!refreshed[0].is_expanded());
        assert_eq!(refreshed[0].children.len(), 1);
    }

    #[::core::prelude::v1::test]
    fn editor_uses_exact_utf8_text_and_rejects_binary() {
        let text = "```rust\r\n你好\r\n```\r\n";
        assert!(
            matches!(orbit_client::attachments::FilePreview::from_bytes(text.as_bytes().to_vec()), orbit_client::attachments::FilePreview::Text { content, .. } if content == text)
        );
        assert!(matches!(
            orbit_client::attachments::FilePreview::from_bytes(vec![0, 1, 2]),
            orbit_client::attachments::FilePreview::Binary { .. }
        ));
        assert!(matches!(
            orbit_client::attachments::FilePreview::from_bytes(vec![0xff, 0xfe]),
            orbit_client::attachments::FilePreview::Binary { .. }
        ));
    }
}
