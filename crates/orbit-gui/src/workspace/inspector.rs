//! Task evidence stays beside the conversation. All actions use the existing
//! service protocol; this pane has no execution or persistence authority.
use super::*;
use actions::WorkspaceCommand;
use gpui_kit::component::{
    ElementExt,
    scroll::{ScrollableElement, Scrollbar},
    tab::{Tab, TabBar},
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

impl Workspace {
    pub(super) fn task_tabs(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self
            .open_tasks
            .iter()
            .position(|task| task.key == self.key)
            .unwrap_or(0);
        let tabs = TabBar::new("open-tasks")
            .w_full()
            .segmented()
            .track_scroll(&self.task_scroll)
            .max_width(window.rem_size() * 14.)
            .menu(true)
            .selected_index(active)
            .children(self.open_tasks.iter().map(|task| {
                let key = task.key.clone();
                let title = task.title.clone();
                Tab::new()
                    .px_2()
                    .label(task.title.clone())
                    .prefix(Icon::new(IconName::BookOpen).size_4())
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(title.clone()).build(window, cx)
                    })
                    .when(task.attention, |tab| {
                        tab.prefix(Icon::new(IconName::CircleAlert).size_3())
                    })
                    .when(task.loading && !task.attention, |tab| {
                        tab.prefix(gpui_kit::component::spinner::Spinner::new().small())
                    })
                    .when(task.unread && !task.attention && !task.loading, |tab| {
                        tab.prefix(div().size_1p5().rounded_full().bg(cx.theme().primary))
                    })
                    .suffix(
                        Button::new(SharedString::from(format!("close-task-{key}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .accessibility_label(format!("关闭 {}", task.title))
                            .tooltip(if task.busy {
                                "任务仍在运行，请先停止任务"
                            } else {
                                "关闭标签页，保留会话记录"
                            })
                            .disabled(task.busy)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.stop_propagation();
                                cx.emit(WorkspaceEvent::Close(key.clone()));
                            })),
                    )
            }))
            .on_click(cx.listener(|view, ix: &usize, _, cx| {
                if let Some(task) = view.open_tasks.get(*ix) {
                    cx.emit(WorkspaceEvent::Activate(task.key.clone()));
                }
            }));
        let owner = cx.weak_entity();
        div()
            .w_full()
            .child(tabs)
            .on_prepaint(move |bounds, _, cx| {
                let _ = owner.update(cx, |view, cx| {
                    if view.task_reveal || view.task_width != Some(bounds.size.width) {
                        view.task_reveal = false;
                        view.task_width = Some(bounds.size.width);
                        let before = view.task_scroll.offset();
                        view.reveal_task();
                        if view.task_scroll.offset() != before {
                            cx.notify();
                        }
                    }
                });
            })
    }

    pub(super) fn inspector_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let section = self.inspector.unwrap_or(Section::Execution);
        let content = match section {
            Section::Execution => self.execution_view(cx).into_any_element(),
            Section::Plan => self.plan_view(cx).into_any_element(),
            Section::Changes => self.changes_view(cx).into_any_element(),
            Section::Agents => self.agents_view(cx).into_any_element(),
        };
        v_flex()
            .size_full()
            .min_w_0()
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .h_9()
                    .px_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div().flex_1().min_w_0().child(
                            TabBar::new("task-inspector")
                                .underline()
                                .small()
                                .selected_index(
                                    Section::ALL.iter().position(|s| *s == section).unwrap_or(0),
                                )
                                .children(Section::ALL.iter().map(|s| Tab::new().label(s.label())))
                                .on_click(cx.listener(|view, ix: &usize, _, cx| {
                                    view.inspector = Section::ALL.get(*ix).copied();
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        Button::new("close-inspector")
                            .ghost()
                            .small()
                            .icon(IconName::Close)
                            .accessibility_label("收起工作面板")
                            .tooltip("收起工作面板")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.inspector = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(if section == Section::Execution {
                div().flex_1().min_h_0().child(content).into_any_element()
            } else {
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(content)
                    .into_any_element()
            })
    }

    fn execution_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        v_flex()
            .size_full()
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
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
            )
            .when(self.execution.tools().is_empty(), |view| {
                view.child(hint("工具调用会随任务执行显示在这里。", cx))
            })
            .when(!self.execution.tools().is_empty(), |view| {
                view.child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .child(
                            uniform_list(
                                "execution-history",
                                self.execution.tools().len(),
                                move |range, _, cx| {
                                    owner
                                        .update(cx, |view, cx| {
                                            range
                                                .map(|ix| view.tool_row(ix, cx))
                                                .collect::<Vec<_>>()
                                        })
                                        .unwrap_or_default()
                                },
                            )
                            .size_full()
                            .track_scroll(&self.execution_scroll),
                        )
                        .child(Scrollbar::vertical(&self.execution_scroll)),
                )
            })
    }

    fn tool_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let tool = &self.execution.tools()[ix];
        let title = tool.name.clone();
        let id = tool.id.clone();
        Button::new(SharedString::from(format!("tool-evidence:{}", tool.id)))
            .ghost()
            .w_full()
            .h_16()
            .py_2()
            .disabled(tool.output.is_none())
            .accessibility_label(format!("{} · {}", tool.name, tool.status.label()))
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
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
                                    .child(tool.name.clone()),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(tool.status.label()),
                            ),
                    )
                    .when(!tool.detail.is_empty(), |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .text_ellipsis()
                                .child(tool.detail.clone()),
                        )
                    }),
            )
            .on_click(cx.listener(move |view, _, window, cx| {
                if let Some(output) = view
                    .execution
                    .tools()
                    .iter()
                    .find(|tool| tool.id == id)
                    .and_then(|tool| tool.output.as_ref())
                {
                    let text = format!("````text\n{output}\n````");
                    view.show_report(title.clone(), text, window, cx);
                }
            }))
            .into_any_element()
    }

    fn plan_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .p_4()
            .gap_3()
            .when_some(self.planning.plan.as_ref(), |view, plan| {
                view.child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .child(plan.title.clone()),
                )
                .child(hint(
                    match plan.status {
                        orbit_protocol::plan::Status::Draft => "草案",
                        orbit_protocol::plan::Status::Review => "等待你确认",
                        orbit_protocol::plan::Status::Approved => "已批准",
                    },
                    cx,
                ))
                .children(plan.steps.iter().enumerate().map(|(ix, step)| {
                    h_flex()
                        .items_start()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{}.", ix + 1)),
                        )
                        .child(div().flex_1().min_w_0().text_sm().child(step.clone()))
                }))
                .child(
                    Button::new("inspect-plan")
                        .small()
                        .label(if plan.status == orbit_protocol::plan::Status::Review {
                            "审阅计划…"
                        } else {
                            "查看完整计划…"
                        })
                        .on_click(cx.listener(|view, _, window, cx| view.show_plan(window, cx))),
                )
            })
            .when(self.planning.plan.is_none(), |view| {
                view.child(hint("还没有计划。使用规划模式，让 Agent 先分析任务。", cx))
            })
            .child(
                Button::new("inspect-task-board")
                    .ghost()
                    .small()
                    .label("任务清单")
                    .disabled(self.info.is_none())
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.execute(WorkspaceCommand::Tasks, window, cx)
                    })),
            )
    }

    fn changes_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .child(format!("{} 个文件", self.execution.files().len())),
                    )
                    .child(
                        Button::new("inspect-workspace-diff")
                            .ghost()
                            .small()
                            .label("查看差异")
                            .disabled(self.info.is_none())
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.execute(WorkspaceCommand::Diff, window, cx)
                            })),
                    ),
            )
            .when(self.execution.files().is_empty(), |view| {
                view.child(hint("当前会话还没有记录到文件修改。", cx))
            })
            .children(self.execution.files().iter().map(|path| {
                let path = path.clone();
                Button::new(SharedString::from(format!("changed-file:{path}")))
                    .ghost()
                    .small()
                    .w_full()
                    .child(div().w_full().text_ellipsis().child(path.clone()))
                    .tooltip(path.clone())
                    .on_click(cx.listener(move |view, _, _, cx| {
                        if let Some(info) = &view.info {
                            view.command(
                                Command::Request(Request::SessionAction {
                                    session: info.id.clone(),
                                    action: orbit_protocol::SessionAction::Inspect {
                                        section: orbit_protocol::Inspection::Diff {
                                            path: Some(path.clone()),
                                        },
                                    },
                                }),
                                cx,
                            );
                        }
                    }))
            }))
            .child(
                Button::new("inspect-session-changes")
                    .ghost()
                    .small()
                    .label("查看变更记录")
                    .disabled(self.info.is_none())
                    .on_click(cx.listener(|view, _, window, cx| {
                        view.execute(WorkspaceCommand::Changes, window, cx)
                    })),
            )
    }

    fn agents_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .p_4()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("协作 Agent"),
            )
            .when(self.execution.agents().is_empty(), |view| {
                view.child(hint("任务委派后，子 Agent 会出现在这里。", cx))
            })
            .children(self.execution.agents().iter().map(|agent| {
                let id = agent.id.clone();
                let recipient = id.clone();
                h_flex()
                    .gap_2()
                    .child(
                        Button::new(SharedString::from(format!("inspect-agent:{id}")))
                            .ghost()
                            .h_auto()
                            .flex_1()
                            .min_w_0()
                            .py_2()
                            .child(
                                v_flex()
                                    .w_full()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .child(format!("{} · {}", agent.role, agent.status)),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground)
                                            .text_ellipsis()
                                            .child(id.clone()),
                                    ),
                            )
                            .on_click(cx.listener(move |view, _, _, cx| {
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
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("message-agent:{recipient}")))
                            .ghost()
                            .small()
                            .label("发消息…")
                            .on_click(cx.listener(move |view, _, window, cx| {
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
                            })),
                    )
            }))
    }
}

fn hint(text: &str, cx: &App) -> impl IntoElement {
    div()
        .text_sm()
        .text_color(cx.theme().muted_foreground)
        .child(text.to_owned())
}
