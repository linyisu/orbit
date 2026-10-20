//! Retained review surface. Git index transactions and Orbit write receipts
//! have separate sources and exact reviewed-state guards.
use super::*;
use gpui_kit::component::{
    IndexPath,
    diff::{Diff, DiffFile, DiffMode, DiffState},
    list::{List, ListDelegate, ListItem, ListState},
    tab::{Tab, TabBar},
};
use orbit_protocol::review::{Action, Checkpoint, Entry, Kind, Snapshot};

const KINDS: [(Kind, &str); 4] = [
    (Kind::Checkpoint, "Orbit 检查点"),
    (Kind::Unstaged, "未暂存"),
    (Kind::Staged, "已暂存"),
    (Kind::Untracked, "未跟踪"),
];

pub(super) struct Review {
    owner: WeakEntity<Workspace>,
    session: String,
    workspace: String,
    kind: Kind,
    entries: Vec<Entry>,
    selected: Option<String>,
    checkpoint: Option<Checkpoint>,
    index_receipt: Option<String>,
    list: Entity<ListState<ReviewList>>,
    diff: Entity<DiffState>,
    next: Option<usize>,
    branch: Option<String>,
    note: SharedString,
    error: Option<SharedString>,
    busy: bool,
    undoing: bool,
    indexing: bool,
    loaded: bool,
    generation: usize,
    split: bool,
    width: Option<Pixels>,
    request: Task<()>,
}

impl Review {
    pub(super) fn new(
        owner: WeakEntity<Workspace>,
        session: String,
        workspace: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let list_owner = cx.weak_entity();
        Self {
            owner,
            session,
            workspace,
            kind: Kind::Checkpoint,
            entries: Vec::new(),
            selected: None,
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
            split: false,
            width: None,
            request: Task::ready(()),
            diff: cx.new(|cx| DiffState::new([], cx)),
            list: cx.new(|cx| {
                ListState::new(
                    ReviewList {
                        owner: list_owner,
                        selected: None,
                    },
                    window,
                    cx,
                )
            }),
        }
    }

    pub(super) fn belongs_to(&self, session: &str, workspace: &str) -> bool {
        self.session == session && self.workspace == workspace
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &mut App) {
        let list = self.list.focus_handle(cx);
        if !list.contains_focused(window, cx)
            && !self
                .diff
                .focus_handle(cx)
                .contains_focused(window, cx)
        {
            list.focus(window, cx);
        }
    }

    pub(super) fn activate(&mut self, path: Option<String>, cx: &mut Context<Self>) {
        if self.mutating() {
            return;
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
        self.selected = None;
        self.checkpoint = None;
        self.index_receipt = None;
        self.entries.clear();
        self.loaded = false;
        self.diff
            .update(cx, |diff, cx| diff.set_files([], cx));
        self.list
            .update(cx, |_, cx| cx.notify());
        self.activate(path, cx);
    }

    fn select(&mut self, path: String, cx: &mut Context<Self>) {
        if self.mutating() {
            return;
        }
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
        let append = matches!(action, Action::List { offset, .. } if offset > 0);
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
            let _ = owner.update_in(cx, |view, _, cx| {
                if view.generation != generation {
                    return;
                }
                view.busy = false;
                view.undoing = false;
                view.indexing = false;
                match result {
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
                        view.list
                            .update(cx, |_, cx| cx.notify());
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
                        ..
                    })) => {
                        view.selected = Some(path);
                        view.note = note.into();
                        view.checkpoint = checkpoint;
                        match parsed.unwrap_or_else(|| Ok(Vec::new())) {
                            Ok(files) => {
                                view.index_receipt = index_receipt;
                                view.diff
                                    .update(cx, |diff, cx| diff.set_files(files, cx));
                            }
                            Err(error) => {
                                view.index_receipt = None;
                                view.error = Some(format!("差异无法解析：{error}").into())
                            }
                        }
                    }
                    Ok(Response::Review(Snapshot::Undone { detail, .. })) => {
                        view.checkpoint = None;
                        view.index_receipt = None;
                        view.selected = None;
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
            .child(List::new(&self.list).size_full())
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
                                    .child(
                                        Button::new("review-mode")
                                            .ghost()
                                            .small()
                                            .label(if self.split { "并排" } else { "统一" })
                                            .on_click(cx.listener(|view, _, _, cx| {
                                                view.split = !view.split;
                                                let mode = if view.split {
                                                    DiffMode::Split
                                                } else {
                                                    DiffMode::Unified
                                                };
                                                view.diff
                                                    .update(cx, |diff, cx| diff.set_mode(mode, cx));
                                                cx.notify();
                                            })),
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
                                    .child(Diff::new(&self.diff).single_line_number(true)),
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

struct ReviewList {
    owner: WeakEntity<Review>,
    selected: Option<IndexPath>,
}

impl ListDelegate for ReviewList {
    type Item = ListItem;
    fn items_count(&self, _: usize, cx: &App) -> usize {
        self.owner
            .upgrade()
            .map_or(0, |owner| owner.read(cx).entries.len())
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let owner = self.owner.upgrade()?;
        let view = owner.read(cx);
        let entry = view.entries.get(index.row)?;
        Some(
            ListItem::new(SharedString::from(format!("review-file-{}", index.row)))
                .h_12()
                .px_2()
                .disabled(view.mutating())
                .confirmed(view.selected.as_ref() == Some(&entry.path))
                .child(
                    v_flex()
                        .min_w_0()
                        .w_full()
                        .gap_1()
                        .child(div().text_sm().text_ellipsis().child(
                            orbit_client::directories::display_path(&entry.path).into_owned(),
                        ))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(if entry.undone {
                                    "已撤销".into()
                                } else {
                                    entry.status.clone()
                                }),
                        ),
                ),
        )
    }

    fn set_selected_index(
        &mut self,
        selected: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected = selected;
    }

    fn confirm(&mut self, _: bool, _: &mut Window, cx: &mut Context<ListState<Self>>) {
        let Some(index) = self.selected else { return };
        let _ = self.owner.update(cx, |view, cx| {
            if view.mutating() {
                return;
            }
            if let Some(entry) = view.entries.get(index.row) {
                view.select(entry.path.clone(), cx);
            }
        });
    }
}
