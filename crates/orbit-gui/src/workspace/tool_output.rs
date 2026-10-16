//! Native, read-only evidence received from the session service. Explicit image
//! and subtask navigation goes through the owning session; no command runs here.
use super::*;
use crate::execution::{ToolRun, ToolStatus};
use gpui_kit::component::{
    clipboard::Clipboard,
    input::{Editor, EditorState},
    tab::{Tab, TabBar},
};

use orbit_client::execution::details::{
    Request as ToolRequest, Stream, content, default_stream, retained::Retained, status,
};

pub(super) struct ToolOutput {
    owner: WeakEntity<Workspace>,
    options: Options,
    session: String,
    workspace: String,
    tool: ToolRun,
    stream: Stream,
    editor: Entity<EditorState>,
    request_editor: Entity<EditorState>,
    request: ToolRequest,
    pages: &'static [Stream],
    status: String,
    retained: Retained,
    displayed: String,
    paused: bool,
    disconnected: bool,
    closed: bool,
}

impl ToolOutput {
    pub(super) fn new(
        owner: WeakEntity<Workspace>,
        options: Options,
        session: String,
        workspace: String,
        tool: ToolRun,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let stream = default_stream(&tool);
        let displayed = content(&tool, stream);
        let request = ToolRequest::new(&tool, &workspace);
        Self {
            owner,
            options,
            session,
            workspace,
            editor: cx.new(|cx| {
                EditorState::new(window, cx)
                    .language("plaintext")
                    .line_number(true)
                    .folding(false)
                    .soft_wrap(true)
                    .default_value(displayed.clone())
            }),
            request_editor: cx.new(|cx| {
                EditorState::new(window, cx)
                    .language(request.language())
                    .line_number(true)
                    .folding(false)
                    .soft_wrap(true)
                    .default_value(request.content())
            }),
            request,
            pages: orbit_client::execution::details::streams(&tool),
            status: status(&tool),
            retained: Retained::default(),
            tool,
            stream,
            displayed,
            paused: false,
            disconnected: false,
            closed: false,
        }
    }

    pub(super) fn belongs_to(&self, options: &Options, session: &str, workspace: &str) -> bool {
        self.options.same_host(options) && self.session == session && self.workspace == workspace
    }

    pub(super) fn matches(&self, tool: &ToolRun) -> bool {
        self.tool.generation == tool.generation
            && self.tool.id == tool.id
            && self.tool.name == tool.name
            && self.tool.arguments == tool.arguments
    }

    /// Replayed events have new observer generations. A restored call can be opened explicitly, but
    /// cannot take ownership of an older viewer.
    pub(super) fn sync(
        &mut self,
        tool: &ToolRun,
        restoring: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.disconnected || !self.matches(tool) || restoring {
            return;
        }
        self.tool = tool.clone();
        if !self.paused && !self.closed {
            self.refresh(window, cx);
        }
        cx.notify();
    }

    pub(super) fn disconnected(&mut self, cx: &mut Context<Self>) {
        self.disconnected = true;
        self.tool.live = false;
        cx.notify();
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pages = orbit_client::execution::details::streams(&self.tool);
        if !self.pages.contains(&self.stream) {
            self.stream = default_stream(&self.tool);
        }
        self.status = status(&self.tool);
        if !self.disconnected && self.retained.begin(&self.tool) {
            let (result, response) = tokio::sync::oneshot::channel();
            let session = self.session.clone();
            let tool = Box::new(self.tool.clone());
            let output = self
                .tool
                .output
                .clone()
                .unwrap_or_default();
            let owner = self.owner.clone();
            cx.spawn_in(window, async move |view, cx| {
                // Opening and syncing can run inside the workspace's update.
                // Dispatch after that update releases its entity borrow.
                let _ = owner.update(cx, |workspace, cx| {
                    workspace.command(
                        Command::ToolOutput {
                            session,
                            tool,
                            result,
                        },
                        cx,
                    );
                });
                let result = response
                    .await
                    .unwrap_or_else(|_| Err("读取已取消或连接已断开".into()));
                let _ = view.update_in(cx, |view, window, cx| {
                    if view.retained.receive(&output, result) && !view.closed {
                        view.refresh(window, cx);
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        let next = self
            .retained
            .content(&self.tool, self.stream);
        if next != self.displayed {
            self.editor.update(cx, |editor, cx| {
                editor.set_value(next.clone(), window, cx);
            });
            self.displayed = next;
        }
    }

    fn select(&mut self, stream: Stream, window: &mut Window, cx: &mut Context<Self>) {
        self.stream = stream;
        self.refresh(window, cx);
        self.editor
            .update(cx, |editor, cx| editor.focus(window, cx));
        cx.notify();
    }

    fn inspect_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use orbit_client::execution::details::{Target, target};
        if self.disconnected {
            return;
        }
        let Some(target) = target(&self.tool) else {
            return;
        };
        let session = self.session.clone();
        let current = self
            .owner
            .upgrade()
            .is_some_and(|owner| {
                let workspace = owner.read(cx);
                workspace
                    .options
                    .same_host(&self.options)
                    && workspace
                        .info
                        .as_ref()
                        .is_some_and(|info| info.id == session)
            });
        if !current {
            return;
        }
        match target {
            Target::Agent(agent) => {
                let _ = self.owner.update(cx, |workspace, cx| {
                    workspace.command(
                        Command::Request(Request::ReadSubagent {
                            session,
                            agent,
                            after: 0,
                        }),
                        cx,
                    );
                });
                window.close_dialog(cx);
            }
            Target::Image(path) => {
                let (result, response) = tokio::sync::oneshot::channel();
                let _ = self.owner.update(cx, |workspace, cx| {
                    workspace.command(
                        Command::Settings {
                            request: Request::ReadFile {
                                session,
                                path,
                                allow_missing: false,
                            },
                            result,
                        },
                        cx,
                    );
                });
                cx.spawn_in(window, async move |panel, cx| {
                    let result = response
                        .await
                        .map_err(|_| "图片读取已取消".to_owned())
                        .and_then(|result| result);
                    let image = cx
                        .background_spawn(async move {
                            let Response::File(encoded) = result? else {
                                return Err("图片响应格式无效".to_owned());
                            };
                            let bytes = orbit_client::protocol::file_bytes(encoded)
                                .map_err(|error| error.to_string())?;
                            crate::image_preview::decode_image(bytes)
                                .map_err(|error| error.to_string())
                        })
                        .await;
                    let _ = panel.update_in(cx, |panel, window, cx| {
                        if panel.closed {
                            return;
                        }
                        match image {
                            Ok((image, size)) => {
                                crate::image_preview::open(Ok(Some(image)), Some(size), window, cx)
                            }
                            Err(error) => {
                                crate::image_preview::open(Err(error.into()), None, window, cx)
                            }
                        }
                    });
                })
                .detach();
            }
            Target::Question => self.select(Stream::Result, window, cx),
        }
    }
}

pub(super) fn present(panel: Entity<ToolOutput>, window: &mut Window, cx: &mut App) {
    panel.update(cx, |panel, cx| {
        panel.closed = false;
        panel.refresh(window, cx);
    });
    crate::dialog::open(window, cx, move |dialog, window, cx| {
        let close = panel.downgrade();
        let height = (window.rem_size() * 46.).min(window.viewport_size().height * 0.8);
        let content = panel.clone();
        dialog
            .title(panel.read(cx).tool.name.clone())
            .h(height)
            // The editor owns scrolling; Dialog's ordinary child slot adds
            // a second scroll viewport around the entire inspector.
            .content(move |body, _, _| {
                body.size_full()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .child(content.clone())
            })
            .on_ok(|_, _, _| false)
            .on_close(move |_, _, cx| {
                let _ = close.update(cx, |panel, cx| {
                    panel.closed = true;
                    cx.notify();
                });
            })
    });
}

impl Render for ToolOutput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_owner = self.owner.upgrade().is_some();
        let status = if self.disconnected || !has_owner {
            "连接已断开，保留已接收输出".to_owned()
        } else if self.paused {
            "已暂停更新".to_owned()
        } else {
            self.status.clone()
        };
        let streams = self.pages;
        let target = orbit_client::execution::details::target(&self.tool)
            .filter(|_| !self.disconnected && has_owner)
            .filter(|target| !matches!(target, orbit_client::execution::details::Target::Question));
        let selected = streams
            .iter()
            .position(|stream| *stream == self.stream)
            .unwrap_or(0);
        let request_lines = self
            .request
            .content()
            .lines()
            .count()
            .clamp(2, 6) as f32;
        let header_height = (window.rem_size() * (request_lines * 1.5 + 1.))
            .min(window.viewport_size().height * 0.20);
        let copy = (!matches!(self.retained, Retained::Loading(_))).then(|| {
            Clipboard::new(("copy-tool-output", self.stream as usize))
                .small()
                .value_fn({
                    let view = cx.entity().downgrade();
                    move |_, cx| {
                        view.upgrade()
                            .map(|view| view.read(cx).displayed.clone())
                            .unwrap_or_default()
                            .into()
                    }
                })
                .tooltip("复制结果")
                .accessibility_label(format!("复制{}", self.stream.label()))
        });
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .gap_3()
            // Forward the editor's unhandled Escape to the native dialog close path.
            .on_action(|_: &gpui_kit::component::input::Escape, window, cx| {
                window.dispatch_action(Box::new(gpui_kit::base::actions::Cancel), cx)
            })
            .child(
                v_flex()
                    .flex_shrink_0()
                    .min_h_0()
                    .min_w_0()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .px_3()
                            .h(rems(2.))
                            .flex_shrink_0()
                            .gap_2()
                            .text_xs()
                            .child(self.request.heading())
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_ellipsis()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(status),
                            )
                            .when_some(target, |bar, target| {
                                bar.child(
                                    Button::new("inspect-tool-target")
                                        .small()
                                        .ghost()
                                        .label(target.label())
                                        .on_click(cx.listener(|view, _, window, cx| {
                                            view.inspect_target(window, cx)
                                        })),
                                )
                            }),
                    )
                    .child(
                        div()
                            .h(header_height)
                            .min_h_0()
                            .min_w_0()
                            .child(editor_content(
                                &self.request_editor,
                                Some(
                                    Clipboard::new("copy-tool-request")
                                        .small()
                                        .value(self.request.content().to_owned())
                                        .tooltip("复制调用内容")
                                        .accessibility_label("复制调用内容"),
                                ),
                                self.request.heading().into(),
                            )),
                    )
                    .when(!self.request.context().is_empty(), |view| {
                        view.child(
                            div()
                                .id("tool-request-context")
                                .max_h(rems(4.))
                                .overflow_y_scroll()
                                .px_3()
                                .py_2()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .whitespace_normal()
                                .child(self.request.context().to_owned()),
                        )
                    }),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .px_2()
                            .h(rems(2.5))
                            .flex_shrink_0()
                            .min_w_0()
                            .gap_2()
                            .child(
                                TabBar::new("tool-output-streams")
                                    .small()
                                    .underline()
                                    .flex_1()
                                    .min_w_0()
                                    .menu(false)
                                    .selected_index(selected)
                                    .children(
                                        streams
                                            .iter()
                                            .map(|stream| Tab::new().label(stream.label())),
                                    )
                                    .on_click(cx.listener(move |view, ix: &usize, window, cx| {
                                        view.select(streams[*ix], window, cx)
                                    })),
                            )
                            .when(
                                self.paused
                                    || (!self.disconnected
                                        && self.tool.status == ToolStatus::Running
                                        && self.tool.is_foreground_command()),
                                |bar| {
                                    bar.child(
                                        Button::new("pause-output-updates")
                                            .small()
                                            .ghost()
                                            .icon(if self.paused {
                                                IconName::Play
                                            } else {
                                                IconName::Pause
                                            })
                                            .tooltip(if self.paused {
                                                "继续更新"
                                            } else {
                                                "暂停更新"
                                            })
                                            .on_click(cx.listener(|view, _, window, cx| {
                                                view.paused = !view.paused;
                                                if !view.paused {
                                                    view.refresh(window, cx);
                                                }
                                                cx.notify();
                                            })),
                                    )
                                },
                            )
                            .when(matches!(self.retained, Retained::Failed { .. }), |bar| {
                                bar.child(
                                    Button::new("retry-tool-output")
                                        .small()
                                        .ghost()
                                        .icon(IconName::RefreshCw)
                                        .tooltip("重新读取结果")
                                        .accessibility_label("重新读取结果")
                                        .on_click(cx.listener(|view, _, window, cx| {
                                            view.retained = Retained::default();
                                            view.refresh(window, cx);
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("tool-output-content")
                            .occlude()
                            .flex_1()
                            .min_h_0()
                            .min_w_0()
                            .overflow_hidden()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .child(editor_content(
                                &self.editor,
                                copy,
                                format!("工具输出 {}", self.stream.label()).into(),
                            )),
                    ),
            )
    }
}

/// The copy control belongs to the viewport, so editor scrolling cannot move it.
fn editor_content(
    editor: &Entity<EditorState>,
    copy: Option<Clipboard>,
    label: SharedString,
) -> impl IntoElement {
    div()
        .relative()
        .size_full()
        .min_w_0()
        .min_h_0()
        .child(
            Editor::new(editor)
                .readonly(true)
                .bordered(false)
                .h_full()
                .pr_10()
                .aria_label(label),
        )
        .when_some(copy, |body, copy| {
            body.child(
                div()
                    .absolute()
                    .top_2()
                    .right_2()
                    .child(copy),
            )
        })
}
