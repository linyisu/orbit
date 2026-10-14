//! Native, read-only evidence received from the session service. This view
//! never opens output paths, executes a command, or claims a PTY connection.
use super::*;
use crate::execution::{ToolRun, ToolStatus};
use gpui_kit::component::{
    input::{Editor, EditorState},
    scroll::ScrollableElement,
    tab::{Tab, TabBar},
    tooltip::Tooltip,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stream {
    Stdout,
    Stderr,
    Command,
    Result,
    Arguments,
}

impl Stream {
    fn label(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Command => "命令",
            Self::Result => "结果",
            Self::Arguments => "参数",
        }
    }
}

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
        let stream = if tool.name == "run_command"
            && tool.is_foreground_command()
            && (tool.status != ToolStatus::Failed || tool.command_result().is_some())
        {
            Stream::Stdout
        } else {
            Stream::Result
        };
        let displayed = content(&tool, stream);
        let args = serde_json::from_str::<serde_json::Value>(&tool.arguments).ok();
        let command = args.as_ref().and_then(|args| args["command"].as_str());
        let command = command.unwrap_or(&tool.name).to_owned();
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

    pub(super) fn sync(
        &mut self,
        tool: &ToolRun,
        restoring: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Replayed events have new observer generations. A restored call can
        // be opened explicitly, but cannot take ownership of an older viewer.
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
        if self.tool.name == "run_command" {
            &[
                Stream::Stdout,
                Stream::Stderr,
                Stream::Command,
                Stream::Result,
                Stream::Arguments,
            ]
        } else {
            &[Stream::Result, Stream::Arguments]
        }
    }
}

pub(super) fn present(panel: Entity<ToolOutput>, window: &mut Window, cx: &mut App) {
    panel.update(cx, |panel, cx| {
        panel.closed = false;
        panel.refresh(window, cx);
    });
    crate::dialog::open(window, cx, move |dialog, window, _| {
        let close = panel.downgrade();
        let height = window.viewport_size().height * 0.8;
        let content = panel.clone();
        dialog
            .title("工具输出")
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
                            .child(div().text_sm().child(status(&self.tool))),
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

fn content(tool: &ToolRun, stream: Stream) -> String {
    if stream == Stream::Command {
        return serde_json::from_str::<serde_json::Value>(&tool.arguments)
            .ok()
            .and_then(|args| args["command"].as_str().map(str::to_owned))
            .unwrap_or_default();
    }
    if stream == Stream::Arguments {
        return pretty(&tool.arguments);
    }
    if let Some(result) = tool.command_result() {
        match stream {
            Stream::Stdout => return result.stdout,
            Stream::Stderr => return result.stderr,
            _ => {}
        }
    }
    if stream == Stream::Stdout {
        return tool.stdout_preview.clone().unwrap_or_default();
    }
    if stream == Stream::Stderr {
        return String::new();
    }
    let Some(output) = &tool.output else {
        return String::new();
    };
    if tool.name == "read_command_output"
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(output)
        && let Some(content) = value["content"].as_str()
    {
        return content.into();
    }
    pretty(output)
}

fn pretty(text: &str) -> String {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(|| text.into())
}

fn request_directory(tool: &ToolRun, workspace: &str) -> String {
    let args = serde_json::from_str::<serde_json::Value>(&tool.arguments).ok();
    args.as_ref()
        .and_then(|args| args["cwd"].as_str())
        .map_or_else(
            || workspace.into(),
            |cwd| {
                PathBuf::from(workspace)
                    .join(cwd)
                    .to_string_lossy()
                    .into_owned()
            },
        )
}

fn status(tool: &ToolRun) -> String {
    if let Some(result) = tool.command_result() {
        let label = if result.cancelled {
            "已取消".into()
        } else if result.timed_out {
            "已超时".into()
        } else {
            result
                .exit_code
                .map_or_else(|| "进程已结束".into(), |code| format!("退出码 {code}"))
        };
        return format!("{label} · {} ms", result.elapsed.as_millis());
    }
    if tool.name == "run_command" && tool.status == ToolStatus::Background && tool.run_finished {
        return "结果未确认".into();
    }
    tool.elapsed_ms.map_or_else(
        || tool.status.label().into(),
        |elapsed| format!("{} · {elapsed} ms", tool.status.label()),
    )
}

fn note(tool: &ToolRun, stream: Stream) -> String {
    if stream == Stream::Command {
        return "完整请求命令，可选择和复制。".into();
    }
    if stream == Stream::Arguments {
        return "工具调用的完整参数，可选择和复制。".into();
    }
    if tool.name == "read_command_output"
        && let Some(output) = &tool.output
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(output)
    {
        return format!(
            "{} 单页快照 · 字节 {}–{} / 已保留 {}{}",
            value["stream"].as_str().unwrap_or("输出"),
            value["offset"],
            value["next_offset"],
            value["total_retained_bytes"],
            if value["truncated"] == true {
                " · 仍有后页"
            } else {
                ""
            },
        );
    }
    if tool.name != "run_command" {
        if tool.output.is_none() && tool.status == ToolStatus::Running {
            return "工具调用正在进行，尚未收到结果。".into();
        }
        return "已接收的工具结果，可选择和复制。".into();
    }
    if let Some(result) = tool.command_result() {
        let mut note = format!(
            "工具返回的有界结果 · stdout {} 字节 · stderr {} 字节",
            result.stdout_total_bytes, result.stderr_total_bytes,
        );
        if result.output_truncated {
            note.push_str(" · 仅显示每路最多 32KiB 的尾部");
        }
        if result.retained_output_truncated {
            note.push_str(" · 原始日志保留不完整");
        }
        if let Some(error) = result.retained_output_error {
            note.push_str(&format!(" · {error}"));
        }
        return note;
    }
    if tool.status == ToolStatus::Background || !tool.is_foreground_command() {
        if tool.run_finished {
            return "当前任务已结束，未收到后台命令的完成结果；此处保留启动回执。".into();
        }
        return "后台启动回执；等待完成事件，此页不提供后台实时日志。".into();
    }
    if tool.status != ToolStatus::Running {
        return "已接收的结果；未识别为命令结果结构，可在“结果”中查看原文。".into();
    }
    if stream == Stream::Stderr {
        return "stderr 将在命令结束后显示工具返回的结果。".into();
    }
    if tool.live {
        "实时 stdout 尾部 · 最多 16KiB · 由服务端提供".into()
    } else {
        "等待 stdout 预览；审批状态和停止任务操作位于主界面。".into()
    }
}

#[cfg(test)]
mod tests;
