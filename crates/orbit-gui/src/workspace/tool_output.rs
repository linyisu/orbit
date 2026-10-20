//! Native, read-only evidence received from the session service. Explicit image
//! and subtask navigation goes through the owning session; no command runs here.
use super::*;
use crate::execution::{ToolRun, ToolStatus};
use gpui_kit::component::{
    clipboard::Clipboard,
    diff::{Diff, DiffFile, DiffState},
    input::{Editor, EditorState, RopeExt},
    tab::{Tab, TabBar},
};

use orbit_client::execution::details::{
    Request as ToolRequest, RequestLayout, Stream, content, default_stream, failure,
    retained::Retained, source_text, status,
};

enum ImageContent {
    Idle,
    Loading,
    Ready {
        image: std::sync::Arc<Image>,
        dimensions: (u32, u32),
    },
    Failed(SharedString),
}

pub(super) struct ToolOutput {
    owner: WeakEntity<Workspace>,
    options: Options,
    session: String,
    workspace: String,
    tool: ToolRun,
    stream: Stream,
    selected_stream: bool,
    diff: Entity<DiffState>,
    diff_patch: Option<String>,
    editor: Entity<EditorState>,
    request_editor: Entity<EditorState>,
    request_height: Option<Pixels>,
    image: ImageContent,
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
            diff: cx.new(|cx| DiffState::new([], cx)),
            diff_patch: None,
            selected_stream: false,
            owner,
            options,
            session,
            workspace,
            editor: cx.new(|cx| {
                EditorState::new(window, cx)
                    .language("plaintext")
                    .line_number(true)
                    .folding(false)
                    .indent_guides(false)
                    .soft_wrap(true)
                    .scroll_beyond_last_line(Some(0))
                    .default_value(displayed.clone())
            }),
            request_editor: cx.new(|cx| {
                EditorState::new(window, cx)
                    .language(request.language())
                    .line_number(true)
                    .folding(false)
                    .indent_guides(false)
                    .soft_wrap(true)
                    .scroll_beyond_last_line(Some(0))
                    .default_value(request.content())
            }),
            request,
            request_height: None,
            image: ImageContent::Idle,
            pages: orbit_client::execution::details::streams(&tool),
            status: status(&tool),
            retained: Retained::default(),
            tool,
            stream,
            displayed: String::new(),
            paused: false,
            disconnected: false,
            closed: false,
        }
    }

    #[cfg(feature = "test-support")]
    pub(super) fn result_focus(&self, cx: &App) -> FocusHandle {
        self.editor.focus_handle(cx)
    }

    pub(super) fn title(&self) -> String {
        self.tool.summary()
    }

    pub(super) fn set_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.closed = !open;
        if open {
            self.refresh(window, cx);
        }
        cx.notify();
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
        self.request = ToolRequest::new(tool, &self.workspace);
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
        if !self.selected_stream || !self.pages.contains(&self.stream) {
            self.stream = default_stream(&self.tool);
        }
        self.status = status(&self.tool);
        if !self.disconnected && self.retained.begin(&self.tool) {
            let (result, response) = tokio::sync::oneshot::channel();
            let session = self.session.clone();
            let tool = Box::new(self.tool.clone());
            let (output, previous) = self.retained.pending().unwrap();
            let owner = self.owner.clone();
            cx.spawn_in(window, async move |view, cx| {
                // Opening and syncing can run inside the workspace's update.
                // Dispatch after that update releases its entity borrow.
                let _ = owner.update(cx, |workspace, cx| {
                    workspace.command(
                        Command::ToolOutput {
                            session,
                            tool,
                            previous,
                            result,
                        },
                        cx,
                    );
                });
                let result = response
                    .await
                    .unwrap_or_else(|_| Err("读取已取消或连接已断开".into()));
                let _ = view.update_in(cx, |view, window, cx| {
                    if view.retained.receive(&output, result) && !view.closed && !view.paused {
                        view.refresh(window, cx);
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        let source = (self.stream == Stream::Result)
            .then(|| source_text(&self.tool))
            .flatten();
        let next = source.as_ref().map_or_else(
            || {
                self.retained
                    .content(&self.tool, self.stream)
            },
            |source| source.text().to_owned(),
        );
        let first_line = source
            .as_ref()
            .map_or(1, |source| source.first_line());
        self.editor.update(cx, |editor, cx| {
            editor.set_line_number_start(first_line, cx);
        });
        if next != self.displayed {
            if self.stream == Stream::Changes && self.diff_patch.as_deref() != Some(&next) {
                let patch = next.clone();
                cx.spawn(async move |view, cx| {
                    let parsed = cx
                        .background_spawn({
                            let patch = patch.clone();
                            async move { DiffFile::parse(&patch) }
                        })
                        .await;
                    let _ = view.update(cx, |view, cx| {
                        if view.displayed == patch
                            && let Ok(files) = parsed
                        {
                            view.diff
                                .update(cx, |diff, cx| diff.set_files(files, cx));
                            view.diff_patch = Some(patch);
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            let live_log = self.tool.name == "Exec"
                && matches!(self.stream, Stream::Stdout | Stream::Stderr)
                && (matches!(
                    self.tool.status,
                    ToolStatus::Running | ToolStatus::Background
                ) || !self.displayed.is_empty());
            let append_from =
                (live_log && !self.displayed.is_empty() && next.starts_with(&self.displayed))
                    .then_some(self.displayed.len());
            let language = match self.stream {
                Stream::Raw => "json",
                Stream::Changes => "diff",
                Stream::Result if self.tool.name == "ReadFile" => {
                    orbit_syntax::label_for_path(self.request.content())
                }
                _ => "plaintext",
            };
            let previous_focus = window.focused(cx);
            self.editor.update(cx, |editor, cx| {
                let offset = editor.scroll_offset();
                let selection = editor.selected_range();
                let following = live_log
                    && selection.is_empty()
                    && editor
                        .visible_row_range()
                        .is_none_or(|range| range.end >= editor.text().lines_len());
                if editor.language_name().as_ref() != language {
                    editor.set_highlighter(language, cx);
                }
                if let Some(start) = append_from {
                    let end = editor.text().len();
                    editor.set_selected_range(end..end, cx);
                    editor.insert(next[start..].to_owned(), window, cx);
                    editor.set_selected_range(selection, cx);
                } else {
                    editor.set_value(next.clone(), window, cx);
                }
                if following {
                    let end = editor
                        .text()
                        .offset_to_position(editor.text().len());
                    editor.set_cursor_position(end, window, cx);
                } else {
                    editor.set_scroll_offset(offset, cx);
                }
            });
            if let Some(focus) = previous_focus {
                window.focus(&focus, cx);
            }
            self.displayed = next;
        }
    }

    fn select(&mut self, stream: Stream, window: &mut Window, cx: &mut Context<Self>) {
        self.stream = stream;
        self.selected_stream = true;
        self.refresh(window, cx);
        if stream == Stream::Changes {
            self.diff
                .focus_handle(cx)
                .focus(window, cx);
        } else {
            self.editor.update(cx, |editor, cx| {
                editor.set_scroll_offset(point(px(0.), px(0.)), cx);
                editor.focus(window, cx);
            });
        }
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
            }
            Target::Image(path) => {
                if !matches!(self.image, ImageContent::Idle) {
                    return;
                }
                self.image = ImageContent::Loading;
                cx.notify();
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
                    let _ = panel.update(cx, |panel, cx| {
                        panel.image = match image {
                            Ok((image, dimensions)) => ImageContent::Ready { image, dimensions },
                            Err(error) => ImageContent::Failed(error.into()),
                        };
                        cx.notify();
                    });
                })
                .detach();
            }
            Target::Question => self.select(Stream::Result, window, cx),
            Target::Plan(state) => {
                let owner = self.owner.clone();
                cx.spawn_in(window, async move |_, cx| {
                    let _ = owner.update_in(cx, |workspace, window, cx| {
                        workspace.plan_record = Some(*state);
                        workspace.set_inspector(Some(inspector::Section::Plan), window, cx);
                    });
                })
                .detach();
            }
        }
    }
}

pub(super) fn present(panel: Entity<ToolOutput>, window: &mut Window, cx: &mut App) {
    panel.update(cx, |panel, cx| {
        panel.set_open(true, window, cx);
        if matches!(
            orbit_client::execution::details::target(&panel.tool),
            Some(orbit_client::execution::details::Target::Image(_))
        ) {
            cx.defer_in(window, |panel, window, cx| panel.inspect_target(window, cx));
        }
    });
}

impl Render for ToolOutput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if matches!(
            orbit_client::execution::details::target(&self.tool),
            Some(orbit_client::execution::details::Target::Image(_))
        ) {
            return self
                .render_image(window, cx)
                .into_any_element();
        }
        let has_owner = self.owner.upgrade().is_some();
        let status = if self.disconnected || !has_owner {
            "连接已断开，保留已接收输出".to_owned()
        } else if self.paused {
            "已暂停更新".to_owned()
        } else if self.status.is_empty() {
            self.tool.status.label().to_owned()
        } else {
            self.status.clone()
        };
        let call_failure = failure(&self.tool);
        let streams = self.pages;
        let target = orbit_client::execution::details::target(&self.tool)
            .filter(|_| !self.disconnected && has_owner)
            .filter(|target| !matches!(target, orbit_client::execution::details::Target::Question));
        let selected = streams
            .iter()
            .position(|stream| *stream == self.stream)
            .unwrap_or(0);
        let request_padding = window.rem_size();
        let header_height = self
            .request_height
            .unwrap_or(window.rem_size() * 4.)
            .min(window.viewport_size().height * 0.20);
        let request_owner = cx.entity().downgrade();
        let request_editor = self.request_editor.clone();
        let measured_height = self.request_height;
        let background = cx
            .theme()
            .highlight_theme
            .style
            .editor_background
            .unwrap_or_else(|| cx.theme().input_background());
        let copy_owner = cx.entity().downgrade();
        let copy = || {
            Clipboard::new(("copy-tool-output", self.stream as usize))
                .small()
                .value_fn({
                    let view = copy_owner.clone();
                    move |_, cx| {
                        view.upgrade()
                            .map(|view| view.read(cx).displayed.clone())
                            .unwrap_or_default()
                            .into()
                    }
                })
                .tooltip("复制结果")
                .accessibility_label(format!("复制{}", self.stream.label()))
        };
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .gap_3()
            .child(
                div()
                    .flex_shrink_0()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(format!("{} · {status}", self.tool.name)),
            )
            .when(self.request.layout() == RequestLayout::Body, |view| {
                view.child(
                    v_flex()
                        .flex_shrink_0()
                        .min_h_0()
                        .min_w_0()
                        .overflow_hidden()
                        .child(
                            h_flex()
                                .h(rems(2.))
                                .flex_shrink_0()
                                .gap_2()
                                .text_xs()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_ellipsis()
                                        .child(self.request.heading()),
                                )
                                .when_some(target.clone(), |bar, target| {
                                    bar.child(
                                        Button::new("inspect-tool-target")
                                            .small()
                                            .ghost()
                                            .label(target.label())
                                            .on_click(cx.listener(|view, _, window, cx| {
                                                view.inspect_target(window, cx)
                                            })),
                                    )
                                })
                                .child(
                                    Clipboard::new("copy-tool-request")
                                        .small()
                                        .value(self.request.content().to_owned())
                                        .tooltip("复制调用内容")
                                        .accessibility_label("复制调用内容"),
                                ),
                        )
                        .child(
                            div()
                                .relative()
                                .h(header_height)
                                .flex_shrink_0()
                                .min_h_0()
                                .min_w_0()
                                .rounded_md()
                                .border_1()
                                .border_color(cx.theme().border)
                                .overflow_hidden()
                                .child(editor_content(
                                    &self.request_editor,
                                    self.request.heading().into(),
                                    background,
                                    request_padding,
                                ))
                                .child(
                                    canvas(
                                        |_, _, _| {},
                                        move |_, _, _, cx| {
                                            let editor = request_editor.read(cx);
                                            let Some(line_height) = editor.line_height() else {
                                                return;
                                            };
                                            // Editor publishes its geometry during paint. Read
                                            // after it so wrapping and font metrics agree.
                                            // Editor also requires a two-row minimum viewport.
                                            let height = line_height
                                                * editor
                                                    .display_map()
                                                    .display_row_count()
                                                    .max(2)
                                                + request_padding / 3.;
                                            if measured_height == Some(height) {
                                                return;
                                            }
                                            let owner = request_owner.clone();
                                            cx.defer(move |cx| {
                                                let _ = owner.update(cx, |view, cx| {
                                                    if view.request_height != Some(height) {
                                                        view.request_height = Some(height);
                                                        cx.notify();
                                                    }
                                                });
                                            });
                                        },
                                    )
                                    .absolute()
                                    .top_0()
                                    .left_0()
                                    .size_full(),
                                ),
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
            })
            .when(self.request.layout() == RequestLayout::Summary, |view| {
                view.child(
                    v_flex()
                        .flex_shrink_0()
                        .min_w_0()
                        .gap_1()
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap_2()
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_sm()
                                        .whitespace_normal()
                                        .child(self.request.content().to_owned()),
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
                        .when(!self.request.context().is_empty(), |view| {
                            view.child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(self.request.context().to_owned()),
                            )
                        }),
                )
            })
            .when_some(call_failure, |view, failure| {
                view.child(
                    v_flex()
                        .flex_shrink_0()
                        .min_w_0()
                        .gap_1()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .bg(cx.theme().muted.opacity(0.15))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(
                                    Icon::new(if self.tool.status == ToolStatus::Stopped {
                                        IconName::Square
                                    } else {
                                        IconName::CircleAlert
                                    })
                                    .small(),
                                )
                                .child(failure.title()),
                        )
                        .when(!failure.guidance().is_empty(), |card| {
                            card.child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .whitespace_normal()
                                    .child(failure.guidance()),
                            )
                        }),
                )
            })
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .child(
                        h_flex()
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
                                    .children(streams.iter().map(|stream| {
                                        Tab::new().label(
                                            if *stream == Stream::Result
                                                && failure(&self.tool).is_some()
                                            {
                                                "错误详情"
                                            } else {
                                                stream.label()
                                            },
                                        )
                                    }))
                                    .on_click(cx.listener(move |view, ix: &usize, window, cx| {
                                        view.select(streams[*ix], window, cx)
                                    })),
                            )
                            .child(copy())
                            .when(
                                self.paused
                                    || (!self.disconnected
                                        && matches!(
                                            self.tool.status,
                                            ToolStatus::Running | ToolStatus::Background
                                        )
                                        && self.tool.name == "Exec"),
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
                            .when(
                                self.stream == Stream::Changes
                                    && self.diff_patch.as_deref() == Some(&self.displayed),
                                |body| {
                                    body.child(
                                        Diff::new(&self.diff)
                                            .single_line_number(true)
                                            .soft_wrap(true)
                                            .size_full(),
                                    )
                                },
                            )
                            .when(
                                self.stream != Stream::Changes
                                    || self.diff_patch.as_deref() != Some(&self.displayed),
                                |body| {
                                    body.child(editor_content(
                                        &self.editor,
                                        format!("工具输出 {}", self.stream.label()).into(),
                                        cx.theme().background,
                                        window.rem_size(),
                                    ))
                                },
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl ToolOutput {
    fn render_image(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .min_w_0()
            .gap_2()
            .child(
                div()
                    .w_full()
                    .text_sm()
                    .whitespace_normal()
                    .child(self.request.content().to_owned()),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(match &self.image {
                        ImageContent::Ready { image, dimensions } => {
                            let image = image.clone();
                            let dimensions = *dimensions;
                            Button::new("tool-image-thumbnail")
                                .ghost()
                                .w((window.rem_size() * 12.).min(crate::dialog::width(window)))
                                .h((window.rem_size() * 8.)
                                    .min(window.viewport_size().height * 0.4))
                                .p_0()
                                .accessibility_label("查看大图")
                                .tooltip("查看大图")
                                .child(
                                    img(image.clone())
                                        .size_full()
                                        .object_fit(ObjectFit::Contain),
                                )
                                .on_click(move |_, window, cx| {
                                    crate::image_preview::open(
                                        Ok(Some(image.clone())),
                                        Some(dimensions),
                                        window,
                                        cx,
                                    );
                                })
                                .into_any_element()
                        }
                        ImageContent::Failed(error) => div()
                            .text_sm()
                            .whitespace_normal()
                            .child(
                                orbit_client::directories::display_diagnostic(error.as_ref())
                                    .into_owned(),
                            )
                            .into_any_element(),
                        ImageContent::Idle
                            if self.disconnected || self.owner.upgrade().is_none() =>
                        {
                            div()
                                .text_sm()
                                .child("连接已断开，无法加载图片")
                                .into_any_element()
                        }
                        ImageContent::Idle | ImageContent::Loading => div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("加载图片…")
                            .into_any_element(),
                    }),
            )
    }
}

/// The surrounding section owns commands; scrolling belongs only to the read-only editor.
fn editor_content(
    editor: &Entity<EditorState>,
    label: SharedString,
    background: Hsla,
    padding_right: Pixels,
) -> impl IntoElement {
    div()
        .relative()
        .size_full()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .bg(background)
        .child(
            Editor::new(editor)
                .readonly(true)
                .bordered(false)
                .w_full()
                .h_full()
                .content_padding(Edges {
                    top: padding_right / 6.,
                    bottom: padding_right / 6.,
                    left: padding_right / 8.,
                    right: padding_right,
                })
                .aria_label(label),
        )
}
