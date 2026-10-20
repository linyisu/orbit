//! Task evidence stays beside the conversation. All actions use the existing
//! service protocol; this pane has no execution or persistence authority.
use super::*;
use actions::WorkspaceCommand;
use gpui_kit::component::{Selectable, scroll::ScrollableElement, tooltip::Tooltip};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Section {
    Overview,
    Tool,
    File,
    Plan,
    Changes,
    Review,
    ProjectFiles,
    History,
    Agents,
    AgentConversation,
}

impl Section {
    fn label(self) -> &'static str {
        match self {
            Self::Overview => "会话详情",
            Self::Tool => "工具详情",
            Self::File => "文件预览",
            Self::Plan => "计划",
            Self::Changes => "文件变更",
            Self::Review => "变更审查",
            Self::ProjectFiles => "项目文件",
            Self::History => "提交历史",
            Self::Agents => "子 Agent",
            Self::AgentConversation => "子 Agent 对话",
        }
    }
}

/// Layout decisions use the full window after the window owner hides navigation.
pub(crate) fn compact(width_rem: f32) -> bool {
    width_rem < 72.
}

pub(crate) fn hide_navigation(width_rem: f32, details: bool) -> bool {
    details && width_rem < 96.
}

actions!(orbit_inspector, [Dismiss]);
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some("OrbitDetails"))]);
}

impl Workspace {
    #[cfg(feature = "test-support")]
    pub(crate) fn check_details(
        &mut self,
        step: u8,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            !window.has_active_dialog(cx) && !window.has_active_sheet(cx),
            "details must not open a modal"
        );
        match step {
            0 => {
                window.resize(size(px(1_600.), px(900.)));
                self.input.update(cx, |input, cx| {
                    input.set_value("Generated detail-view draft", window, cx)
                });
                let tool = self
                    .execution
                    .tools()
                    .iter()
                    .find(|tool| tool.name == "Exec")
                    .cloned()
                    .ok_or_else(|| {
                        anyhow::anyhow!("detail probe requires generated tool history")
                    })?;
                self.open_tool_record(tool, false, window, cx);
            }
            1 => anyhow::ensure!(
                self.inspector == Some(Section::Tool),
                "tool output must use the detail pane"
            ),
            2 => {
                self.set_inspector(None, window, cx);
                anyhow::ensure!(
                    self.input
                        .focus_handle(cx)
                        .is_focused(window),
                    "closing details must return focus to the composer"
                );
                self.set_inspector(Some(Section::Tool), window, cx);
            }
            3 => {
                window.resize(size(px(1_200.), px(800.)));
                self.show_plan(window, cx);
            }
            4 => anyhow::ensure!(
                self.inspector == Some(Section::Plan),
                "plan must use the detail pane"
            ),
            5 => {
                gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
                gpui_kit::component::Theme::global_mut(cx).font_size = px(20.);
                gpui_kit::component::Theme::sync_base(cx);
                window.resize(size(px(800.), px(650.)));
                self.open_child_agents(window, cx);
            }
            6 => anyhow::ensure!(
                self.inspector == Some(Section::Agents),
                "subtasks must share the detail pane"
            ),
            7 => {
                use base64::Engine;
                let request = crate::file_preview::open(
                    orbit_protocol::MessageImage {
                        name: "Generated attachment.txt".into(),
                        path: "generated-attachment.txt".into(),
                    },
                    window,
                    cx,
                );
                self.file_preview = request.preview();
                let encoded = base64::engine::general_purpose::STANDARD
                    .encode("Generated read-only attachment\n".repeat(80));
                let _ = request
                    .take_response()
                    .unwrap()
                    .send(Ok(Response::File(encoded)));
                self.set_inspector(Some(Section::File), window, cx);
            }
            8 => {
                anyhow::ensure!(
                    self.inspector == Some(Section::File),
                    "attachments must use the detail pane"
                );
                let tool = self
                    .execution
                    .tools()
                    .iter()
                    .find(|tool| {
                        tool.name == "Exec" && tool.status == crate::execution::ToolStatus::Failed
                    })
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("detail probe requires a failed command"))?;
                self.open_tool_record(tool, false, window, cx);
            }
            9 => {
                if let Some(panel) = &self.tool_output {
                    let focus = panel.read(cx).result_focus(cx);
                    focus.focus(window, cx);
                }
            }
            10 => {
                anyhow::ensure!(
                    self.inspector.is_none(),
                    "Escape must close compact details"
                );
                self.show_review(None, window, cx);
            }
            11 => {
                anyhow::ensure!(
                    self.inspector == Some(Section::Review),
                    "diff review must share the detail pane"
                );
                self.set_inspector(None, window, cx);
            }
            12 if std::env::var_os("ORBIT_GUI_PROJECT_PROBE").is_some() => {
                gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Light, None, cx);
                gpui_kit::component::Theme::global_mut(cx).font_size = px(16.);
                gpui_kit::component::Theme::sync_base(cx);
                window.resize(size(px(1_600.), px(900.)));
                self.show_files(None, window, cx);
            }
            13 => self
                .files
                .as_ref()
                .expect("file pane")
                .read(cx)
                .check_project(false, cx)?,
            14 => self.show_files(Some("src/main.rs".into()), window, cx),
            15 => self
                .files
                .as_ref()
                .expect("file pane")
                .read(cx)
                .check_project(true, cx)?,
            16 => self.show_git_history(window, cx),
            17..=19 => {
                self.git_history
                    .as_ref()
                    .expect("history pane")
                    .update(cx, |history, cx| history.check_project(step - 17, cx))?;
            }
            20 => self.set_inspector(None, window, cx),
            _ => {
                anyhow::ensure!(
                    self.inspector.is_none(),
                    "Escape must close compact details"
                );
                anyhow::ensure!(
                    self.input.read(cx).value() == "Generated detail-view draft",
                    "detail navigation changed the draft"
                );
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn inspector_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let section = self
            .inspector
            .unwrap_or(Section::Overview);
        let title = if section == Section::Tool {
            self.tool_output
                .as_ref()
                .map(|panel| panel.read(cx).title())
                .unwrap_or_else(|| section.label().into())
        } else if section == Section::File {
            self.file_preview
                .as_ref()
                .map(|(name, _)| name.clone())
                .unwrap_or_else(|| section.label().into())
        } else {
            section.label().into()
        };
        let content = match section {
            Section::Overview => self
                .details_overview(cx)
                .into_any_element(),
            Section::Tool => div()
                .size_full()
                .p_3()
                .children(self.tool_output.clone())
                .into_any_element(),
            Section::AgentConversation => div()
                .size_full()
                .children(
                    self.subagent
                        .as_ref()
                        .map(|(_, view)| view.clone()),
                )
                .into_any_element(),
            Section::File => div()
                .size_full()
                .p_3()
                .children(
                    self.file_preview
                        .as_ref()
                        .map(|(_, view)| view.clone()),
                )
                .on_action(cx.listener(
                    |view, _: &gpui_kit::component::input::Escape, window, cx| {
                        view.set_inspector(None, window, cx)
                    },
                ))
                .into_any_element(),
            Section::Plan => self.plan_view(cx).into_any_element(),
            Section::Changes => self.changes_view(cx).into_any_element(),
            Section::Review => div()
                .size_full()
                .children(self.review.clone())
                .into_any_element(),
            Section::ProjectFiles => div()
                .size_full()
                .children(self.files.clone())
                .into_any_element(),
            Section::History => div()
                .size_full()
                .children(self.git_history.clone())
                .into_any_element(),
            Section::Agents => div()
                .size_full()
                .children(self.child_agents.clone())
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(cx.theme().background)
            .track_focus(&self.details_focus)
            .key_context("OrbitDetails")
            .on_action(cx.listener(|view, _: &Dismiss, window, cx| {
                view.set_inspector(None, window, cx);
            }))
            .on_action(
                cx.listener(|view, _: &gpui_kit::component::input::Escape, window, cx| {
                    view.set_inspector(None, window, cx);
                }),
            )
            .on_action(
                cx.listener(|view, _: &gpui_kit::base::actions::Cancel, window, cx| {
                    view.set_inspector(None, window, cx);
                }),
            )
            .child(
                h_flex()
                    .h_10()
                    .px_3()
                    .gap_2()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_ellipsis()
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    .child(
                        Button::new("close-inspector")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .accessibility_label("关闭详情，返回对话")
                            .tooltip("关闭详情 · Esc")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.set_inspector(None, window, cx);
                            })),
                    ),
            )
            .child(
                h_flex()
                    .px_3()
                    .py_1()
                    .gap_2()
                    .when(
                        matches!(
                            section,
                            Section::ProjectFiles | Section::Review | Section::History
                        ),
                        |row| {
                            row.children(
                                [
                                    (Section::ProjectFiles, "文件"),
                                    (Section::Review, "变更"),
                                    (Section::History, "历史"),
                                ]
                                .into_iter()
                                .map(|(target, label)| {
                                    Button::new(label)
                                        .ghost()
                                        .small()
                                        .label(label)
                                        .selected(section == target)
                                        .on_click(cx.listener(move |view, _, window, cx| {
                                            match target {
                                                Section::ProjectFiles => {
                                                    view.show_files(None, window, cx)
                                                }
                                                Section::Review => {
                                                    view.show_review(None, window, cx)
                                                }
                                                Section::History => {
                                                    view.show_git_history(window, cx)
                                                }
                                                _ => unreachable!(),
                                            }
                                        }))
                                }),
                            )
                        },
                    )
                    .when(
                        !matches!(
                            section,
                            Section::ProjectFiles | Section::Review | Section::History
                        ),
                        |row| row.hidden(),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(content),
            )
    }

    fn details_overview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .p_3()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("点击对话中的工具查看输出，或查看本会话的相关内容。"),
            )
            .when(self.tool_output.is_some(), |view| {
                view.child(
                    Button::new("details-last-tool")
                        .ghost()
                        .label("返回上次工具详情")
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.set_inspector(Some(Section::Tool), window, cx);
                        })),
                )
            })
            .children(
                [
                    (Section::Plan, self.planning.plan.is_some()),
                    (Section::Changes, !self.execution.files().is_empty()),
                    (Section::Agents, !self.execution.agents().is_empty()),
                    (Section::ProjectFiles, self.info.is_some()),
                    (Section::History, self.info.is_some()),
                ]
                .into_iter()
                .filter(|(_, available)| *available)
                .map(|(section, _)| {
                    Button::new(SharedString::from(format!("details-{section:?}")))
                        .ghost()
                        .label(section.label())
                        .on_click(cx.listener(move |view, _, window, cx| {
                            if section == Section::Agents {
                                view.open_child_agents(window, cx);
                            } else if section == Section::ProjectFiles {
                                view.show_files(None, window, cx);
                            } else if section == Section::History {
                                view.show_git_history(window, cx);
                            } else {
                                if section == Section::Changes {
                                    view.change_tools = None;
                                }
                                view.set_inspector(Some(section), window, cx);
                            }
                        }))
                }),
            )
    }

    pub(super) fn open_tool_record(
        &mut self,
        tool: crate::execution::ToolRun,
        snapshot: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match orbit_client::execution::details::target(&tool) {
            Some(orbit_client::execution::details::Target::Plan(state)) => {
                self.plan_record = Some(*state);
                self.set_inspector(Some(Section::Plan), window, cx);
                return;
            }
            Some(orbit_client::execution::details::Target::Agent(agent)) if !snapshot => {
                if let Some(info) = &self.info {
                    self.command(
                        Command::Request(Request::ReadSubagent {
                            session: info.id.clone(),
                            agent,
                            after: 0,
                        }),
                        cx,
                    );
                }
                return;
            }
            _ => {}
        }
        let Some(info) = &self.info else { return };
        let tool = self
            .execution
            .tools()
            .iter()
            .find(|current| {
                !snapshot
                    && current.id == tool.id
                    && current.name == tool.name
                    && current.arguments == tool.arguments
            })
            .cloned()
            .unwrap_or(tool);
        let existing = self
            .tool_output
            .as_ref()
            .filter(|panel| {
                let panel = panel.read(cx);
                panel.belongs_to(&self.options, &info.id, &info.workspace) && panel.matches(&tool)
            });
        let panel = if let Some(panel) = existing {
            panel.update(cx, |panel, cx| panel.sync(&tool, false, window, cx));
            panel.clone()
        } else {
            let options = self.options.clone();
            let owner = cx.weak_entity();
            let session = info.id.clone();
            let workspace = info.workspace.clone();
            cx.new(|cx| {
                tool_output::ToolOutput::new(owner, options, session, workspace, tool, window, cx)
            })
        };
        if snapshot || !self.connected {
            panel.update(cx, |panel, cx| panel.disconnected(cx));
        }
        self.tool_output = Some(panel.clone());
        self.set_inspector(Some(Section::Tool), window, cx);
        tool_output::present(panel, window, cx);
    }

    pub(super) fn sync_tool_output(
        &mut self,
        restoring: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if restoring {
            return;
        }
        let Some(panel) = self.tool_output.clone() else {
            return;
        };
        let current = self.info.as_ref().is_some_and(|info| {
            panel
                .read(cx)
                .belongs_to(&self.options, &info.id, &info.workspace)
        });
        let tool = current
            .then(|| {
                self.execution
                    .tools()
                    .iter()
                    .find(|tool| panel.read(cx).matches(tool))
                    .cloned()
            })
            .flatten();
        panel.update(cx, |panel, cx| {
            if let Some(tool) = tool {
                panel.sync(&tool, false, window, cx);
            } else {
                panel.disconnected(cx);
            }
        });
    }

    pub(super) fn disconnect_tool_output(&mut self, cx: &mut Context<Self>) {
        if let Some(panel) = &self.tool_output {
            panel.update(cx, |panel, cx| panel.disconnected(cx));
        }
    }

    fn plan_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self
            .plan_record
            .as_ref()
            .unwrap_or(&self.planning);
        let plan = state.plan.as_ref();
        let generation = state.generation;
        let reviewable = generation == self.planning.generation
            && plan == self.planning.plan.as_ref()
            && state.mode == orbit_protocol::plan::WorkMode::Plan
            && plan.is_some_and(|plan| plan.status == orbit_protocol::plan::Status::Review);
        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(
                div()
                    .id("inspector-plan-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(if let Some(plan) = plan {
                        div()
                            .p_4()
                            .child(
                                TextView::markdown("details-plan", plan.markdown.clone())
                                    .style(crate::theme::document_style(cx)),
                            )
                            .into_any_element()
                    } else {
                        empty_state(
                            IconName::ListTodo,
                            "还没有计划",
                            "在输入框切换到计划模式，先分析任务与实现步骤。",
                            cx,
                        )
                        .into_any_element()
                    }),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .when(reviewable, |row| {
                        row.child(
                            Button::new("plan-feedback")
                                .small()
                                .label("修改意见…")
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    if let Some(key) = view.plan_decision() {
                                        view.attention.dismiss(&key);
                                    }
                                    if let Some(info) = &view.info {
                                        let session = info.id.clone();
                                        view.prompt(
                                            ("修改计划", "修改意见"),
                                            "",
                                            "提交意见",
                                            move |text| {
                                                Ok(Request::Plan {
                                                    session: session.clone(),
                                                    action:
                                                        orbit_protocol::plan::Action::Feedback {
                                                            generation,
                                                            text,
                                                        },
                                                })
                                            },
                                            window,
                                            cx,
                                        );
                                    }
                                })),
                        )
                        .child(
                            Button::new("approve-plan")
                                .small()
                                .primary()
                                .label("批准并执行")
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    if let Some(key) = view.plan_decision() {
                                        view.attention.dismiss(&key);
                                    }
                                    view.plan_action(
                                        orbit_protocol::plan::Action::Approve { generation },
                                        cx,
                                    );
                                })),
                        )
                    })
                    .child(
                        Button::new("inspect-task-board")
                            .ghost()
                            .small()
                            .label("任务清单…")
                            .disabled(self.info.is_none())
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.execute(WorkspaceCommand::Tasks, window, cx);
                            })),
                    ),
            )
    }

    fn changes_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let files = self
            .change_tools
            .as_ref()
            .map(|tools| {
                tools
                    .iter()
                    .flat_map(|tool| {
                        tool.changes
                            .iter()
                            .map(|change| change.path.clone())
                    })
                    .collect::<std::collections::BTreeSet<_>>()
            })
            .unwrap_or_else(|| self.execution.files().clone());
        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(section_summary(
                "文件变更",
                format!(
                    "{} 个文件 · {}",
                    files.len(),
                    if self.change_tools.is_some() {
                        "本轮写入快照"
                    } else {
                        "本会话"
                    }
                ),
                cx,
            ))
            .child(
                div()
                    .id("inspector-changes-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(if files.is_empty() {
                        empty_state(
                            IconName::File,
                            "还没有文件变更",
                            "Agent 修改文件后，可以在这里查看差异或继续编辑。",
                            cx,
                        )
                        .into_any_element()
                    } else {
                        v_flex()
                            .px_2()
                            .children(files.iter().map(|path| {
                                let path = path.clone();
                                let edit_path = path.clone();
                                let snapshot = self
                                    .change_tools
                                    .as_ref()
                                    .and_then(|tools| {
                                        tools.iter().rev().find(|tool| {
                                            tool.changes
                                                .iter()
                                                .any(|change| change.path == path)
                                        })
                                    })
                                    .cloned();
                                let (name, parent) = path_labels(&path);
                                let tooltip = preview_tooltip(&path, "点击查看文件差异");
                                h_flex()
                                    .gap_1()
                                    .min_w_0()
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "changed-file:{path}"
                                        )))
                                        .ghost()
                                        .small()
                                        .h_12()
                                        .px_2()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .min_w_0()
                                                .gap_2()
                                                .child(
                                                    Icon::new(IconName::File)
                                                        .small()
                                                        .text_color(cx.theme().muted_foreground),
                                                )
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .gap_1()
                                                        .child(
                                                            div()
                                                                .id(SharedString::from(format!(
                                                                    "changed-file-title:{path}"
                                                                )))
                                                                .text_sm()
                                                                .text_ellipsis()
                                                                .child(name)
                                                                .tooltip(move |window, cx| {
                                                                    evidence_tooltip(
                                                                        tooltip.clone(),
                                                                        window,
                                                                        cx,
                                                                    )
                                                                }),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(
                                                                    cx.theme().muted_foreground,
                                                                )
                                                                .text_ellipsis()
                                                                .child(parent),
                                                        ),
                                                ),
                                        )
                                        .accessibility_label(format!("查看 {path} 的差异"))
                                        .disabled(self.info.is_none())
                                        .on_click(cx.listener(move |view, _, window, cx| {
                                            if let Some(tool) = snapshot.clone() {
                                                view.open_tool_record(tool, true, window, cx);
                                            } else {
                                                view.show_checkpoints(
                                                    Some(path.clone()),
                                                    window,
                                                    cx,
                                                );
                                            }
                                        })),
                                    )
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "edit-changed-file:{edit_path}"
                                        )))
                                        .ghost()
                                        .small()
                                        .flex_shrink_0()
                                        .icon(IconName::Pencil)
                                        .tooltip("编辑文件…")
                                        .accessibility_label(format!("编辑 {edit_path}"))
                                        .disabled(self.info.is_none())
                                        .on_click(cx.listener(move |view, _, window, cx| {
                                            view.show_files(Some(edit_path.clone()), window, cx)
                                        })),
                                    )
                            }))
                            .into_any_element()
                    }),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("inspect-workspace-diff")
                            .ghost()
                            .small()
                            .icon(IconName::File)
                            .label("审查全部变更…")
                            .disabled(self.info.is_none())
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.execute(WorkspaceCommand::Diff, window, cx)
                            })),
                    ),
            )
    }
}

fn section_summary(title: &'static str, detail: String, cx: &App) -> impl IntoElement {
    v_flex()
        .px_3()
        .py_3()
        .gap_1()
        .flex_shrink_0()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(title),
        )
        .child(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(detail),
        )
}

fn empty_state(
    icon: IconName,
    title: &'static str,
    description: &'static str,
    cx: &App,
) -> impl IntoElement {
    v_flex()
        .px_3()
        .py_6()
        .gap_2()
        .child(
            Icon::new(icon)
                .small()
                .text_color(cx.theme().muted_foreground),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .child(title),
        )
        .child(
            div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(description),
        )
}

fn path_labels(path: &str) -> (String, String) {
    let path = path.replace('\\', "/");
    match path.rsplit_once('/') {
        Some((parent, name)) if !name.is_empty() => (
            name.into(),
            if parent.is_empty() {
                "/".into()
            } else {
                parent.into()
            },
        ),
        _ => (path, "项目根目录".into()),
    }
}

fn evidence_tooltip(text: String, window: &mut Window, cx: &mut App) -> AnyView {
    let width = (window.viewport_size().width * 0.25).min(window.rem_size() * 22.);
    Tooltip::element(move |_, _| {
        v_flex()
            .w(width)
            .gap_1()
            .children(text.lines().map(|line| {
                div()
                    .text_ellipsis()
                    .child(line.to_owned())
            }))
    })
    .build(window, cx)
}

fn preview_tooltip(text: &str, action: &str) -> String {
    let preview = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(3)
        .map(|line| {
            line.chars()
                .take(120)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{preview}\n{action}")
}

#[cfg(test)]
mod tests {
    use super::{compact, hide_navigation, path_labels, preview_tooltip};

    #[test]
    fn details_reclaim_navigation_before_replacing_the_conversation() {
        assert!(!hide_navigation(60., false));
        assert!(hide_navigation(60., true));
        assert!(compact(60.));
        assert!(hide_navigation(80., true));
        assert!(!compact(80.));
        assert!(!hide_navigation(96., true));
        assert!(!compact(72.));
    }

    #[test]
    fn file_identity_and_tooltip_survive_long_unicode_and_remote_paths() {
        assert_eq!(
            path_labels("C:\\项目\\src\\测试.rs"),
            ("测试.rs".into(), "C:/项目/src".into())
        );
        assert_eq!(path_labels("/file.rs"), ("file.rs".into(), "/".into()));
        let preview = preview_tooltip(&"检查🦀\n".repeat(200), "点击查看完整参数与输出");
        assert!(preview.ends_with("点击查看完整参数与输出"));
        assert!(preview.chars().count() < 520);
        assert_eq!(preview.lines().count(), 4);
    }
}
