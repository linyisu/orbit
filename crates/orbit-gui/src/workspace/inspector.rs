//! Task evidence stays beside the conversation. All actions use the existing
//! service protocol; this pane has no execution or persistence authority.
use super::*;
use crate::execution::{Phase, ToolStatus};
use actions::WorkspaceCommand;
use gpui_kit::component::{
    Selectable,
    scroll::{ScrollableElement, Scrollbar},
    spinner::Spinner,
    tab::{Tab, TabBar},
    tooltip::Tooltip,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Section {
    Execution,
    Plan,
    Changes,
    Agents,
}

impl Section {
    const ALL: [Self; 4] = [Self::Execution, Self::Plan, Self::Changes, Self::Agents];
    fn label(self) -> &'static str {
        match self {
            Self::Execution => "执行",
            Self::Plan => "计划",
            Self::Changes => "变更",
            Self::Agents => "Agent",
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Filter {
    #[default]
    All,
    Active,
    Failed,
}

impl Filter {
    fn accepts(self, status: ToolStatus) -> bool {
        match self {
            Self::All => true,
            Self::Active => matches!(status, ToolStatus::Running | ToolStatus::Background),
            Self::Failed => status == ToolStatus::Failed,
        }
    }
}

pub(super) struct State {
    filter: Filter,
    selected: Option<u64>,
    pub(super) focus: FocusHandle,
}

impl State {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            filter: Filter::All,
            selected: None,
            focus: cx.focus_handle(),
        }
    }

    pub(super) fn reset(&mut self) {
        self.filter = Filter::All;
        self.selected = None;
    }
}

actions!(orbit_inspector, [Previous, Next, First, Last, Open]);
pub(super) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", Previous, Some("OrbitInspector")),
        KeyBinding::new("down", Next, Some("OrbitInspector")),
        KeyBinding::new("home", First, Some("OrbitInspector")),
        KeyBinding::new("end", Last, Some("OrbitInspector")),
        KeyBinding::new("enter", Open, Some("OrbitInspector")),
    ]);
}

impl Workspace {
    pub(super) fn inspector_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let section = self
            .inspector
            .unwrap_or(Section::Execution);
        let content = match section {
            Section::Execution => self
                .execution_view(cx)
                .into_any_element(),
            Section::Plan => self.plan_view(cx).into_any_element(),
            Section::Changes => self.changes_view(cx).into_any_element(),
            Section::Agents => self.agents_view(cx).into_any_element(),
        };
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(cx.theme().background)
            .child(
                v_flex()
                    .flex_shrink_0()
                    .px_3()
                    .pb_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        h_flex()
                            .h_10()
                            .gap_2()
                            .child(
                                Icon::new(IconName::PanelRight)
                                    .small()
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("工作面板"),
                            )
                            .child(
                                Button::new("close-inspector")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Close)
                                    .accessibility_label("收起工作面板")
                                    .tooltip("收起工作面板")
                                    .on_click(cx.listener(|view, _, window, cx| {
                                        view.set_inspector(None, window, cx)
                                    })),
                            ),
                    )
                    .child(
                        TabBar::new("task-inspector")
                            .segmented()
                            .small()
                            .w_full()
                            .selected_index(
                                Section::ALL
                                    .iter()
                                    .position(|s| *s == section)
                                    .unwrap_or(0),
                            )
                            .children(
                                Section::ALL
                                    .iter()
                                    .map(|s| Tab::new().label(s.label()).flex_1()),
                            )
                            .on_click(cx.listener(|view, ix: &usize, window, cx| {
                                view.set_inspector(Section::ALL.get(*ix).copied(), window, cx);
                            })),
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

    fn visible_tools(&self) -> Vec<usize> {
        self.execution
            .tools()
            .iter()
            .enumerate()
            .filter_map(|(ix, tool)| {
                self.inspector_state
                    .filter
                    .accepts(tool.status)
                    .then_some(ix)
            })
            .collect()
    }

    fn select_tool(
        &mut self,
        direction: i8,
        edge: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let visible = self.visible_tools();
        let current = visible.iter().position(|ix| {
            Some(self.execution.tools()[*ix].generation) == self.inspector_state.selected
        });
        let Some(next) = selection(visible.len(), current, direction, edge) else {
            return;
        };
        self.inspector_state.selected = Some(self.execution.tools()[visible[next]].generation);
        self.execution_scroll
            .scroll_to_item(next, ScrollStrategy::Center);
        self.inspector_state
            .focus
            .focus(window, cx);
        cx.notify();
    }

    fn open_selected_tool(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self
            .visible_tools()
            .into_iter()
            .find(|ix| {
                Some(self.execution.tools()[*ix].generation) == self.inspector_state.selected
            });
        if let Some(ix) = selected {
            self.open_tool_output(self.execution.tools()[ix].id.clone(), window, cx);
        }
    }

    fn execution_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let tools = self.execution.tools();
        let visible = self.visible_tools();
        let count = visible.len();
        let failed = tools
            .iter()
            .filter(|t| t.status == ToolStatus::Failed)
            .count();
        let active = tools
            .iter()
            .filter(|t| Filter::Active.accepts(t.status))
            .count();
        let phase_color =
            if !self.connected || self.error.is_some() || self.execution.phase() == Phase::Failed {
                cx.theme().danger
            } else if !self.approvals.is_empty() {
                cx.theme().warning
            } else {
                cx.theme().muted_foreground
            };
        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(
                v_flex()
                    .px_3()
                    .py_3()
                    .gap_1()
                    .flex_shrink_0()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(if self.busy && self.connected {
                                Spinner::new()
                                    .small()
                                    .color(cx.theme().primary)
                                    .into_any_element()
                            } else {
                                Icon::new(if self.execution.phase() == Phase::Complete {
                                    IconName::Check
                                } else if !self.connected
                                    || self.error.is_some()
                                    || self.execution.phase() == Phase::Failed
                                {
                                    IconName::CircleAlert
                                } else {
                                    IconName::SquareTerminal
                                })
                                .small()
                                .text_color(phase_color)
                                .into_any_element()
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(self.status()),
                            )
                            .when(self.execution.turns() > 0, |row| {
                                row.child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("第 {} 轮", self.execution.turns())),
                                )
                            }),
                    ),
            )
            .when(!tools.is_empty(), |view| {
                view.child(
                    h_flex()
                        .px_3()
                        .pb_2()
                        .gap_1()
                        .flex_shrink_0()
                        .children(
                            [
                                (Filter::All, "全部", tools.len()),
                                (Filter::Active, "进行中", active),
                                (Filter::Failed, "失败", failed),
                            ]
                            .into_iter()
                            .map(|(filter, label, count)| {
                                Button::new(("execution-filter", filter as usize))
                                    .ghost()
                                    .xsmall()
                                    .selected(self.inspector_state.filter == filter)
                                    .label(format!("{label} {count}"))
                                    .on_click(cx.listener(move |view, _, window, cx| {
                                        view.inspector_state.filter = filter;
                                        // A new filter starts at the top, without a hidden
                                        // selection.
                                        view.inspector_state.selected = None;
                                        view.execution_scroll = UniformListScrollHandle::new();
                                        view.inspector_state
                                            .focus
                                            .focus(window, cx);
                                        cx.notify();
                                    }))
                            }),
                        ),
                )
            })
            .child(
                div()
                    .id("inspector-execution-body")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .track_focus(&self.inspector_state.focus)
                    .key_context("OrbitInspector")
                    .on_action(cx.listener(|view, _: &Previous, window, cx| {
                        view.select_tool(-1, false, window, cx)
                    }))
                    .on_action(cx.listener(|view, _: &Next, window, cx| {
                        view.select_tool(1, false, window, cx)
                    }))
                    .on_action(cx.listener(|view, _: &First, window, cx| {
                        view.select_tool(-1, true, window, cx)
                    }))
                    .on_action(cx.listener(|view, _: &Last, window, cx| {
                        view.select_tool(1, true, window, cx)
                    }))
                    .on_action(
                        cx.listener(|view, _: &Open, window, cx| {
                            view.open_selected_tool(window, cx)
                        }),
                    )
                    .child(if count == 0 {
                        empty_state(
                            IconName::SquareTerminal,
                            if tools.is_empty() {
                                "还没有工具调用"
                            } else {
                                "没有匹配的调用"
                            },
                            if tools.is_empty() {
                                "执行命令、搜索或修改文件时，记录会显示在这里。"
                            } else {
                                "切换到“全部”查看本轮执行记录。"
                            },
                            cx,
                        )
                        .into_any_element()
                    } else {
                        div()
                            .size_full()
                            .child(
                                uniform_list("execution-history", count, move |range, _, cx| {
                                    owner
                                        .update(cx, |view, cx| {
                                            range
                                                .filter_map(|ix| {
                                                    visible
                                                        .get(ix)
                                                        .map(|source| view.tool_row(*source, cx))
                                                })
                                                .collect::<Vec<_>>()
                                        })
                                        .unwrap_or_default()
                                })
                                .size_full()
                                .track_scroll(&self.execution_scroll),
                            )
                            .child(Scrollbar::vertical(&self.execution_scroll))
                            .into_any_element()
                    }),
            )
            .child(
                h_flex()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .flex_shrink_0()
                    .child(
                        Button::new("open-input-queue-inspector")
                            .small()
                            .ghost()
                            .icon(IconName::ListTodo)
                            .label("输入队列…")
                            .disabled(self.info.is_none())
                            .on_click(
                                cx.listener(|view, _, window, cx| {
                                    view.open_input_queue(window, cx)
                                }),
                            ),
                    ),
            )
    }

    fn tool_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let tool = &self.execution.tools()[ix];
        let presentation = tool.presentation();
        let id = tool.id.clone();
        let generation = tool.generation;
        let (status, color) = tool_status(tool.status, cx);
        let tooltip = preview_tooltip(presentation.tooltip(), "点击查看完整参数与输出");
        div()
            .id(SharedString::from(format!("tool-preview:{generation}")))
            .px_2()
            .tooltip(move |window, cx| evidence_tooltip(tooltip.clone(), window, cx))
            .child(
                Button::new(SharedString::from(format!(
                    "tool-evidence:{}",
                    tool.generation
                )))
                .ghost()
                .small()
                .w_full()
                .h_12()
                .px_2()
                .py_1()
                .selected(self.inspector_state.selected == Some(generation))
                .when(tool.status == ToolStatus::Failed, |row| {
                    row.bg(cx.theme().danger.opacity(0.05))
                })
                .accessibility_label(format!(
                    "{} · {status} · {}",
                    presentation.title(),
                    presentation.subtitle()
                ))
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .child(
                            div().w_4().flex_shrink_0().child(
                                Icon::new(tool_icon(&tool.name))
                                    .small()
                                    .text_color(cx.theme().muted_foreground),
                            ),
                        )
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .text_ellipsis()
                                        .when(tool.name == "run_command", |text| {
                                            text.font_family(cx.theme().mono_font_family.clone())
                                        })
                                        .child(presentation.title().to_owned()),
                                )
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .text_ellipsis()
                                                .child(presentation.subtitle().to_owned()),
                                        )
                                        .when_some(tool.elapsed_ms, |row, elapsed| {
                                            row.child(
                                                div()
                                                    .flex_shrink_0()
                                                    .child(duration(elapsed)),
                                            )
                                        }),
                                ),
                        )
                        .child(
                            v_flex()
                                .w_12()
                                .flex_shrink_0()
                                .items_end()
                                .gap_1()
                                .child(
                                    h_flex()
                                        .gap_1()
                                        .text_xs()
                                        .text_color(color)
                                        .child(status_icon(tool.status, cx))
                                        .child(status),
                                )
                                .child(
                                    Icon::new(IconName::ChevronRight)
                                        .xsmall()
                                        .text_color(cx.theme().muted_foreground),
                                ),
                        ),
                )
                .on_click(cx.listener(move |view, _, window, cx| {
                    view.inspector_state.selected = Some(generation);
                    view.open_tool_output(id.clone(), window, cx);
                })),
            )
            .into_any_element()
    }

    pub(super) fn open_tool_output(
        &mut self,
        call_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = &self.info else { return };
        let Some(tool) = self
            .execution
            .tools()
            .iter()
            .find(|tool| tool.id == call_id)
            .cloned()
        else {
            return;
        };
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
        if !self.connected {
            panel.update(cx, |panel, cx| panel.disconnected(cx));
        }
        self.tool_output = Some(panel.clone());
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
        let plan = self.planning.plan.as_ref();
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
                        v_flex()
                            .px_3()
                            .py_3()
                            .gap_3()
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(plan.title.clone()),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(
                                                Icon::new(
                                                    if plan.status
                                                        == orbit_protocol::plan::Status::Approved
                                                    {
                                                        IconName::Check
                                                    } else {
                                                        IconName::ListTodo
                                                    },
                                                )
                                                .small(),
                                            )
                                            .child(match plan.status {
                                                orbit_protocol::plan::Status::Draft => "草案",
                                                orbit_protocol::plan::Status::Review => "等待确认",
                                                orbit_protocol::plan::Status::Approved => "已批准",
                                            })
                                            .child(format!("· {} 个步骤", plan.steps.len())),
                                    ),
                            )
                            .child(
                                v_flex().children(plan.steps.iter().enumerate().map(
                                    |(ix, step)| {
                                        h_flex()
                                            .items_start()
                                            .gap_2()
                                            .py_2()
                                            .when(ix + 1 < plan.steps.len(), |row| {
                                                row.border_b_1()
                                                    .border_color(cx.theme().border)
                                            })
                                            .child(
                                                div()
                                                    .w_5()
                                                    .flex_shrink_0()
                                                    .text_sm()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(format!("{:02}", ix + 1)),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .text_sm()
                                                    .child(step.clone()),
                                            )
                                    },
                                )),
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
                    .when_some(plan, |row, plan| {
                        row.child(
                            Button::new("inspect-plan")
                                .small()
                                .label(if plan.status == orbit_protocol::plan::Status::Review {
                                    "审阅计划…"
                                } else {
                                    "完整计划…"
                                })
                                .on_click(
                                    cx.listener(|view, _, window, cx| view.show_plan(window, cx)),
                                ),
                        )
                    })
                    .child(
                        Button::new("inspect-task-board")
                            .ghost()
                            .small()
                            .label("任务清单…")
                            .disabled(self.info.is_none())
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.execute(WorkspaceCommand::Tasks, window, cx)
                            })),
                    ),
            )
    }

    fn changes_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(section_summary(
                "文件变更",
                format!("{} 个文件 · 本会话", self.execution.files().len()),
                cx,
            ))
            .child(
                div()
                    .id("inspector-changes-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(if self.execution.files().is_empty() {
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
                            .children(
                                self.execution
                                    .files()
                                    .iter()
                                    .map(|path| {
                                        let path = path.clone();
                                        let edit_path = path.clone();
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
                                        .on_click(
                                            cx.listener(move |view, _, window, cx| {
                                                view.show_review(Some(path.clone()), window, cx)
                                            }),
                                        ),
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
                                        .on_click(
                                            cx.listener(move |view, _, window, cx| {
                                                view.show_files(Some(edit_path.clone()), window, cx)
                                            }),
                                        ),
                                    )
                                    }),
                            )
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

    fn agents_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(section_summary(
                "协作 Agent",
                format!("{} 个子任务", self.execution.agents().len()),
                cx,
            ))
            .child(
                div()
                    .id("inspector-agents-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(if self.execution.agents().is_empty() {
                        empty_state(
                            IconName::User,
                            "还没有委派任务",
                            "Agent 分配子任务后，可以在这里查看进展和发送消息。",
                            cx,
                        )
                        .into_any_element()
                    } else {
                        v_flex()
                            .px_2()
                            .children(
                                self.execution
                                    .agents()
                                    .iter()
                                    .map(|agent| {
                                        let id = agent.id.clone();
                                        let recipient = id.clone();
                                        let title = agent
                                            .task_summary()
                                            .unwrap_or_else(|| agent.role.clone());
                                        let detail = format!("{} · {id}", agent.role);
                                        let tooltip = preview_tooltip(
                                            agent.task().unwrap_or(&id),
                                            "点击查看子任务",
                                        );
                                        let state = match agent.status.as_str() {
                                            "运行中" => ToolStatus::Running,
                                            "已完成" => ToolStatus::Complete,
                                            "失败" => ToolStatus::Failed,
                                            "已停止" => ToolStatus::Stopped,
                                            _ => ToolStatus::Background,
                                        };
                                        h_flex()
                                    .gap_1()
                                    .min_w_0()
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "inspect-agent:{id}"
                                        )))
                                        .ghost()
                                        .small()
                                        .h_12()
                                        .px_2()
                                        .flex_1()
                                        .min_w_0()
                                        .accessibility_label(format!(
                                            "查看 {title} · {} · {} · {id}",
                                            agent.role, agent.status,
                                        ))
                                        .disabled(self.info.is_none())
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .gap_2()
                                                .min_w_0()
                                                .child(
                                                    Icon::new(IconName::User)
                                                        .small()
                                                        .text_color(cx.theme().muted_foreground),
                                                )
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .gap_1()
                                                        .child(
                                                            h_flex()
                                                                .gap_2()
                                                                .child(
                                                                    div()
                                                                        .id(SharedString::from(
                                                                            format!(
                                                                                "agent-title:{id}"
                                                                            ),
                                                                        ))
                                                                        .flex_1()
                                                                        .min_w_0()
                                                                        .text_sm()
                                                                        .text_ellipsis()
                                                                        .child(title)
                                                                        .tooltip(
                                                                            move |window, cx| {
                                                                                evidence_tooltip(
                                                                                    tooltip.clone(),
                                                                                    window,
                                                                                    cx,
                                                                                )
                                                                            },
                                                                        ),
                                                                )
                                                                .child(
                                                                    div()
                                                                        .text_xs()
                                                                        .flex_shrink_0()
                                                                        .text_color(
                                                                            tool_status(state, cx)
                                                                                .1,
                                                                        )
                                                                        .child(
                                                                            agent.status.clone(),
                                                                        ),
                                                                ),
                                                        )
                                                        .child(
                                                            div()
                                                                .text_xs()
                                                                .text_color(
                                                                    cx.theme().muted_foreground,
                                                                )
                                                                .text_ellipsis()
                                                                .child(detail),
                                                        ),
                                                ),
                                        )
                                        .on_click(
                                            cx.listener(move |view, _, _, cx| {
                                                if let Some(info) = &view.info {
                                                    view.command(
                                                        Command::Request(Request::ReadSubagent {
                                                            session: info.id.clone(),
                                                            agent: id.clone(),
                                                            after: 0,
                                                        }),
                                                        cx,
                                                    );
                                                }
                                            }),
                                        ),
                                    )
                                    .child(
                                        Button::new(SharedString::from(format!(
                                            "message-agent:{recipient}"
                                        )))
                                        .ghost()
                                        .small()
                                        .flex_shrink_0()
                                        .icon(IconName::MessageCircle)
                                        .tooltip("发消息…")
                                        .accessibility_label(format!("发消息给 {recipient}"))
                                        .disabled(self.info.is_none() || !self.connected)
                                        .on_click(
                                            cx.listener(move |view, _, window, cx| {
                                                if let Some(info) = &view.info {
                                                    let session = info.id.clone();
                                                    let agent = recipient.clone();
                                                    view.prompt(
                                                        (&format!("发给 {recipient}"), "消息"),
                                                        "",
                                                        "发送",
                                                        move |message| {
                                                            Ok(Request::SendSubagent {
                                                                session: session.clone(),
                                                                agent: agent.clone(),
                                                                message,
                                                            })
                                                        },
                                                        window,
                                                        cx,
                                                    );
                                                }
                                            }),
                                        ),
                                    )
                                    }),
                            )
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
                        Button::new("browse-child-agents")
                            .ghost()
                            .small()
                            .icon(IconName::User)
                            .label("全部子 Agent…")
                            .disabled(self.info.is_none())
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.open_child_agents(window, cx)
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

fn tool_icon(name: &str) -> IconName {
    match name {
        "run_command" => IconName::SquareTerminal,
        "read_file" | "write_file" | "edit_file" | "apply_patch" | "delete_file" | "read_image"
        | "read_document" => IconName::File,
        "search_text" | "search_files" | "web_search" | "web_fetch" => IconName::Search,
        "list_files" | "list_directory" | "project_overview" => IconName::Folder,
        "spawn_agent" | "send_agent_message" | "read_agent" | "wait_agents" | "cancel_agents" => {
            IconName::User
        }
        _ => IconName::Wrench,
    }
}

fn tool_status(status: ToolStatus, cx: &App) -> (&'static str, Hsla) {
    match status {
        ToolStatus::Running => ("运行", cx.theme().primary),
        ToolStatus::Background => ("后台", cx.theme().muted_foreground),
        ToolStatus::Complete => ("完成", cx.theme().muted_foreground),
        ToolStatus::Failed => ("失败", cx.theme().danger),
        ToolStatus::Stopped => ("停止", cx.theme().muted_foreground),
    }
}

fn status_icon(status: ToolStatus, cx: &App) -> AnyElement {
    if status == ToolStatus::Running {
        Spinner::new()
            .xsmall()
            .color(cx.theme().primary)
            .into_any_element()
    } else {
        Icon::new(match status {
            ToolStatus::Complete => IconName::Check,
            ToolStatus::Failed => IconName::CircleAlert,
            ToolStatus::Stopped => IconName::Square,
            _ => IconName::Clock,
        })
        .xsmall()
        .into_any_element()
    }
}

fn duration(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.)
    } else {
        format!("{}m {:02}s", ms / 60_000, ms % 60_000 / 1_000)
    }
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

fn selection(count: usize, current: Option<usize>, direction: i8, edge: bool) -> Option<usize> {
    if count == 0 {
        return None;
    }
    if edge {
        return Some(if direction < 0 { 0 } else { count - 1 });
    }
    Some(match current {
        Some(ix) if direction < 0 => ix.saturating_sub(1).min(count - 1),
        Some(ix) => ix.saturating_add(1).min(count - 1),
        None => {
            if direction < 0 {
                count - 1
            } else {
                0
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{Filter, ToolStatus, path_labels, preview_tooltip, selection};
    #[test]
    fn filtering_and_navigation_preserve_real_states_and_visible_bounds() {
        assert!(Filter::Active.accepts(ToolStatus::Background));
        assert!(Filter::Active.accepts(ToolStatus::Running));
        assert!(!Filter::Active.accepts(ToolStatus::Complete));
        assert!(!Filter::Failed.accepts(ToolStatus::Stopped));
        assert!(Filter::Failed.accepts(ToolStatus::Failed));
        assert_eq!(selection(0, None, 1, false), None);
        assert_eq!(selection(8, None, 1, false), Some(0));
        assert_eq!(selection(8, None, -1, false), Some(7));
        assert_eq!(selection(8, Some(7), 1, false), Some(7));
        assert_eq!(selection(8, Some(0), -1, false), Some(0));
        assert_eq!(selection(8, Some(1), 1, true), Some(7));
        assert_eq!(selection(8, Some(7), -1, true), Some(0));
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
