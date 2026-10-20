//! Retained read-only commit browsing; Git access belongs to the session service.
use super::*;
use gpui_kit::component::{
    Selectable,
    diff::{Diff, DiffFile, DiffHunkSeparator, DiffState},
    input::{Editor, EditorState},
    resizable::ResizableState,
};
use orbit_protocol::review::{Action, Commit, Snapshot};

struct SelectedCommit {
    commit: Commit,
    message: String,
}

pub(super) struct History {
    owner: WeakEntity<Workspace>,
    session: String,
    workspace: String,
    pinned: String,
    commits: Vec<Commit>,
    next: Option<usize>,
    selected: Option<SelectedCommit>,
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
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            owner,
            session,
            workspace,
            pinned: "HEAD".into(),
            commits: vec![],
            next: None,
            selected: None,
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
        if !self.busy {
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
            selected.commit.subject == "Generated local commit",
            "missing selected commit"
        );
        anyhow::ensure!(
            self.diff.read(cx).files().len() == 2,
            "all committed file differences must open together"
        );
        if step == 4 {
            anyhow::ensure!(
                self.panes.read(cx).sizes()[0] > self.probe_width.unwrap() + px(20.),
                "dragging the history divider did not resize the list"
            );
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
                                commit.short_id,
                                commit.author,
                                short_date(&commit.authored_at)
                            )),
                    ),
            )
            .disabled(self.busy)
            .on_click(cx.listener(move |view, _, _, cx| {
                view.fetch(
                    Action::Commit {
                        revision: id.clone(),
                        include_patch: true,
                    },
                    cx,
                )
            }))
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.selected = None;
        self.fetch(
            Action::History {
                revision: "HEAD".into(),
                offset: 0,
            },
            cx,
        );
    }

    fn fetch(&mut self, action: Action, cx: &mut Context<Self>) {
        let append = matches!(action, Action::History { offset, .. } if offset > 0);
        if matches!(action, Action::Commit { .. }) {
            self.selected = None;
            self.raw_diff = None;
            self.diff
                .update(cx, |diff, cx| diff.set_files([], cx));
        }
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
            let parsed = if let Ok(Response::Review(Snapshot::Commit {
                patch: Some(patch), ..
            })) = &result
            {
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
                        next,
                        ..
                    })) => {
                        view.pinned = revision;
                        if append {
                            view.commits.extend(commits);
                        } else {
                            view.commits = commits;
                        }
                        view.next = next;
                        if !append && let Some(commit) = view.commits.first() {
                            let revision = commit.id.clone();
                            let entity = cx.weak_entity();
                            cx.defer(move |cx| {
                                let _ = entity.update(cx, |view, cx| {
                                    view.fetch(
                                        Action::Commit {
                                            revision,
                                            include_patch: true,
                                        },
                                        cx,
                                    )
                                });
                            });
                        }
                    }
                    Ok(Response::Review(Snapshot::Commit {
                        commit,
                        message,
                        patch,
                        ..
                    })) => {
                        view.selected = Some(SelectedCommit { commit, message });
                        view.raw_diff = None;
                        match (patch, parsed) {
                            (Some(patch), Some(parsed)) => match parsed {
                                Ok(files) => {
                                    view.diff = cx.new(|cx| DiffState::new(files, cx));
                                    if patch.is_empty() {
                                        view.error = Some("此提交没有文件差异".into());
                                    }
                                }
                                Err(_) => {
                                    view.raw_diff = Some(cx.new(|cx| {
                                        EditorState::new(window, cx)
                                            .language("diff")
                                            .line_number(true)
                                            .soft_wrap(true)
                                            .scroll_beyond_last_line(Some(0))
                                            .default_value(patch)
                                    }));
                                }
                            },
                            _ => view.error = Some("服务不支持完整提交差异，请更新服务".into()),
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
        let commits = v_flex()
            .size_full()
            .min_w_0()
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
                            );
                        })),
                )
            });
        let detail = v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .when(self.busy, |view| {
                view.child(
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("正在读取提交…"),
                )
            })
            .when(
                !self.busy && self.commits.is_empty() && self.error.is_none(),
                |view| {
                    view.child(
                        div()
                            .p_3()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("暂无提交"),
                    )
                },
            )
            .when_some(self.selected.as_ref(), |detail, selected| {
                detail.child(
                    v_flex()
                        .p_3()
                        .gap_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "{} · {} · {}",
                                    selected.commit.author,
                                    short_date(&selected.commit.authored_at),
                                    selected.commit.short_id
                                )),
                        )
                        .child(
                            div()
                                .id("commit-message")
                                .max_h(rems(8.))
                                .overflow_y_scroll()
                                .text_sm()
                                .whitespace_normal()
                                .child(selected.message.trim().to_owned()),
                        ),
                )
            })
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .p_3()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(orbit_client::directories::display_diagnostic(&error).into_owned()),
                )
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(if let Some(raw) = &self.raw_diff {
                        Editor::new(raw)
                            .readonly(true)
                            .bordered(false)
                            .size_full()
                            .into_any_element()
                    } else {
                        Diff::new(&self.diff)
                            .soft_wrap(true)
                            .hunk_separator(DiffHunkSeparator::Simple)
                            .size_full()
                            .into_any_element()
                    }),
            );
        div()
            .size_full()
            .min_w_0()
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
            )
    }
}

fn short_date(value: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|date| date.format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|_| "时间未知".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn commit_dates_are_short_and_do_not_expose_raw_invalid_values() {
        assert_eq!(
            super::short_date("2024-05-06T07:08:09+08:00"),
            "05-06 07:08"
        );
        assert_eq!(super::short_date("invalid"), "时间未知");
    }
}
