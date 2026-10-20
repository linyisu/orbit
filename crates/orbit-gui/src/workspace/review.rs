//! Retained review surface. Git index transactions and Orbit write receipts
//! have separate sources and exact reviewed-state guards.
use super::*;
use gpui_kit::component::{
    diff::{Diff, DiffFile, DiffState},
    input::{Editor, EditorState},
    list::ListItem,
    tab::{Tab, TabBar},
};
use orbit_protocol::review::{Action, Checkpoint, Entry, Kind, Snapshot};

const KINDS: [(Kind, &str); 2] = [(Kind::Unstaged, "更改"), (Kind::Staged, "已暂存")];

pub(super) struct Review {
    owner: WeakEntity<Workspace>,
    session: String,
    workspace: String,
    kind: Kind,
    entries: Vec<Entry>,
    selected: Option<String>,
    checkpoint: Option<Checkpoint>,
    index_receipt: Option<String>,
    list: Entity<TreeState>,
    tree_items: std::collections::HashMap<String, TreeItem>,
    diff: Entity<DiffState>,
    raw: Option<Entity<EditorState>>,
    next: Option<usize>,
    branch: Option<String>,
    note: SharedString,
    error: Option<SharedString>,
    busy: bool,
    undoing: bool,
    indexing: bool,
    loaded: bool,
    generation: usize,
    width: Option<Pixels>,
    request: Task<()>,
}

impl Review {
    pub(super) fn new(
        owner: WeakEntity<Workspace>,
        session: String,
        workspace: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            owner,
            session,
            workspace,
            kind: Kind::Unstaged,
            entries: Vec::new(),
            selected: None,
            raw: None,
            tree_items: Default::default(),
            checkpoint: None,
            index_receipt: None,
            next: None,
            branch: None,
            note: "选择文件查看差异".into(),
            error: None,
            busy: false,
            undoing: false,
            indexing: false,
            loaded: false,
            generation: 0,
            width: None,
            request: Task::ready(()),
            diff: cx.new(|cx| DiffState::new([], cx)),
            list: cx.new(|cx| TreeState::new(cx)),
        }
    }

    pub(super) fn belongs_to(&self, session: &str, workspace: &str) -> bool {
        self.session == session && self.workspace == workspace
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &mut App) {
        if !self
            .diff
            .focus_handle(cx)
            .contains_focused(window, cx)
        {
            self.list
                .update(cx, |tree, cx| tree.focus(window, cx));
        }
    }

    pub(super) fn activate(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        if self.mutating() {
            return;
        }
        if self.kind == Kind::Checkpoint {
            self.kind = Kind::Unstaged;
        }
        if let Some(path) = path {
            self.selected = Some(path);
            self.fetch(
                Action::List {
                    kind: self.kind,
                    offset: 0,
                },
                cx,
            );
        } else if !self.busy {
            self.fetch(
                Action::List {
                    kind: self.kind,
                    offset: 0,
                },
                cx,
            );
        }
    }

    pub(super) fn activate_checkpoint(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        if self.mutating() {
            return;
        }
        self.kind = Kind::Checkpoint;
        self.raw = None;
        self.selected = None;
        self.checkpoint = None;
        self.index_receipt = None;
        self.entries.clear();
        self.loaded = false;
        self.diff
            .update(cx, |diff, cx| diff.set_files([], cx));
        self.list
            .update(cx, |_, cx| cx.notify());
        self.selected = path;
        self.fetch(
            Action::List {
                kind: Kind::Checkpoint,
                offset: 0,
            },
            cx,
        );
    }

    fn select(&mut self, path: String, cx: &mut Context<Self>) {
        if self.mutating() {
            return;
        }
        if self.selected.as_deref() != Some(&path) {
            self.diff
                .update(cx, |diff, cx| diff.set_files([], cx));
        }
        if self.kind != Kind::Checkpoint
            && let Some(entry) = self
                .entries
                .iter()
                .find(|entry| entry.path == path)
        {
            self.kind = entry.kind;
        }
        self.raw = None;
        self.selected = Some(path.clone());
        self.checkpoint = None;
        self.index_receipt = None;
        self.fetch(
            Action::Diff {
                kind: self.kind,
                path,
            },
            cx,
        );
    }

    pub(super) fn disconnected(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            self.generation += 1;
            self.busy = false;
            self.error = Some(
                if self.undoing {
                    "连接已中断，撤销结果尚未确认。重连后请刷新检查点，勿重复操作。"
                } else if self.indexing {
                    "连接已中断，暂存区更新结果尚未确认。重连后请刷新差异，勿重复操作。"
                } else {
                    "连接已中断，重连后可刷新差异。"
                }
                .into(),
            );
            self.undoing = false;
            self.indexing = false;
            self.index_receipt = None;
            cx.notify();
        }
    }

    fn fetch(&mut self, action: Action, cx: &mut Context<Self>) {
        if self.mutating() {
            return;
        }
        let action = match action {
            Action::List {
                kind: Kind::Unstaged | Kind::Untracked,
                offset,
            } => Action::Status { offset },
            action => action,
        };
        let append =
            matches!(action, Action::List { offset, .. } | Action::Status { offset } if offset > 0);
        self.undoing = matches!(action, Action::Undo { .. });
        self.indexing = matches!(action, Action::Stage { .. } | Action::Unstage { .. });
        let updating_index = self.indexing;
        self.busy = true;
        self.error = None;
        self.generation += 1;
        let generation = self.generation;
        let workspace_owner = self.owner.clone();
        let request = Request::Review {
            session: self.session.clone(),
            expected_workspace: self.workspace.clone(),
            action,
        };
        self.request = cx.spawn(async move |owner, cx| {
            // Workspace may still be borrowed by show_review. Dispatch only
            // after that parent update returns.
            let (result, response) = tokio::sync::oneshot::channel();
            let sent = workspace_owner
                .update(cx, |view, _| {
                    view.backend
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
                    .unwrap_or_else(|_| Err("连接已中断，请重新连接后重试".into()))
            } else {
                Err("工作区未连接，请重新连接后重试".into())
            };
            // Parsing a complete patch can be expensive. Keep it off the UI thread.
            let parsed = if let Ok(Response::Review(Snapshot::Diff { patch, .. })) = &result {
                let patch = patch.clone();
                Some(
                        cx.background_executor()
                            .spawn(async move {
                                DiffFile::parse(&patch).map_err(|error| error.to_string())
                            })
                            .await,
                    )
            } else {
                None
            };
            let _ = owner.update_in(cx, |view, window, cx| {
                if view.generation != generation {
                    return;
                }
                view.busy = false;
                view.undoing = false;
                view.indexing = false;
                match result {
                    Ok(Response::Review(Snapshot::Status {
                        entries,
                        next,
                        branch,
                    })) => {
                        let entries = entries
                            .into_iter()
                            .filter(|entry| matches!(entry.kind, Kind::Unstaged | Kind::Untracked))
                            .collect::<Vec<_>>();
                        if append {
                            view.entries.extend(entries);
                        } else {
                            view.entries = entries;
                        }
                        view.next = next;
                        view.branch = branch;
                        view.loaded = true;
                        view.rebuild_tree(cx);
                        if !append
                            && let Some(path) = view
                                .entries
                                .first()
                                .map(|entry| entry.path.clone())
                        {
                            view.select(path, cx);
                        } else if view.entries.is_empty() {
                            view.selected = None;
                            view.raw = None;
                            view.diff
                                .update(cx, |diff, cx| diff.set_files([], cx));
                        }
                    }
                    Ok(Response::Review(Snapshot::List {
                        entries,
                        next,
                        branch,
                    })) => {
                        if append {
                            view.entries.extend(entries);
                        } else {
                            view.entries = entries;
                        }
                        view.next = next;
                        view.branch = branch;
                        view.loaded = true;
                        view.rebuild_tree(cx);
                        if !append
                            && let Some(path) = view.selected.clone().or_else(|| {
                                view.entries
                                    .first()
                                    .map(|entry| entry.path.clone())
                            })
                        {
                            view.select(path, cx);
                        } else if !append && view.entries.is_empty() {
                            view.selected = None;
                            view.raw = None;
                            view.checkpoint = None;
                            view.index_receipt = None;
                            view.diff
                                .update(cx, |diff, cx| diff.set_files([], cx));
                        }
                    }
                    Ok(Response::Review(Snapshot::Diff {
                        path,
                        note,
                        checkpoint,
                        index_receipt,
                        patch,
                    })) => {
                        view.selected = Some(path);
                        view.note = note.into();
                        view.checkpoint = checkpoint;
                        match parsed.unwrap_or_else(|| Ok(Vec::new())) {
                            Ok(files) => {
                                view.raw = None;
                                view.index_receipt = index_receipt;
                                view.diff
                                    .update(cx, |diff, cx| diff.set_files(files, cx));
                            }
                            Err(error) => {
                                view.index_receipt = None;
                                view.raw = Some(cx.new(|cx| {
                                    EditorState::new(window, cx)
                                        .language("diff")
                                        .line_number(true)
                                        .soft_wrap(false)
                                        .scroll_beyond_last_line(Some(0))
                                        .default_value(patch)
                                }));
                                view.note = format!("显示原始差异：{error}").into();
                            }
                        }
                    }
                    Ok(Response::Review(Snapshot::Undone { detail, .. })) => {
                        view.checkpoint = None;
                        view.index_receipt = None;
                        view.selected = None;
                        view.raw = None;
                        view.note = detail.into();
                        view.diff
                            .update(cx, |diff, cx| diff.set_files([], cx));
                        view.fetch(
                            Action::List {
                                kind: view.kind,
                                offset: 0,
                            },
                            cx,
                        );
                    }
                    Ok(Response::Review(Snapshot::IndexUpdated { staged, .. })) => {
                        view.index_receipt = None;
                        view.selected = None;
                        view.raw = None;
                        view.note = if staged {
                            "文件已暂存，工作区内容已保留"
                        } else {
                            "已取消暂存，工作区内容已保留"
                        }
                        .into();
                        view.diff
                            .update(cx, |diff, cx| diff.set_files([], cx));
                        view.fetch(
                            Action::List {
                                kind: view.kind,
                                offset: 0,
                            },
                            cx,
                        );
                    }
                    Ok(_) => view.error = Some("工作区服务没有返回差异数据，请更新服务".into()),
                    Err(error) => {
                        view.index_receipt = None;
                        view.error = Some(
                            if updating_index {
                                format!("暂存区更新结果未确认：{error}。请刷新差异后再操作。")
                            } else {
                                error
                            }
                            .into(),
                        );
                    }
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    fn confirm_undo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(checkpoint) = self
            .checkpoint
            .clone()
            .filter(|value| !value.undone)
        else {
            return;
        };
        let Some(path) = self.selected.clone() else {
            return;
        };
        let owner = cx.weak_entity();
        crate::dialog::open(window, cx, move |dialog, _, _| {
            let owner = owner.clone();
            let path = path.clone();
            let checkpoint = checkpoint.clone();
            dialog
                .title("撤销这次 Orbit 写入？")
                .child(div().text_sm().child(format!(
                    "将 {path} 恢复为该检查点写入前的内容。若文件已有其他修改，服务会拒绝覆盖。"
                )))
                .footer(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("cancel-review-undo")
                                .label("取消")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("confirm-review-undo")
                                .label("撤销此检查点")
                                .danger()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = owner.update(cx, |view, cx| {
                                        view.fetch(
                                            Action::Undo {
                                                path: path.clone(),
                                                run_id: checkpoint.run_id,
                                                call_id: checkpoint.call_id.clone(),
                                            },
                                            cx,
                                        )
                                    });
                                }),
                        ),
                )
        });
    }

    fn mutating(&self) -> bool {
        self.undoing || self.indexing
    }

    fn update_index(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(path) = self.selected.clone() else {
            return;
        };
        let Some(receipt) = self.index_receipt.clone() else {
            return;
        };
        let action = match self.kind {
            Kind::Staged => Action::Unstage { path, receipt },
            Kind::Unstaged | Kind::Untracked => Action::Stage { path, receipt },
            Kind::Checkpoint => return,
        };
        self.fetch(action, cx);
    }
}

impl Render for Review {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let approvals = self
            .owner
            .upgrade()
            .map(|owner| owner.read(cx).approvals.clone())
            .unwrap_or_default();
        let narrow = self
            .width
            .unwrap_or(window.bounds().size.width)
            < window.rem_size() * 46.;
        let owner = cx.weak_entity();
        let measured = self.width;
        let list = v_flex()
            .min_h_0()
            .min_w_0()
            .border_1()
            .border_color(cx.theme().border)
            .rounded_md()
            .when(narrow, |view| view.h_32())
            .when(!narrow, |view| view.w_56().h_full())
            .capture_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "enter" && !view.mutating() {
                    let path = view
                        .list
                        .read(cx)
                        .selected_item()
                        .map(|item| item.id.to_string());
                    if let Some(path) = path.filter(|path| {
                        view.entries
                            .iter()
                            .any(|entry| entry.path == *path)
                    }) {
                        view.select(path, cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .child(Tree::new(&self.list, {
                let owner = cx.weak_entity();
                let entries = self.entries.clone();
                let busy = self.mutating();
                move |_, item, selected, _, _| {
                    let path = item.item().id.to_string();
                    let entry = entries
                        .iter()
                        .find(|entry| entry.path == path);
                    let file = entry.is_some();
                    let status = entry.map(|entry| {
                        if entry.undone {
                            "已撤销".into()
                        } else {
                            entry.status.clone()
                        }
                    });
                    let owner = owner.clone();
                    ListItem::new(item.item().id.clone())
                        .selected(selected)
                        .disabled(busy)
                        .pl(rems(item.depth() as f32))
                        .child(
                            h_flex()
                                .gap_2()
                                .min_w_0()
                                .child(
                                    Icon::new(if file {
                                        IconName::File
                                    } else if item.is_expanded() {
                                        IconName::FolderOpen
                                    } else {
                                        IconName::Folder
                                    })
                                    .small(),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_ellipsis()
                                        .child(item.item().label.clone()),
                                )
                                .children(status.map(|status| div().text_xs().child(status))),
                        )
                        .on_click(move |_, _, cx| {
                            if file {
                                let _ = owner.update(cx, |view, cx| view.select(path.clone(), cx));
                            }
                        })
                }
            }))
            .when_some(self.next, |view, next| {
                view.child(
                    Button::new("review-more")
                        .ghost()
                        .small()
                        .label("加载更多文件")
                        .disabled(self.busy)
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.fetch(
                                Action::List {
                                    kind: view.kind,
                                    offset: next,
                                },
                                cx,
                            )
                        })),
                )
            })
            .when(self.loaded && self.entries.is_empty(), |view| {
                view.child(
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("此分组暂无文件改动"),
                )
            });
        v_flex()
            .size_full()
            .relative()
            .min_h_0()
            .min_w_0()
            .p_3()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .when_some(self.branch.clone(), |view, branch| {
                        view.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(branch),
                        )
                    })
                    .child(
                        Button::new("review-refresh")
                            .small()
                            .icon(IconName::RefreshCw)
                            .label("刷新")
                            .disabled(self.busy)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.fetch(
                                    Action::List {
                                        kind: view.kind,
                                        offset: 0,
                                    },
                                    cx,
                                )
                            })),
                    ),
            )
            .child(
                TabBar::new("review-kind")
                    .small()
                    .selected_index(
                        KINDS
                            .iter()
                            .position(|(kind, _)| *kind == self.kind)
                            .unwrap_or(0),
                    )
                    .children(
                        KINDS
                            .iter()
                            .map(|(_, label)| Tab::new().label(*label)),
                    )
                    .on_click(cx.listener(|view, index: &usize, _, cx| {
                        if view.mutating() {
                            return;
                        }
                        let Some((kind, _)) = KINDS.get(*index) else {
                            return;
                        };
                        if view.kind == *kind {
                            return;
                        }
                        view.kind = *kind;
                        view.selected = None;
                        view.raw = None;
                        view.checkpoint = None;
                        view.index_receipt = None;
                        view.entries.clear();
                        view.loaded = false;
                        view.diff
                            .update(cx, |diff, cx| diff.set_files([], cx));
                        view.list
                            .update(cx, |_, cx| cx.notify());
                        view.fetch(
                            Action::List {
                                kind: *kind,
                                offset: 0,
                            },
                            cx,
                        );
                    })),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(orbit_client::directories::display_diagnostic(&error).into_owned()),
                )
            })
            .when(self.undoing, |view| {
                view.child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            div()
                                .text_sm()
                                .child(if approvals.is_empty() {
                                    "正在撤销检查点…"
                                } else {
                                    "撤销操作等待审批"
                                }),
                        )
                        .children(approvals.into_iter().map(|approval| {
                            Button::new(("review-approval", approval.id))
                                .small()
                                .label("审查操作")
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    let _ = view.owner.update(cx, |owner, cx| {
                                        owner.approval(&approval, window, cx)
                                    });
                                }))
                        }))
                        .child(
                            Button::new("review-cancel")
                                .small()
                                .ghost()
                                .label("取消撤销")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    let session = view.session.clone();
                                    let _ = view.owner.update(cx, |owner, cx| {
                                        owner.command(
                                            Command::Request(Request::Cancel { session }),
                                            cx,
                                        );
                                    });
                                })),
                        ),
                )
            })
            .child(
                h_flex()
                    .min_h_0()
                    .min_w_0()
                    .flex_1()
                    .gap_3()
                    .items_stretch()
                    .when(narrow, |view| view.flex_col())
                    .child(list)
                    .child(
                        v_flex()
                            .min_h_0()
                            .min_w_0()
                            .flex_1()
                            .gap_2()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .flex_wrap()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .text_sm()
                                            .text_ellipsis()
                                            .child(
                                                self.selected
                                                    .clone()
                                                    .unwrap_or_else(|| "选择文件查看差异".into()),
                                            ),
                                    )
                                    .when(
                                        self.checkpoint
                                            .as_ref()
                                            .is_some_and(|checkpoint| !checkpoint.undone),
                                        |row| {
                                            row.child(
                                                Button::new("review-undo")
                                                    .small()
                                                    .label("撤销此检查点")
                                                    .disabled(self.busy)
                                                    .on_click(cx.listener(
                                                        |view, _, window, cx| {
                                                            view.confirm_undo(window, cx)
                                                        },
                                                    )),
                                            )
                                        },
                                    )
                                    .when(self.kind != Kind::Checkpoint, |row| {
                                        row.child(
                                            Button::new("review-update-index")
                                                .small()
                                                .label(if self.kind == Kind::Staged {
                                                    "取消暂存此文件"
                                                } else {
                                                    "暂存此文件"
                                                })
                                                .disabled(self.busy || self.index_receipt.is_none())
                                                .on_click(cx.listener(|view, _, _, cx| {
                                                    view.update_index(cx)
                                                })),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if self.indexing {
                                        "正在更新暂存区…".into()
                                    } else if self.undoing {
                                        "正在撤销检查点…".into()
                                    } else if self.busy {
                                        "正在读取…".into()
                                    } else {
                                        self.note.clone()
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .min_w_0()
                                    .border_1()
                                    .border_color(cx.theme().border)
                                    .rounded_md()
                                    .overflow_hidden()
                                    .child(if let Some(raw) = &self.raw {
                                        Editor::new(raw)
                                            .readonly(true)
                                            .bordered(false)
                                            .size_full()
                                            .into_any_element()
                                    } else {
                                        Diff::new(&self.diff)
                                            .single_line_number(true)
                                            .into_any_element()
                                    }),
                            ),
                    ),
            )
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let width = bounds.size.width;
                        if width > px(0.) && measured != Some(width) {
                            let owner = owner.clone();
                            cx.defer(move |cx| {
                                let _ = owner.update(cx, |view, cx| {
                                    if view.width != Some(width) {
                                        view.width = Some(width);
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

impl Review {
    fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let mut directories = std::collections::BTreeMap::<String, Vec<String>>::new();
        for entry in &self.entries {
            let mut parent = String::new();
            let parts: Vec<_> = entry.path.split('/').collect();
            for (index, part) in parts.iter().enumerate() {
                let id = if parent.is_empty() {
                    (*part).to_owned()
                } else {
                    format!("{parent}/{part}")
                };
                let children = directories
                    .entry(parent.clone())
                    .or_default();
                if !children.contains(&id) {
                    children.push(id.clone());
                }
                self.tree_items
                    .entry(id.clone())
                    .or_insert_with(|| {
                        TreeItem::new(id.clone(), (*part).to_owned()).expanded(true)
                    });
                if index + 1 < parts.len() {
                    parent = id;
                }
            }
        }
        fn build(
            parent: &str,
            paths: &std::collections::BTreeMap<String, Vec<String>>,
            retained: &mut std::collections::HashMap<String, TreeItem>,
        ) -> Vec<TreeItem> {
            paths
                .get(parent)
                .into_iter()
                .flatten()
                .map(|path| {
                    let children = build(path, paths, retained);
                    let mut item = retained
                        .get(path)
                        .cloned()
                        .expect("tree paths were inserted");
                    item.children = children;
                    retained.insert(path.clone(), item.clone());
                    item
                })
                .collect()
        }
        let items = build("", &directories, &mut self.tree_items);
        self.list
            .update(cx, |list, cx| list.set_items(items, cx));
    }
}
