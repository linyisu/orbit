//! Typed approval previews. Rendering never changes the operation being approved.
use super::*;
use gpui_kit::component::{
    collapsible::Collapsible,
    diff::{Diff, DiffFile, DiffMode, DiffState},
    input::{Editor, EditorState},
};
use orbit_client::protocol::approval::{ApprovalPreview, CommandWorkspaceAccess};
use serde_json::Value;

impl Workspace {
    pub(super) fn approval(
        &mut self,
        approval: &Approval,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = &self.info else {
            return;
        };
        let session = info.id.clone();
        let id = approval.id;
        let decision = attention::Decision::Approval(id);
        self.attention.present(decision.clone());
        let title = title(&approval.preview);
        let destructive = matches!(approval.preview, ApprovalPreview::Delete(_));
        let content = cx.new(|cx| {
            ApprovalContent::new(approval.preview.clone(), info.workspace.clone(), window, cx)
        });
        let owner = cx.weak_entity();
        crate::dialog::open(window, cx, move |dialog, _, _| {
            let buttons = [
                (false, false, "拒绝"),
                (true, false, "允许一次"),
                (true, true, "记住并允许"),
            ]
            .into_iter()
            .map(|(allow, remember, label)| {
                let owner = owner.clone();
                let session = session.clone();
                Button::new(if remember {
                    "remember"
                } else if allow {
                    "allow"
                } else {
                    "deny"
                })
                .label(label)
                .when(allow && !remember, |button| {
                    if destructive {
                        button.danger()
                    } else {
                        button.primary()
                    }
                })
                .on_click(move |_, window, cx| {
                    let accepted = owner
                        .update(cx, |view, cx| {
                            if !view
                                .info
                                .as_ref()
                                .is_some_and(|info| info.id == session)
                                || !view
                                    .approvals
                                    .iter()
                                    .any(|approval| approval.id == id)
                            {
                                view.report_error("审批已结束或会话已切换，请关闭后刷新。", cx);
                                return false;
                            }
                            let accepted = view.command(
                                Command::Request(Request::Approve {
                                    session: session.clone(),
                                    approval: id,
                                    allow,
                                    remember,
                                }),
                                cx,
                            );
                            if accepted {
                                view.attention
                                    .dismiss(&attention::Decision::Approval(id));
                            }
                            accepted
                        })
                        .unwrap_or(false);
                    if accepted {
                        window.close_dialog(cx);
                    }
                })
            });
            let close_owner = owner.clone();
            let decision = decision.clone();
            dialog
                .on_ok(|_, _, _| false)
                .on_close(move |_, _, cx| {
                    let _ = close_owner.update(cx, |view, _| view.attention.dismiss(&decision));
                })
                .title(title)
                .child(content.clone())
                .footer(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .justify_end()
                        .children(buttons),
                )
        });
    }
}

fn title(preview: &ApprovalPreview) -> &'static str {
    match preview {
        ApprovalPreview::Write(write) if write.before.is_none() => "审批新建文件",
        ApprovalPreview::Write(_) => "审批写入文件",
        ApprovalPreview::Delete(_) => "审批删除文件",
        ApprovalPreview::Command(_) => "审批执行命令",
        ApprovalPreview::Mcp(_) => "审批 MCP 工具调用",
        ApprovalPreview::Capability(_) => "审批文件访问权限",
    }
}

struct Parameter {
    name: String,
    value: String,
}

struct Arguments {
    fields: Vec<Parameter>,
    raw: String,
    note: Option<String>,
}

fn short(value: &str, limit: usize) -> (String, bool) {
    let mut chars = value.chars();
    let preview = chars
        .by_ref()
        .take(limit)
        .collect::<String>();
    if chars.next().is_some() {
        (format!("{preview}…（已截断）"), true)
    } else {
        (preview, false)
    }
}

fn arguments(source: &str) -> Arguments {
    const MAX_FIELDS: usize = 16;
    let value = match serde_json::from_str::<Value>(source) {
        Ok(value) => value,
        Err(_) => {
            return Arguments {
                fields: Vec::new(),
                raw: source.into(),
                note: Some("参数不是有效 JSON；展开完整参数查看原始内容。".into()),
            };
        }
    };
    let mut shortened = false;
    let mut omitted = 0;
    let fields = match &value {
        Value::Object(values) => {
            omitted = values.len().saturating_sub(MAX_FIELDS);
            values
                .iter()
                .take(MAX_FIELDS)
                .map(|(name, value)| {
                    let display = match value {
                        Value::String(value) => value.clone(),
                        Value::Array(values) => format!("数组，{} 项", values.len()),
                        Value::Object(values) => format!("对象，{} 个字段", values.len()),
                        value => value.to_string(),
                    };
                    let (value, truncated) = short(&display, 240);
                    let (name, name_truncated) = short(name, 80);
                    shortened |= truncated || name_truncated;
                    Parameter { name, value }
                })
                .collect()
        }
        _ => {
            let (value, truncated) = short(&value.to_string(), 240);
            shortened = truncated;
            vec![Parameter {
                name: "参数值".into(),
                value,
            }]
        }
    };
    Arguments {
        fields,
        raw: serde_json::to_string_pretty(&value).unwrap_or_else(|_| source.into()),
        note: if omitted > 0 {
            Some(format!(
                "摘要省略了 {omitted} 个字段；长值与嵌套内容请展开完整参数查看。"
            ))
        } else if shortened {
            Some("摘要中的长值已截断；展开完整参数可查看全部内容。".into())
        } else {
            None
        },
    }
}

fn language(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
    {
        Some("rs") => "rust",
        Some("js" | "mjs" | "cjs") => "javascript",
        Some("ts") => "typescript",
        Some("tsx" | "jsx") => "tsx",
        Some("py") => "python",
        Some("sh" | "bash") => "bash",
        Some("json") => "json",
        Some("toml") => "toml",
        Some("yaml" | "yml") => "yaml",
        Some("html") => "html",
        Some("css") => "css",
        Some("c" | "h") => "c",
        Some("cpp" | "hpp" | "cc") => "cpp",
        Some("go") => "go",
        Some("lua") => "lua",
        _ => "",
    }
}

fn patch(before: &str, after: &str, new_file: bool, deleted: bool) -> String {
    let diff = similar::TextDiff::configure()
        .timeout(Duration::from_secs(2))
        .diff_lines(before, after);
    diff.unified_diff()
        // Keep all source context; Diff virtualizes it and offers fold controls.
        .context_radius(
            before
                .lines()
                .count()
                .max(after.lines().count()),
        )
        // The real target is displayed above the diff. Fixed internal paths
        // keep control characters in filenames out of the patch grammar.
        .header(
            if new_file { "/dev/null" } else { "a/approval" },
            if deleted { "/dev/null" } else { "b/approval" },
        )
        .to_string()
}

enum Prepared {
    File {
        files: Result<Vec<DiffFile>, String>,
        before: String,
        after: String,
        lines: (usize, usize),
        same_content: bool,
        language: &'static str,
    },
    Command(String),
    Mcp(Arguments),
    Capability,
}

fn prepare(preview: ApprovalPreview) -> Prepared {
    match preview {
        ApprovalPreview::Write(write) => {
            let source = patch(
                write
                    .before
                    .as_deref()
                    .unwrap_or_default(),
                &write.after,
                write.before.is_none(),
                false,
            );
            Prepared::File {
                files: DiffFile::parse(&source)
                    .map(|files| {
                        files
                            .into_iter()
                            .map(|file| file.with_language(language(&write.path)))
                            .collect()
                    })
                    .map_err(|error| error.to_string()),
                lines: (
                    write
                        .before
                        .as_deref()
                        .unwrap_or_default()
                        .split_inclusive('\n')
                        .count(),
                    write
                        .after
                        .split_inclusive('\n')
                        .count(),
                ),
                same_content: write.before.as_deref() == Some(write.after.as_str()),
                before: write.before.unwrap_or_default(),
                after: write.after,
                language: language(&write.path),
            }
        }
        ApprovalPreview::Delete(delete) => {
            let source = patch(&delete.before, "", false, true);
            Prepared::File {
                files: DiffFile::parse(&source)
                    .map(|files| {
                        files
                            .into_iter()
                            .map(|file| file.with_language(language(&delete.path)))
                            .collect()
                    })
                    .map_err(|error| error.to_string()),
                lines: (
                    delete
                        .before
                        .split_inclusive('\n')
                        .count(),
                    0,
                ),
                same_content: false,
                before: delete.before,
                after: String::new(),
                language: language(&delete.path),
            }
        }
        ApprovalPreview::Command(command) => Prepared::Command(command.command),
        ApprovalPreview::Mcp(mcp) => Prepared::Mcp(arguments(&mcp.arguments)),
        ApprovalPreview::Capability(_) => Prepared::Capability,
    }
}

struct ApprovalContent {
    preview: ApprovalPreview,
    workspace: String,
    diff: Entity<DiffState>,
    original: Entity<EditorState>,
    code: Entity<EditorState>,
    loading: bool,
    has_diff: bool,
    lines: (usize, usize),
    same_content: bool,
    split: bool,
    error: Option<String>,
    parameters: Vec<Parameter>,
    note: Option<String>,
    raw_open: bool,
    _task: Task<()>,
}

struct FileContent<'a> {
    path: &'a str,
    before: Option<&'a str>,
    after: &'a str,
    deleted: bool,
}

impl ApprovalContent {
    fn new(
        preview: ApprovalPreview,
        workspace: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let background = preview.clone();
        let job = cx
            .background_executor()
            .spawn(async move { prepare(background) });
        let task = cx.spawn(async move |owner, cx| {
            let prepared = job.await;
            let _ = owner.update_in(cx, |view, window, cx| {
                match prepared {
                    Prepared::File {
                        files,
                        before,
                        after,
                        lines,
                        same_content,
                        language,
                    } => {
                        view.lines = lines;
                        view.same_content = same_content;
                        match files {
                            Ok(files) => {
                                view.has_diff = !files.is_empty();
                                view.diff
                                    .update(cx, |diff, cx| diff.set_files(files, cx));
                            }
                            Err(error) => {
                                view.error =
                                    Some(format!("差异无法解析，以下展示完整原文：{error}"))
                            }
                        }
                        if !view.has_diff {
                            view.original.update(cx, |editor, cx| {
                                editor.set_highlighter(language, cx);
                                editor.set_value(before, window, cx);
                            });
                            view.code.update(cx, |editor, cx| {
                                editor.set_highlighter(language, cx);
                                editor.set_value(after, window, cx);
                            });
                        }
                    }
                    Prepared::Command(command) => view.code.update(cx, |editor, cx| {
                        editor.set_highlighter("bash", cx);
                        editor.set_value(command, window, cx);
                    }),
                    Prepared::Mcp(arguments) => {
                        view.parameters = arguments.fields;
                        view.note = arguments.note;
                        view.code.update(cx, |editor, cx| {
                            editor.set_highlighter("json", cx);
                            editor.set_value(arguments.raw, window, cx);
                        });
                    }
                    Prepared::Capability => {}
                }
                view.loading = false;
                cx.notify();
            });
        });
        Self {
            preview,
            workspace,
            diff: cx.new(|cx| DiffState::new([], cx)),
            original: cx.new(|cx| {
                EditorState::new(window, cx)
                    .line_number(true)
                    .soft_wrap(false)
            }),
            code: cx.new(|cx| {
                EditorState::new(window, cx)
                    .line_number(true)
                    .soft_wrap(false)
            }),
            loading: true,
            has_diff: false,
            lines: (0, 0),
            same_content: false,
            split: false,
            error: None,
            parameters: Vec::new(),
            note: None,
            raw_open: false,
            _task: task,
        }
    }

    fn row(
        label: impl Into<SharedString>,
        value: impl Into<SharedString>,
        narrow: bool,
        cx: &App,
    ) -> AnyElement {
        h_flex()
            .items_start()
            .gap_2()
            .when(narrow, |row| row.flex_col())
            .child(
                div()
                    .when(!narrow, |label| label.w_32().flex_shrink_0())
                    .text_color(cx.theme().muted_foreground)
                    .child(label.into()),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .whitespace_normal()
                    .child(value.into()),
            )
            .into_any_element()
    }

    fn file(
        &self,
        file: FileContent<'_>,
        narrow: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let FileContent {
            path,
            before,
            after,
            deleted,
        } = file;
        let mut body = v_flex()
            .gap_3()
            .child(Self::row(
                "文件",
                orbit_client::directories::display_path(path).into_owned(),
                narrow,
                cx,
            ))
            .child(Self::row(
                "工作区",
                orbit_client::directories::display_path(&self.workspace).into_owned(),
                narrow,
                cx,
            ))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.loading {
                        format!(
                            "正在准备预览：写入前 {} 字节，写入后 {} 字节。",
                            before.unwrap_or_default().len(),
                            after.len()
                        )
                    } else if deleted {
                        format!(
                            "将删除整个文件，原文 {} 行、{} 字节。",
                            self.lines.0,
                            before.unwrap_or_default().len()
                        )
                    } else {
                        format!(
                            "写入前 {} 行、{} 字节 → 写入后 {} 行、{} 字节。预览保留完整内容。",
                            self.lines.0,
                            before.unwrap_or_default().len(),
                            self.lines.1,
                            after.len()
                        )
                    }),
            );
        if self.loading {
            return body
                .child(
                    h_flex()
                        .gap_2()
                        .child(gpui_kit::component::spinner::Spinner::new())
                        .child("正在准备完整差异…"),
                )
                .into_any_element();
        }
        if self.has_diff {
            body = body
                .child(
                    h_flex()
                        .justify_between()
                        .gap_2()
                        .flex_wrap()
                        .child(div().text_sm().child("文件变更"))
                        .child(
                            Button::new("approval-diff-mode")
                                .small()
                                .ghost()
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
                        ),
                )
                .child(
                    div()
                        .h(window.viewport_size().height * 0.42)
                        .min_w_0()
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded_md()
                        .overflow_hidden()
                        .child(
                            Diff::new(&self.diff)
                                .single_line_number(true)
                                .header_visible(false),
                        ),
                );
        } else {
            body = body.child(div().text_sm().child(if deleted {
                "文件为空。"
            } else if before.is_none() && after.is_empty() {
                "将创建空文件。"
            } else if self.same_content {
                "写入内容与现有文件相同。"
            } else {
                "完整文件内容"
            }));
            if deleted || self.error.is_some() {
                body = body.child("写入前").child(
                    Editor::new(&self.original)
                        .readonly(true)
                        .h(window.rem_size() * 12.)
                        .aria_label("审批文件原文"),
                );
            }
            if !deleted {
                body = body.child("写入后").child(
                    Editor::new(&self.code)
                        .readonly(true)
                        .h(window.rem_size() * 12.)
                        .aria_label("审批写入内容"),
                );
            }
        }
        body.into_any_element()
    }
}

impl Render for ApprovalContent {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = crate::dialog::width(window) < window.rem_size() * 36.;
        let body = match &self.preview {
            ApprovalPreview::Write(write) => self.file(
                FileContent {
                    path: &write.path,
                    before: write.before.as_deref(),
                    after: &write.after,
                    deleted: false,
                },
                narrow,
                window,
                cx,
            ),
            ApprovalPreview::Delete(delete) => self.file(
                FileContent {
                    path: &delete.path,
                    before: Some(&delete.before),
                    after: "",
                    deleted: true,
                },
                narrow,
                window,
                cx,
            ),
            ApprovalPreview::Command(command) => v_flex()
                .gap_3()
                .child(Self::row(
                    "工作区",
                    orbit_client::directories::display_path(&self.workspace).into_owned(),
                    narrow,
                    cx,
                ))
                .child(Self::row(
                    "执行目录",
                    orbit_client::directories::display_joined_path(
                        &self.workspace,
                        command.cwd.as_deref().unwrap_or("."),
                    ),
                    narrow,
                    cx,
                ))
                .child(Self::row(
                    "工作区权限",
                    match command.workspace_access {
                        CommandWorkspaceAccess::Read => "只读",
                        CommandWorkspaceAccess::ReadWrite => "读写",
                    },
                    narrow,
                    cx,
                ))
                .child(Self::row(
                    "网络",
                    if command.network_allowed {
                        "允许"
                    } else {
                        "未允许"
                    },
                    narrow,
                    cx,
                ))
                .child(Self::row(
                    "外部写入",
                    if command.external_write {
                        "包含已授权的工作区外路径"
                    } else {
                        "未声明"
                    },
                    narrow,
                    cx,
                ))
                .child(Self::row(
                    "超时",
                    format!("{} 秒", command.timeout_seconds),
                    narrow,
                    cx,
                ))
                .when(!command.external_capabilities.is_empty(), |body| {
                    body.child(Self::row(
                        "外部权限",
                        command.external_capabilities.join("\n"),
                        narrow,
                        cx,
                    ))
                })
                .when_some(command.git.as_ref(), |body, git| {
                    body.child(Self::row("Git 操作", git.subcommand.clone(), narrow, cx))
                })
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("这些是命令声明的权限；命令将使用目标环境的系统用户执行。"),
                )
                .child("命令")
                .when(self.loading, |body| body.child("正在准备完整命令…"))
                .child(
                    Editor::new(&self.code)
                        .readonly(true)
                        .h(window.rem_size() * 12.)
                        .aria_label("待执行命令"),
                )
                .into_any_element(),
            ApprovalPreview::Mcp(mcp) => v_flex()
                .gap_3()
                .child(Self::row("服务器", mcp.server.clone(), narrow, cx))
                .child(Self::row("工具", mcp.tool.clone(), narrow, cx))
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("工具将在该 MCP 服务器中执行。"),
                )
                .when(self.loading, |body| body.child("正在准备调用参数…"))
                .children(self.parameters.iter().map(|parameter| {
                    Self::row(parameter.name.clone(), parameter.value.clone(), narrow, cx)
                }))
                .when_some(self.note.clone(), |body, note| {
                    body.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(note),
                    )
                })
                .child(
                    Collapsible::new()
                        .open(self.raw_open)
                        .child(
                            Button::new("approval-raw-arguments")
                                .small()
                                .ghost()
                                .icon(if self.raw_open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .label("完整调用参数")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.raw_open = !view.raw_open;
                                    cx.notify();
                                })),
                        )
                        .content(
                            Editor::new(&self.code)
                                .readonly(true)
                                .h(window.rem_size() * 14.)
                                .aria_label("完整 MCP 调用参数"),
                        ),
                )
                .into_any_element(),
            ApprovalPreview::Capability(capability) => v_flex()
                .gap_3()
                .child(Self::row(
                    "环境",
                    if capability.environment.is_local() {
                        "本机".into()
                    } else {
                        capability.environment.to_string()
                    },
                    narrow,
                    cx,
                ))
                .child(Self::row(
                    "访问路径",
                    orbit_client::directories::display_path(&capability.path.to_string_lossy())
                        .into_owned(),
                    narrow,
                    cx,
                ))
                .child(Self::row(
                    "访问权限",
                    if capability.access.allows_write() {
                        "读取与写入"
                    } else {
                        "读取"
                    },
                    narrow,
                    cx,
                ))
                .child(Self::row("申请原因", capability.reason.clone(), narrow, cx))
                .into_any_element(),
        };
        v_flex()
            .id("approval-content")
            .min_w_0()
            .max_h(crate::dialog::body_height(window))
            .overflow_y_scroll()
            .gap_3()
            .when_some(self.error.clone(), |body, error| {
                body.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(
                            orbit_client::directories::display_diagnostic(error.as_ref())
                                .into_owned(),
                        ),
                )
            })
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::{arguments, patch, short};

    #[::core::prelude::v1::test]
    fn full_patch_preserves_context_and_missing_final_newlines() {
        let before = "unchanged\nold\ntrailing\nlast";
        let after = "unchanged\nnew\ntrailing\nlast\n";
        let source = patch(before, after, false, false);
        assert!(source.contains(" unchanged\n"));
        assert!(source.contains(" trailing\n"));
        assert!(source.contains("\\ No newline at end of file"));
        let files = gpui_kit::component::diff::DiffFile::parse(&source).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].has_changes());
        assert!(
            gpui_kit::component::diff::DiffFile::parse(&patch("", "new\n", true, false)).unwrap()
                [0]
            .original_path()
            .is_none()
        );
        assert!(
            gpui_kit::component::diff::DiffFile::parse(&patch("old\n", "", false, true)).unwrap()
                [0]
            .modified_path()
            .is_none()
        );
    }

    #[::core::prelude::v1::test]
    fn argument_summaries_keep_full_values_and_mark_shortened_data() {
        let source = serde_json::json!({
            "message": "字".repeat(300),
            "options": {
                "active": true
            }
        })
        .to_string();
        let parsed = arguments(&source);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&parsed.raw).unwrap(),
            serde_json::from_str::<serde_json::Value>(&source).unwrap()
        );
        assert!(parsed.note.is_some());
        assert!(
            parsed.fields[0]
                .value
                .ends_with("（已截断）")
        );
        assert_eq!(short("🦀中文", 2), ("🦀中…（已截断）".into(), true));
        let invalid = arguments("not-json\nraw");
        assert_eq!(invalid.raw, "not-json\nraw");
        assert!(invalid.note.is_some());
    }
}
