//! Retained read-only commit browsing; Git access belongs to the session service.
use super::*;
use gpui_kit::component::{
    Selectable,
    diff::{Diff, DiffFile, DiffState},
    input::{Editor, EditorState},
    resizable::ResizableState,
};
use orbit_protocol::review::{Action, Commit, Snapshot};

#[derive(Clone, Copy, PartialEq, Eq)]
enum FileView {
    Content,
    Diff,
}

struct SelectedCommit {
    commit: Commit,
    message: String,
    files: Vec<String>,
}

pub(super) struct History {
    owner: WeakEntity<Workspace>,
    session: String,
    workspace: String,
    revision: String,
    pinned: String,
    commits: Vec<Commit>,
    branches: Vec<String>,
    next: Option<usize>,
    selected: Option<SelectedCommit>,
    path: Option<String>,
    view: FileView,
    editor: Entity<EditorState>,
    diff: Entity<DiffState>,
    raw_diff: Option<Entity<EditorState>>,
    busy: bool,
    panes: Entity<ResizableState>,
    error: Option<String>,
    generation: u64,
    task: Task<()>,
    #[cfg(feature = "test-support")]
    probe_bounds: std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    #[cfg(feature = "test-support")]
    probe_width: Option<Pixels>,
}

impl History {
    pub(super) fn new(
        owner: WeakEntity<Workspace>,
        session: String,
        workspace: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            owner,
            session,
            workspace,
            revision: "HEAD".into(),
            pinned: "HEAD".into(),
            commits: vec![],
            branches: vec![],
            next: None,
            selected: None,
            path: None,
            view: FileView::Content,
            editor: cx.new(|cx| {
                EditorState::new(window, cx)
                    .line_number(true)
                    .soft_wrap(true)
                    .scroll_beyond_last_line(Some(0))
            }),
            diff: cx.new(|cx| DiffState::new([], cx)),
            raw_diff: None,
            busy: false,
            panes: cx.new(|_| ResizableState::default()),
            error: None,
            generation: 0,
            task: Task::ready(()),
            #[cfg(feature = "test-support")]
            probe_bounds: Default::default(),
            #[cfg(feature = "test-support")]
            probe_width: None,
        }
    }

    pub(super) fn belongs_to(&self, session: &str, workspace: &str) -> bool {
        self.session == session && self.workspace == workspace
    }

    pub(super) fn activate(&mut self, cx: &mut Context<Self>) {
        if self.commits.is_empty() && !self.busy {
            self.refresh(cx);
        }
    }

    #[cfg(feature = "test-support")]
    pub(super) fn check_project(
        &mut self,
        step: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(!self.busy, "Git history request did not complete");
        anyhow::ensure!(self.error.is_none(), "Git history failed: {:?}", self.error);
        let selected = self
            .selected
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing automatically selected commit"))?;
        anyhow::ensure!(
            selected.commit.publication == orbit_protocol::review::Publication::Unpushed,
            "local commit must be marked unpushed"
        );
        anyhow::ensure!(
            self.path.as_deref() == Some("src/main.rs"),
            "first historical file must open automatically"
        );
        anyhow::ensure!(
            self.editor
                .read(cx)
                .value()
                .contains("Generated project"),
            "history must show the committed blob, not the worktree"
        );
        if step == 4 {
            anyhow::ensure!(
                self.panes.read(cx).sizes()[0] > self.probe_width.unwrap() + px(20.),
                "dragging the history divider did not resize the list"
            );
            self.view = FileView::Diff;
            cx.notify();
            return Ok(());
        }
        let bounds = self
            .probe_bounds
            .get()
            .ok_or_else(|| anyhow::anyhow!("missing history pane bounds"))?;
        let width = *self
            .probe_width
            .get_or_insert(self.panes.read(cx).sizes()[0]);
        let position = point(
            bounds.left()
                + width
                + match step {
                    0 => px(0.),
                    1 => px(10.),
                    _ => px(40.),
                },
            bounds.center().y,
        );
        let event = match step {
            0 => PlatformInput::MouseDown(MouseDownEvent {
                position,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }),
            1 | 2 => PlatformInput::MouseMove(MouseMoveEvent {
                position,
                pressed_button: Some(MouseButton::Left),
                ..Default::default()
            }),
            _ => PlatformInput::MouseUp(MouseUpEvent {
                position,
                button: MouseButton::Left,
                click_count: 1,
                ..Default::default()
            }),
        };
        let handle = window.window_handle();
        cx.defer(move |cx| {
            let _ = handle.update(cx, |_, window, cx| {
                window.dispatch_event(event, cx);
            });
        });
        Ok(())
    }

    fn commit_row(&self, commit: &Commit, cx: &mut Context<Self>) -> Button {
        let id = commit.id.clone();
        Button::new(SharedString::from(format!("commit-{id}")))
            .ghost()
            .w_full()
            .h_auto()
            .py_2()
            .justify_start()
            .selected(
                self.selected
                    .as_ref()
                    .is_some_and(|selected| selected.commit.id == id),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .w_full()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_ellipsis()
                            .child(commit.subject.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .text_ellipsis()
                            .child(format!(
                                "{} · {} · {}",
                                commit.short_id, commit.author, commit.authored_at
                            )),
                    ),
            )
            .when(
                commit.publication == orbit_protocol::review::Publication::Unpushed,
                |button| {
                    button.child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("未推送"),
                    )
                },
            )
            .disabled(self.busy)
            .on_click(cx.listener(move |view, _, _, cx| {
                view.fetch(
                    Action::Commit {
                        revision: id.clone(),
                    },
                    cx,
                )
            }))
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.path = None;
        self.fetch(
            Action::History {
                revision: self.revision.clone(),
                offset: 0,
            },
            cx,
        );
    }

    fn fetch(&mut self, action: Action, cx: &mut Context<Self>) {
        let append = matches!(action, Action::History { offset, .. } if offset > 0);
        self.generation += 1;
        let generation = self.generation;
        self.busy = true;
        self.error = None;
        let owner = self.owner.clone();
        let session = self.session.clone();
        let workspace = self.workspace.clone();
        self.task = cx.spawn(async move |entity, cx| {
            let (result, response) = tokio::sync::oneshot::channel();
            let request = Request::Review {
                session: session.clone(),
                expected_workspace: workspace.clone(),
                action,
            };
            let sent = owner
                .update(cx, |view, _| {
                    view.info
                        .as_ref()
                        .is_some_and(|info| info.id == session && info.workspace == workspace)
                        && view
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
                    .unwrap_or_else(|_| Err("连接已断开".into()))
            } else {
                Err("会话或项目已切换，请重新打开历史".into())
            };
            let parsed = if let Ok(Response::Review(Snapshot::CommitFile { patch, .. })) = &result {
                let patch = patch.clone();
                Some(
                    cx.background_spawn(async move {
                        DiffFile::parse(&patch).map_err(|error| error.to_string())
                    })
                    .await,
                )
            } else {
                None
            };
            let _ = entity.update_in(cx, |view, window, cx| {
                if generation != view.generation {
                    return;
                }
                view.busy = false;
                match result {
                    Ok(Response::Review(Snapshot::History {
                        revision,
                        commits,
                        branches,
                        next,
                    })) => {
                        view.pinned = revision;
                        if append {
                            view.commits.extend(commits);
                        } else {
                            view.commits = commits;
                        }
                        view.branches = branches;
                        view.next = next;
                        if !append && let Some(commit) = view.commits.first() {
                            let revision = commit.id.clone();
                            let entity = cx.weak_entity();
                            cx.defer(move |cx| {
                                let _ = entity.update(cx, |view, cx| {
                                    view.fetch(Action::Commit { revision }, cx)
                                });
                            });
                        }
                    }
                    Ok(Response::Review(Snapshot::Commit {
                        commit,
                        message,
                        files,
                    })) => {
                        view.path = None;
                        if let Some(path) = files.first() {
                            let path = path.clone();
                            let revision = commit.id.clone();
                            let entity = cx.weak_entity();
                            cx.defer(move |cx| {
                                let _ = entity.update(cx, |view, cx| {
                                    view.fetch(Action::CommitFile { revision, path }, cx)
                                });
                            });
                        }
                        view.selected = Some(SelectedCommit {
                            commit,
                            message,
                            files,
                        });
                    }
                    Ok(Response::Review(Snapshot::CommitFile {
                        path,
                        content,
                        patch,
                    })) => {
                        view.editor = cx.new(|cx| {
                            EditorState::new(window, cx)
                                .language(orbit_syntax::label_for_path(&path))
                                .line_number(true)
                                .soft_wrap(true)
                                .scroll_beyond_last_line(Some(0))
                                .default_value(content.unwrap_or_else(|| {
                                    "此提交中不存在该文件，或文件不是文本".into()
                                }))
                        });
                        view.path = Some(path);
                        view.view = FileView::Content;
                        view.raw_diff = None;
                        match parsed.unwrap_or_else(|| Ok(vec![])) {
                            Ok(files) => view
                                .diff
                                .update(cx, |diff, cx| diff.set_files(files, cx)),
                            Err(_) => {
                                view.raw_diff = Some(cx.new(|cx| {
                                    EditorState::new(window, cx)
                                        .language("diff")
                                        .line_number(true)
                                        .soft_wrap(true)
                                        .scroll_beyond_last_line(Some(0))
                                        .default_value(patch)
                                }))
                            }
                        }
                    }
                    Ok(Response::Error(error)) | Err(error) => view.error = Some(error),
                    _ => view.error = Some("服务未返回提交历史，请更新服务后重试".into()),
                }
                cx.notify();
            });
        });
        cx.notify();
    }
}

impl Render for History {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let branch_owner = cx.weak_entity();
        let branches = self.branches.clone();
        let commits = v_flex()
            .id("git-history-commits")
            .w_full()
            .min_w_0()
            .h_full()
            .px_2()
            .py_2()
            .child(
                uniform_list(
                    "git-commit-rows",
                    self.commits.len(),
                    cx.processor(|view, range: std::ops::Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                view.commit_row(&view.commits[index], cx)
                                    .into_any_element()
                            })
                            .collect()
                    }),
                )
                .flex_1()
                .min_h_0(),
            )
            .when_some(self.next, |list, next| {
                list.child(
                    Button::new("history-more")
                        .ghost()
                        .label("加载更多")
                        .disabled(self.busy)
                        .on_click(cx.listener(move |view, _, _, cx| {
                            view.fetch(
                                Action::History {
                                    revision: view.pinned.clone(),
                                    offset: next,
                                },
                                cx,
                            )
                        })),
                )
            });
        let detail = v_flex()
            .h_full()
            .justify_start()
            .items_stretch()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .when_some(self.selected.as_ref(), |detail, selected| {
                let id = selected.commit.id.clone();
                detail
                    .child(
                        h_flex()
                            .px_3()
                            .pt_3()
                            .gap_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} · {}",
                                selected.commit.short_id, selected.commit.author
                            ))
                            .child(match selected.commit.publication {
                                orbit_protocol::review::Publication::Unpushed => "未推送",
                                orbit_protocol::review::Publication::Published => "远端已有",
                                orbit_protocol::review::Publication::Unknown => "远端状态未知",
                            }),
                    )
                    .child(
                        div()
                            .id("commit-message")
                            .h_auto()
                            .w_full()
                            .flex_shrink_0()
                            .max_h(rems(8.))
                            .overflow_y_scroll()
                            .p_3()
                            .text_sm()
                            .whitespace_normal()
                            .child(selected.message.clone()),
                    )
                    .child(
                        div()
                            .id("commit-files")
                            .h_auto()
                            .w_full()
                            .flex_shrink_0()
                            .max_h(rems(12.))
                            .overflow_y_scroll()
                            .px_2()
                            .children(selected.files.iter().map(|path| {
                                let path = path.clone();
                                let id = id.clone();
                                Button::new(SharedString::from(format!("commit-file-{path}")))
                                    .ghost()
                                    .small()
                                    .w_full()
                                    .justify_start()
                                    .selected(self.path.as_ref() == Some(&path))
                                    .child(
                                        div()
                                            .w_full()
                                            .min_w_0()
                                            .text_ellipsis()
                                            .child(path.clone()),
                                    )
                                    .disabled(self.busy)
                                    .on_click(cx.listener(move |view, _, _, cx| {
                                        view.fetch(
                                            Action::CommitFile {
                                                revision: id.clone(),
                                                path: path.clone(),
                                            },
                                            cx,
                                        )
                                    }))
                            })),
                    )
            })
            .when(self.selected.is_none(), |detail| {
                detail.child(
                    div()
                        .p_4()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("选择提交查看说明与文件"),
                )
            })
            .when(self.path.is_some(), |detail| {
                detail
                    .child(
                        h_flex()
                            .px_3()
                            .py_2()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .text_ellipsis()
                                    .child(self.path.clone().unwrap_or_default()),
                            )
                            .children(
                                [(FileView::Content, "完整文件"), (FileView::Diff, "差异")]
                                    .into_iter()
                                    .map(|(mode, label)| {
                                        Button::new(label)
                                            .ghost()
                                            .small()
                                            .label(label)
                                            .selected(self.view == mode)
                                            .on_click(cx.listener(move |view, _, _, cx| {
                                                view.view = mode;
                                                cx.notify();
                                            }))
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .min_h_0()
                            .child(match self.view {
                                FileView::Content => Editor::new(&self.editor)
                                    .readonly(true)
                                    .bordered(false)
                                    .size_full()
                                    .into_any_element(),
                                FileView::Diff => {
                                    if let Some(raw) = &self.raw_diff {
                                        Editor::new(raw)
                                            .readonly(true)
                                            .bordered(false)
                                            .size_full()
                                            .into_any_element()
                                    } else {
                                        Diff::new(&self.diff)
                                            .single_line_number(true)
                                            .size_full()
                                            .into_any_element()
                                    }
                                }
                            }),
                    )
            });
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .child(
                        Button::new("history-branches")
                            .ghost()
                            .small()
                            .label(self.revision.clone())
                            .dropdown_caret(true)
                            .disabled(self.busy)
                            .dropdown_menu(move |mut menu, _, _| {
                                for branch in &branches {
                                    let owner = branch_owner.clone();
                                    let branch = branch.clone();
                                    menu = menu.item(PopupMenuItem::new(branch.clone()).on_click(
                                        move |_, _, cx| {
                                            let _ = owner.update(cx, |view, cx| {
                                                view.revision = branch.clone();
                                                view.refresh(cx);
                                            });
                                        },
                                    ));
                                }
                                menu
                            }),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("history-refresh")
                            .ghost()
                            .small()
                            .icon(IconName::RefreshCw)
                            .accessibility_label("刷新提交历史")
                            .disabled(self.busy)
                            .on_click(cx.listener(|view, _, _, cx| view.refresh(cx))),
                    ),
            )
            .when(self.busy, |view| {
                view.child(
                    div()
                        .px_3()
                        .text_xs()
                        .child("正在读取提交…"),
                )
            })
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
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .when(cfg!(feature = "test-support"), |view| {
                        #[cfg(feature = "test-support")]
                        {
                            let bounds = self.probe_bounds.clone();
                            view.child(
                                canvas(move |value, _, _| bounds.set(Some(value)), |_, _, _, _| {})
                                    .absolute()
                                    .size_full(),
                            )
                        }
                        #[cfg(not(feature = "test-support"))]
                        view
                    })
                    .child(
                        h_resizable("history-panes")
                            .with_state(&self.panes)
                            .child(
                                resizable_panel()
                                    .size(window.rem_size() * 17.)
                                    .size_range(window.rem_size() * 12. ..window.rem_size() * 32.)
                                    .child(commits),
                            )
                            .child(resizable_panel().child(detail)),
                    ),
            )
    }
}
