//! Retained read-only commit browsing; Git access belongs to the session service.
use super::*;
use gpui_kit::component::{
    Selectable,
    diff::{Diff, DiffFile, DiffState},
    input::{Editor, EditorState, Input, InputState},
    scroll::ScrollableElement,
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
    revision: Entity<InputState>,
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
    show_commits: bool,
    error: Option<String>,
    generation: u64,
    task: Task<()>,
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
            revision: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value("HEAD")
                    .placeholder("分支或提交")
            }),
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
                    .soft_wrap(false)
                    .scroll_beyond_last_line(Some(0))
            }),
            diff: cx.new(|cx| DiffState::new([], cx)),
            raw_diff: None,
            busy: false,
            show_commits: true,
            error: None,
            generation: 0,
            task: Task::ready(()),
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
    pub(super) fn check_project(&mut self, step: u8, cx: &mut Context<Self>) -> anyhow::Result<()> {
        anyhow::ensure!(!self.busy, "Git history request did not complete");
        anyhow::ensure!(self.error.is_none(), "Git history failed: {:?}", self.error);
        match step {
            0 => {
                let commit = self
                    .commits
                    .first()
                    .ok_or_else(|| anyhow::anyhow!("missing generated commit"))?;
                self.fetch(
                    Action::Commit {
                        revision: commit.id.clone(),
                    },
                    cx,
                );
            }
            1 => {
                let selected = self
                    .selected
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("missing commit detail"))?;
                anyhow::ensure!(
                    selected
                        .files
                        .contains(&"src/main.rs".into()),
                    "missing committed path"
                );
                self.fetch(
                    Action::CommitFile {
                        revision: selected.commit.id.clone(),
                        path: "src/main.rs".into(),
                    },
                    cx,
                );
            }
            _ => {
                anyhow::ensure!(
                    self.path.as_deref() == Some("src/main.rs"),
                    "missing historical file"
                );
                anyhow::ensure!(
                    self.editor
                        .read(cx)
                        .value()
                        .contains("Generated project"),
                    "history must show the committed blob, not the worktree"
                );
                self.view = FileView::Diff;
                self.show_commits = false;
                cx.notify();
            }
        }
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
                revision: self
                    .revision
                    .read(cx)
                    .value()
                    .to_string(),
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
                    }
                    Ok(Response::Review(Snapshot::Commit {
                        commit,
                        message,
                        files,
                    })) => {
                        view.path = None;
                        if window.viewport_size().width < window.rem_size() * 80. {
                            view.show_commits = false;
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
                                .soft_wrap(false)
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
                                        .soft_wrap(false)
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let branch_owner = cx.weak_entity();
        let branches = self.branches.clone();
        let commits = v_flex()
            .id("git-history-commits")
            .w_56()
            .min_w_0()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
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
                        div()
                            .id("commit-message")
                            .h_auto()
                            .w_full()
                            .flex_shrink_0()
                            .max_h(rems(8.))
                            .overflow_y_scrollbar()
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
                            .overflow_y_scrollbar()
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
                        Button::new("history-list")
                            .ghost()
                            .small()
                            .label("提交列表")
                            .selected(self.show_commits)
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.show_commits = !view.show_commits;
                                cx.notify();
                            })),
                    )
                    .child(
                        Input::new(&self.revision)
                            .small()
                            .flex_1()
                            .min_w_0()
                            .aria_label("Git 分支或提交"),
                    )
                    .child(
                        Button::new("history-load")
                            .small()
                            .label("查看")
                            .disabled(self.busy)
                            .on_click(cx.listener(|view, _, _, cx| view.refresh(cx))),
                    ),
            )
            .when(!self.branches.is_empty(), |view| {
                view.child(
                    div().px_3().child(
                        Button::new("history-branches")
                            .ghost()
                            .small()
                            .label("选择分支")
                            .disabled(self.busy)
                            .dropdown_caret(true)
                            .dropdown_menu(move |mut menu, _, _| {
                                for branch in &branches {
                                    let owner = branch_owner.clone();
                                    let branch = branch.clone();
                                    menu = menu.item(PopupMenuItem::new(branch.clone()).on_click(
                                        move |_, window, cx| {
                                            let _ = owner.update(cx, |view, cx| {
                                                view.revision.update(cx, |input, cx| {
                                                    input.set_value(branch.clone(), window, cx)
                                                });
                                                view.refresh(cx);
                                            });
                                        },
                                    ));
                                }
                                menu
                            }),
                    ),
                )
            })
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
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .items_stretch()
                    .when(self.show_commits, |view| view.child(commits))
                    .child(detail),
            )
    }
}
