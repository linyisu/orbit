//! Native, read-only evidence received from the session service. Explicit image
//! and subtask navigation goes through the owning session; no command runs here.
use super::*;
use crate::execution::{ToolRun, ToolStatus};
use gpui_kit::component::{
    input::{Editor, EditorState},
    scroll::ScrollableElement,
    tab::{Tab, TabBar},
    tooltip::Tooltip,
};

use orbit_client::execution::details::{
    Stream, content, default_stream, note, request_directory, status,
};

pub(super) struct ToolOutput {
    owner: WeakEntity<Workspace>,
    options: Options,
    session: String,
    workspace: String,
    tool: ToolRun,
    stream: Stream,
    editor: Entity<EditorState>,
    command: String,
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
        let command = content(&tool, Stream::Command);
        Self {
            owner,
            options,
            session,
            workspace,
            editor: cx.new(|cx| {
                EditorState::new(window, cx)
                    .language("plaintext")
                    .line_number(false)
                    .soft_wrap(false)
                    .default_value(displayed.clone())
            }),
            command,
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
        if self.tool.status == ToolStatus::Failed
            && self.tool.command_result().is_none()
            && self.tool.stdout_preview.is_none()
            && self.stream == Stream::Stdout
        {
            self.stream = Stream::Result;
        }
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
        let next = content(&self.tool, self.stream);
        if next != self.displayed {
            self.editor.update(cx, |editor, cx| {
                editor.set_value(next.clone(), window, cx);
            });
            self.displayed = next;
        }
    }

    fn select(&mut self, stream: Stream, window: &mut Window, cx: &mut Context<Self>) {
        self.stream = stream;
        self.editor.update(cx, |editor, cx| {
            editor.set_soft_wrap(stream == Stream::Command, window, cx);
        });
        self.refresh(window, cx);
        self.editor
            .update(cx, |editor, cx| editor.focus(window, cx));
        cx.notify();
    }

    fn streams(&self) -> &'static [Stream] {
        orbit_client::execution::details::streams(&self.tool)
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
        let rows = panel
            .read(cx)
            .displayed
            .lines()
            .count()
            .clamp(8, 28);
        let height =
            (window.rem_size() * (rows as f32 * 1.3 + 9.)).min(window.viewport_size().height * 0.8);
        let content = panel.clone();
        dialog
            .title("工具详情")
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
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_owner = self.owner.upgrade().is_some();
        let note = if self.disconnected || !has_owner {
            "已停止跟随当前调用，保留已接收输出。可从执行列表重新打开当前调用。".into()
        } else if self.paused {
            "已暂停更新，选择和复制保留在当前内容；继续更新后显示最新输出。".into()
        } else {
            note(&self.tool, self.stream)
        };
        let streams = self.streams();
        let target = orbit_client::execution::details::target(&self.tool)
            .filter(|_| !self.disconnected && has_owner)
            .filter(|target| !matches!(target, orbit_client::execution::details::Target::Question));
        let selected = streams
            .iter()
            .position(|stream| *stream == self.stream)
            .unwrap_or(0);
        let directory = request_directory(&self.tool, &self.workspace);
        let summary = self
            .command
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or(&self.tool.name)
            .to_owned();
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            // Editors resolve Escape to Input's action. Forward an unhandled
            // one to the same Dialog cancellation path as its native close X.
            .on_action(|_: &gpui_kit::component::input::Escape, window, cx| {
                window.dispatch_action(Box::new(gpui_kit::base::actions::Cancel), cx)
            })
            .child(
                v_flex()
                    .flex_shrink_0()
                    .py_2()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .text_ellipsis()
                            .child(self.tool.summary()),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .text_ellipsis()
                                    .child(self.tool.name.clone()),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .child(status(&self.tool)),
                            ),
                    )
                    .when(self.tool.name == "run_command", |view| {
                        view.child(
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_2()
                                .child(Icon::new(IconName::SquareTerminal).small())
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .text_ellipsis()
                                        .child(summary),
                                )
                                .child(
                                    Button::new("view-full-command")
                                        .small()
                                        .ghost()
                                        .label("完整命令")
                                        .on_click(cx.listener(|view, _, window, cx| {
                                            view.select(Stream::Command, window, cx)
                                        })),
                                ),
                        )
                    })
                    .child(
                        div()
                            .id("tool-output-directory")
                            .w_full()
                            .min_w_0()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .text_ellipsis()
                            .tooltip({
                                let directory = directory.clone();
                                move |window, cx| Tooltip::new(directory.clone()).build(window, cx)
                            })
                            .child(format!("目录：{directory}")),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_shrink_0()
                    .flex_wrap()
                    .gap_2()
                    .pb_2()
                    .when_some(target, |bar, target| {
                        bar.child(
                            Button::new("inspect-tool-target")
                                .small()
                                .outline()
                                .label(target.label())
                                .on_click(cx.listener(|view, _, window, cx| {
                                    view.inspect_target(window, cx)
                                })),
                        )
                    })
                    .child(
                        TabBar::new("tool-output-streams")
                            .small()
                            .underline()
                            .flex_1()
                            .min_w_0()
                            .menu(true)
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
                                    .label(if self.paused {
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
                    .child(
                        Button::new("copy-tool-output")
                            .small()
                            .outline()
                            .icon(IconName::Copy)
                            .label("复制")
                            .accessibility_label(format!("复制{}", self.stream.label()))
                            .disabled(self.displayed.is_empty())
                            .on_click(cx.listener(|view, _, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    view.displayed.clone(),
                                ))
                            })),
                    ),
            )
            .child(
                div()
                    .id("tool-output-content")
                    .occlude()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .border_y_1()
                    .border_color(cx.theme().border)
                    .child(
                        Editor::new(&self.editor)
                            .readonly(true)
                            .bordered(false)
                            .h_full()
                            .aria_label(format!("工具输出 {}", self.stream.label())),
                    ),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .w_full()
                    .h_auto()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .max_h(rems(4.))
                    .overflow_y_scrollbar()
                    .child(note),
            )
    }
}
