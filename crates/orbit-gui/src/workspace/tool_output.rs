//! Native, read-only evidence received from the session service. This view
//! never opens output paths, executes a command, or claims a PTY connection.
use super::*;
use crate::execution::{ToolRun, ToolStatus};
use gpui_kit::component::{
    Placement, Selectable,
    input::{Editor, EditorState},
    scroll::ScrollableElement,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stream {
    Stdout,
    Stderr,
    Result,
    Arguments,
}

impl Stream {
    fn label(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
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
    command: Entity<EditorState>,
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
        let command_text = format!(
            "请求目录：{}\n{}",
            request_directory(&tool, &workspace),
            command.unwrap_or(&tool.name),
        );
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
            command: cx.new(|cx| {
                EditorState::new(window, cx)
                    .language("plaintext")
                    .line_number(false)
                    .soft_wrap(true)
                    .default_value(command_text)
            }),
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
        // be opened explicitly, but cannot take ownership of an older sheet.
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
        self.refresh(window, cx);
        self.editor
            .update(cx, |editor, cx| editor.focus(window, cx));
        cx.notify();
    }

    fn footer(&self, panel: Entity<Self>) -> AnyElement {
        let streams: &[Stream] = if self.tool.name == "run_command" {
            &[
                Stream::Stdout,
                Stream::Stderr,
                Stream::Result,
                Stream::Arguments,
            ]
        } else {
            &[Stream::Result, Stream::Arguments]
        };
        let selected = self.stream;
        let pause = panel.downgrade();
        let copy = panel.downgrade();
        let paused = self.paused;
        h_flex()
            .w_full()
            .min_w_0()
            .flex_wrap()
            .gap_2()
            .child(h_flex().flex_1().min_w_0().flex_wrap().gap_1().children(
                streams.iter().copied().map(|stream| {
                    let target = panel.downgrade();
                    Button::new(SharedString::from(format!(
                        "output-stream:{}",
                        stream.label()
                    )))
                    .small()
                    .ghost()
                    .selected(stream == selected)
                    .label(stream.label())
                    .accessibility_label(format!("查看{}", stream.label()))
                    .on_click(move |_, window, cx| {
                        let _ = target.update(cx, |panel, cx| panel.select(stream, window, cx));
                    })
                }),
            ))
            .child(
                Button::new("pause-output-updates")
                    .small()
                    .ghost()
                    .disabled(
                        !paused
                            && (self.disconnected
                                || self.tool.status != ToolStatus::Running
                                || !self.tool.is_foreground_command()),
                    )
                    .label(if paused {
                        "继续更新"
                    } else {
                        "暂停更新"
                    })
                    .on_click(move |_, window, cx| {
                        let _ = pause.update(cx, |panel, cx| {
                            panel.paused = !panel.paused;
                            if !panel.paused {
                                panel.refresh(window, cx);
                            }
                            cx.notify();
                        });
                    }),
            )
            .child(
                Button::new("copy-tool-output")
                    .small()
                    .outline()
                    .label("复制输出")
                    .disabled(self.displayed.is_empty())
                    .on_click(move |_, _, cx| {
                        if let Some(panel) = copy.upgrade() {
                            let text = panel.read(cx).displayed.clone();
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                        }
                    }),
            )
            .child(
                Button::new("close-tool-output")
                    .small()
                    .ghost()
                    .label("关闭")
                    .on_click(|_, window, cx| {
                        window.dispatch_action(Box::new(gpui_kit::base::actions::Cancel), cx)
                    }),
            )
            .into_any_element()
    }
}

pub(super) fn present(panel: Entity<ToolOutput>, window: &mut Window, cx: &mut App) {
    panel.update(cx, |panel, cx| {
        panel.closed = false;
        panel.refresh(window, cx);
    });
    let footer = cx.new(|cx| OutputFooter {
        _subscription: cx.observe(&panel, |_, _, cx| cx.notify()),
        panel: panel.clone(),
    });
    window.open_sheet_at(Placement::Bottom, cx, move |sheet, window, _| {
        let close = panel.downgrade();
        sheet
            .title("工具输出")
            .size((window.rem_size() * 30.).min(window.viewport_size().height * 0.8))
            .overlay_closable(false)
            .p_0()
            .child(div().size_full().min_h_0().child(panel.clone()))
            .footer(footer.clone())
            .on_close(move |_, _, cx| {
                let _ = close.update(cx, |panel, cx| {
                    panel.closed = true;
                    cx.notify();
                });
            })
    });
}

struct OutputFooter {
    panel: Entity<ToolOutput>,
    _subscription: Subscription,
}

impl Render for OutputFooter {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.panel.read(cx).footer(self.panel.clone())
    }
}

impl Render for ToolOutput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_owner = self.owner.upgrade().is_some();
        let note = if self.disconnected || !has_owner {
            "已停止跟随当前调用，保留已接收输出。可从执行列表重新打开当前调用。".into()
        } else if self.paused {
            "已暂停更新，选择和复制保留在当前内容；继续更新后显示最新输出。".into()
        } else {
            note(&self.tool, self.stream)
        };
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            // Editors resolve Escape to Input's action. Forward an unhandled
            // one to the same Sheet cancellation path as its native close X.
            .on_action(|_: &gpui_kit::component::input::Escape, window, cx| {
                window.dispatch_action(Box::new(gpui_kit::base::actions::Cancel), cx)
            })
            .child(
                v_flex()
                    .flex_shrink_0()
                    .px_4()
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
                            Editor::new(&self.command)
                                .readonly(true)
                                .bordered(false)
                                .h(window.rem_size() * 2.5)
                                .aria_label("完整命令与请求目录"),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_normal()
                            .max_h(window.rem_size() * 3.)
                            .overflow_y_scrollbar()
                            .child(note),
                    ),
            )
            .child(
                div().flex_1().min_h_0().min_w_0().overflow_hidden().child(
                    Editor::new(&self.editor)
                        .readonly(true)
                        .bordered(false)
                        .h_full()
                        .aria_label(format!("工具输出 {}", self.stream.label())),
                ),
            )
    }
}

fn content(tool: &ToolRun, stream: Stream) -> String {
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
