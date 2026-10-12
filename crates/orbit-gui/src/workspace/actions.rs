//! User-facing workspace commands share one dispatch path across menus and
//! slash commands. Reports belong to a document pane, decisions to dialogs.
use super::*;
use gpui_kit::component::{
    input::{Input, InputState},
    notification::Notification,
};
use orbit_agent::events::AgentEvent;
use orbit_protocol::{
    Inspection, SessionAction, plan,
    project::{MemoryAction, MemoryCategory, TaskAction},
};

#[derive(Clone, Copy, Debug)]
pub(crate) enum WorkspaceCommand {
    OpenProject,
    RemoteProject,
    Changes,
    Diff,
    Status,
    Context,
    Skills,
    Mcp,
    Diagnostics,
    Compact,
    Rename,
    Directory,
    File,
    Export,
    Undo,
    Tasks,
    Memory,
    Plugins,
    Plan,
    Questions,
    Ask,
    Subagent,
    RefreshSkills,
    Accounts,
    RefreshModels,
    AddTask,
    TaskStatus,
    AddMemory,
    ForgetMemory,
    LoadSkill,
    QuestionHistory,
    InputQueue,
    SideQuestions,
    FollowQuestion,
    CancelQuestion,
    SendSubagent,
    ReleaseTask,
    SearchMemory,
    Projects,
    LinkProject,
    Storage,
    History,
    Compactions,
    DeleteSession,
}
impl WorkspaceCommand {
    pub(super) const ALL: &'static [(Self, &'static str)] = &[
        (Self::OpenProject, "打开项目…"),
        (Self::RemoteProject, "打开远程项目…"),
        (Self::Changes, "文件变更"),
        (Self::Diff, "查看差异"),
        (Self::File, "打开文件…"),
        (Self::Export, "导出补丁"),
        (Self::Undo, "撤销文件修改…"),
        (Self::Plan, "计划审阅…"),
        (Self::Questions, "Agent 问答…"),
        (Self::Ask, "旁路提问…"),
        (Self::Subagent, "子 Agent…"),
        (Self::Tasks, "项目任务"),
        (Self::AddTask, "添加任务…"),
        (Self::TaskStatus, "更新任务状态…"),
        (Self::Memory, "会话记忆"),
        (Self::AddMemory, "保存记忆…"),
        (Self::ForgetMemory, "停用记忆…"),
        (Self::Skills, "技能"),
        (Self::LoadSkill, "加载技能…"),
        (Self::Plugins, "插件…"),
        (Self::Mcp, "MCP 服务"),
        (Self::Context, "上下文"),
        (Self::Compact, "压缩上下文"),
        (Self::Status, "会话状态"),
        (Self::Diagnostics, "诊断"),
        (Self::Rename, "重命名会话…"),
        (Self::Directory, "切换目录…"),
        (Self::RefreshSkills, "刷新技能"),
        (Self::Accounts, "账号与模型连接…"),
        (Self::RefreshModels, "刷新模型"),
        (Self::QuestionHistory, "Agent 问答历史"),
        (Self::InputQueue, "待处理输入"),
        (Self::SideQuestions, "旁路问答历史"),
        (Self::FollowQuestion, "继续旁路提问…"),
        (Self::CancelQuestion, "停止旁路提问…"),
        (Self::SendSubagent, "向子 Agent 发送消息…"),
        (Self::ReleaseTask, "释放任务…"),
        (Self::SearchMemory, "搜索记忆…"),
        (Self::Projects, "项目列表"),
        (Self::LinkProject, "关联已有项目…"),
        (Self::Storage, "会话存储"),
        (Self::History, "会话历史"),
        (Self::Compactions, "压缩记录"),
        (Self::DeleteSession, "删除会话…"),
    ];
}
impl Workspace {
    pub(super) fn execute(
        &mut self,
        action: WorkspaceCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(action, WorkspaceCommand::OpenProject) {
            self.open_project(cx);
            return;
        }
        if matches!(action, WorkspaceCommand::RemoteProject) {
            self.remote_connection_dialog(window, cx);
            return;
        }
        if matches!(action, WorkspaceCommand::Accounts) {
            self.settings(true, window, cx);
            return;
        }
        let resource_page = match action {
            WorkspaceCommand::Skills => Some(2),
            WorkspaceCommand::Mcp => Some(3),
            WorkspaceCommand::Memory => Some(4),
            WorkspaceCommand::Plugins => Some(5),
            _ => None,
        };
        if let Some(page) = resource_page {
            self.settings(false, window, cx);
            if let Some(settings) = &self.settings_page {
                settings.update(cx, |view, cx| {
                    view.select_page(page, window, cx);
                });
                cx.emit(WorkspaceEvent::Settings(settings.clone()));
            }
            return;
        }
        if matches!(action, WorkspaceCommand::RefreshModels) {
            if self.info.is_none() || !self.connected {
                self.connect(window, cx);
            } else {
                self.command(Command::RefreshModels, cx);
            }
            return;
        }
        let Some(info) = &self.info else { return };
        let session = info.id.clone();
        if matches!(
            action,
            WorkspaceCommand::Tasks
                | WorkspaceCommand::AddTask
                | WorkspaceCommand::TaskStatus
                | WorkspaceCommand::ReleaseTask
        ) {
            self.show_project_tasks(matches!(action, WorkspaceCommand::AddTask), window, cx);
            return;
        }
        self.page = MainPage::Conversation;

        if matches!(action, WorkspaceCommand::File) {
            self.show_files(None, window, cx);
            return;
        }
        if matches!(action, WorkspaceCommand::Undo) {
            self.show_checkpoints(None, window, cx);
            return;
        }
        if matches!(action, WorkspaceCommand::Changes | WorkspaceCommand::Diff) {
            self.show_review(None, window, cx);
            return;
        }

        let inspection = match action {
            WorkspaceCommand::Status => Some(Inspection::Status),
            WorkspaceCommand::Context => Some(Inspection::Context),
            WorkspaceCommand::Skills => Some(Inspection::Skills),
            WorkspaceCommand::Mcp => Some(Inspection::Mcp),
            WorkspaceCommand::Diagnostics => Some(Inspection::Diagnostics),
            WorkspaceCommand::Export => Some(Inspection::ExportPatch),
            _ => None,
        };
        if let Some(section) = inspection {
            self.command(
                Command::Request(Request::SessionAction {
                    session,
                    action: SessionAction::Inspect { section },
                }),
                cx,
            );
            return;
        }
        match action {
            WorkspaceCommand::Tasks
            | WorkspaceCommand::AddTask
            | WorkspaceCommand::TaskStatus
            | WorkspaceCommand::ReleaseTask => unreachable!("handled above"),
            WorkspaceCommand::Projects
            | WorkspaceCommand::Storage
            | WorkspaceCommand::History
            | WorkspaceCommand::Compactions => {
                use orbit_protocol::admin::{Action, SessionSection};
                let action = match action {
                    WorkspaceCommand::Projects => Action::Projects,
                    WorkspaceCommand::Storage => Action::SessionStorage,
                    WorkspaceCommand::History => Action::InspectSession {
                        id: session,
                        section: SessionSection::History,
                    },
                    _ => Action::InspectSession {
                        id: session,
                        section: SessionSection::Compactions,
                    },
                };
                self.command(Command::Request(Request::Admin { action }), cx);
            }
            WorkspaceCommand::LinkProject => self.prompt(
                ("关联已有项目", "项目 ID（可在项目列表中查看）"),
                "",
                "关联",
                |id| {
                    Ok(Request::Admin {
                        action: orbit_protocol::admin::Action::LinkProject { id },
                    })
                },
                window,
                cx,
            ),
            WorkspaceCommand::DeleteSession => self.delete_session(&session, window, cx),
            WorkspaceCommand::QuestionHistory => self.open_question_history(window, cx),
            WorkspaceCommand::InputQueue => self.open_input_queue(window, cx),
            WorkspaceCommand::SideQuestions
            | WorkspaceCommand::CancelQuestion
            | WorkspaceCommand::FollowQuestion => self.open_side_questions(window, cx),
            WorkspaceCommand::SendSubagent => self.open_child_agents(window, cx),
            WorkspaceCommand::SearchMemory => self.prompt(
                ("搜索记忆", "关键词"),
                "",
                "搜索",
                move |query| {
                    Ok(Request::SessionAction {
                        session: session.clone(),
                        action: SessionAction::Memory {
                            memory: MemoryAction::Search { query },
                        },
                    })
                },
                window,
                cx,
            ),
            WorkspaceCommand::AddMemory => self.prompt(
                ("保存记忆", "本次会话需要记住的事实"),
                "",
                "保存",
                move |content| {
                    Ok(Request::SessionAction {
                        session: session.clone(),
                        action: SessionAction::Memory {
                            memory: MemoryAction::Add {
                                category: MemoryCategory::Fact,
                                content,
                            },
                        },
                    })
                },
                window,
                cx,
            ),
            WorkspaceCommand::ForgetMemory => self.prompt(
                ("停用记忆", "记忆 ID"),
                "",
                "停用",
                move |value| {
                    Ok(Request::SessionAction {
                        session: session.clone(),
                        action: SessionAction::Memory {
                            memory: MemoryAction::Forget {
                                id: value.parse().map_err(|_| "记忆 ID 应为数字")?,
                            },
                        },
                    })
                },
                window,
                cx,
            ),
            WorkspaceCommand::LoadSkill => self.prompt(
                ("加载技能", "技能名称"),
                "",
                "加载",
                move |name| {
                    Ok(Request::SessionAction {
                        session: session.clone(),
                        action: SessionAction::LoadSkill { name },
                    })
                },
                window,
                cx,
            ),
            WorkspaceCommand::Compact => {
                if !self.busy {
                    self.command(
                        Command::Request(Request::Start {
                            session,
                            request_id: orbit_protocol::request_id(),
                            prompt: String::new(),
                            images: vec![],
                            compact: true,
                            model: None,
                        }),
                        cx,
                    );
                }
            }
            WorkspaceCommand::Rename => self.rename_session(&session, window, cx),
            WorkspaceCommand::Directory => self.prompt(
                ("切换目录", "工作区路径"),
                &self.location.clone(),
                "切换",
                move |path| {
                    Ok(Request::SessionAction {
                        session: session.clone(),
                        action: SessionAction::ChangeDirectory { path },
                    })
                },
                window,
                cx,
            ),
            WorkspaceCommand::Memory => {
                self.command(
                    Command::Request(Request::SessionAction {
                        session,
                        action: SessionAction::Memory {
                            memory: MemoryAction::List {
                                include_inactive: true,
                            },
                        },
                    }),
                    cx,
                );
            }
            WorkspaceCommand::Plugins => {
                self.command(
                    Command::Request(Request::Plugins {
                        session,
                        expected_workspace: Some(info.workspace.clone()),
                        action: orbit_protocol::plugins::Action::List,
                    }),
                    cx,
                );
            }
            WorkspaceCommand::RefreshSkills => {
                self.command(
                    Command::Request(Request::SessionAction {
                        session,
                        action: SessionAction::RefreshSkills,
                    }),
                    cx,
                );
            }
            WorkspaceCommand::Plan => self.show_plan(window, cx),
            WorkspaceCommand::Questions => self.questions(window, cx),
            WorkspaceCommand::Ask => self.prompt(
                ("旁路提问", "主任务继续运行"),
                "",
                "提问",
                move |question| {
                    Ok(Request::Ask {
                        session: session.clone(),
                        question,
                        follow_up: None,
                    })
                },
                window,
                cx,
            ),
            WorkspaceCommand::Subagent => self.open_child_agents(window, cx),
            _ => {}
        }
    }

    pub(super) fn show_report(
        &mut self,
        title: String,
        text: String,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.weak_entity();
        if let Some(panel) = &self.panel
            && panel.read(cx).title() == title
        {
            panel.update(cx, |panel, cx| panel.set(text, cx));
            return;
        }
        self.panel = Some(cx.new(|cx| panel::Panel::new(title, text, owner, cx)));
        cx.notify();
    }

    pub(super) fn prompt(
        &self,
        (title, label): (&str, &str),
        initial: &str,
        commit: &'static str,
        request: impl Fn(String) -> Result<Request, String> + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = title.to_owned();
        let label = label.to_owned();
        let input = cx.new(|cx| {
            let mut state = InputState::new(window, cx);
            state.set_value(initial, window, cx);
            state
        });
        let owner = cx.weak_entity();
        let request = std::rc::Rc::new(request);
        let first = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                input.read(cx).focus_handle(cx).focus(window, cx);
            }
            let input2 = input.clone();
            let owner = owner.clone();
            let request = request.clone();
            dialog
                .title(title.clone())
                .child(
                    v_flex()
                        .gap_2()
                        .child(label.clone())
                        .child(Input::new(&input)),
                )
                .footer(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("cancel-form")
                                .label("取消")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(Button::new("submit-form").primary().label(commit).on_click(
                            move |_, window, cx| {
                                let value = input2.read(cx).value().trim().to_string();
                                if value.is_empty() {
                                    window.push_notification(Notification::error("请填写内容"), cx);
                                    return;
                                }
                                match request(value) {
                                    Ok(request) => {
                                        if owner
                                            .update(cx, |view, cx| {
                                                view.command(Command::Request(request), cx)
                                            })
                                            .unwrap_or(false)
                                        {
                                            window.close_dialog(cx);
                                        }
                                    }
                                    Err(error) => {
                                        window.push_notification(Notification::error(error), cx)
                                    }
                                }
                            },
                        )),
                )
        });
    }

    pub(super) fn commands(&self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::command::{Command as Palette, CommandItem, CommandState};
        let owner = cx.weak_entity();
        let state = cx.new(|cx| CommandState::new(window, cx));
        let first = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |state, cx| state.focus(window, cx));
            }
            let owner = owner.clone();
            dialog
                .close_button(false)
                .p_0()
                .width(
                    (window.rem_size() * 34.)
                        .min(window.viewport_size().width - window.rem_size() * 4.),
                )
                .child(
                    Palette::new(&state)
                        .placeholder("搜索命令…")
                        .bordered(false)
                        .max_h((window.viewport_size().height * 0.55).min(window.rem_size() * 24.))
                        .on_cancel(|window, cx| window.close_dialog(cx))
                        .footer(|_, _, cx| {
                            h_flex()
                                .px_3()
                                .py_2()
                                .border_t_1()
                                .border_color(cx.theme().border)
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("↑↓ 选择　Enter 打开　Esc 关闭")
                        })
                        .items(WorkspaceCommand::ALL.iter().map(|(command, label)| {
                            CommandItem::new()
                                .label(*label)
                                .keywords([format!("{command:?}")])
                        }))
                        .on_confirm(move |index, window, cx| {
                            if let Some((command, _)) = WorkspaceCommand::ALL.get(index.row) {
                                window.close_dialog(cx);
                                let _ =
                                    owner.update(cx, |view, cx| view.execute(*command, window, cx));
                            }
                        }),
                )
        });
    }

    pub(super) fn plan_action(&mut self, action: plan::Action, cx: &mut Context<Self>) {
        if let Some(info) = &self.info {
            self.command(
                Command::Request(Request::Plan {
                    session: info.id.clone(),
                    action,
                }),
                cx,
            );
        }
    }
    pub(super) fn show_plan(&self, window: &mut Window, cx: &mut Context<Self>) {
        let owner = cx.weak_entity();
        let generation = self.planning.generation;
        let Some(plan) = &self.planning.plan else {
            window.open_dialog(cx, move |dialog, _, _| {
                let owner = owner.clone();
                dialog
                    .title("计划")
                    .child("进入计划模式后，Agent 会先分析并提交方案，批准后再执行。")
                    .footer(Button::new("enter-plan").label("进入计划模式").on_click(
                        move |_, window, cx| {
                            let _ = owner
                                .update(cx, |view, cx| view.plan_action(plan::Action::Enter, cx));
                            window.close_dialog(cx);
                        },
                    ))
            });
            return;
        };
        let title = plan.title.clone();
        let markdown = plan.markdown.clone();
        let reviewable = plan.status == plan::Status::Review && !self.busy;
        window.open_dialog(cx, move |dialog, window, _| {
            let approve = owner.clone();
            let feedback = owner.clone();
            dialog
                .title(title.clone())
                .width(window.rem_size() * 44.)
                .child(
                    TextView::markdown("plan", markdown.clone())
                        .style(crate::theme::document_style()),
                )
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(Button::new("plan-feedback").label("修改意见…").on_click(
                            move |_, window, cx| {
                                window.close_dialog(cx);
                                let _ = feedback.update(cx, |view, cx| {
                                    if let Some(info) = &view.info {
                                        let session = info.id.clone();
                                        view.prompt(
                                            ("修改计划", "修改意见"),
                                            "",
                                            "提交意见",
                                            move |text| {
                                                Ok(Request::Plan {
                                                    session: session.clone(),
                                                    action: plan::Action::Feedback {
                                                        generation,
                                                        text,
                                                    },
                                                })
                                            },
                                            window,
                                            cx,
                                        );
                                    }
                                });
                            },
                        ))
                        .child(
                            Button::new("approve-plan")
                                .primary()
                                .label("批准并执行")
                                .disabled(!reviewable)
                                .on_click(move |_, window, cx| {
                                    let _ = approve.update(cx, |view, cx| {
                                        view.plan_action(plan::Action::Approve { generation }, cx)
                                    });
                                    window.close_dialog(cx);
                                }),
                        ),
                )
        });
    }

    pub(super) fn track_feedback(&mut self, update: &Update, cx: &mut Context<Self>) {
        match update {
            Update::Started { presentation, .. } => {
                self.replaying = presentation.replayed;
            }
            Update::Agent(event) => match &**event {
                AgentEvent::ContextUsage { estimated_input } => {
                    self.context_used = *estimated_input
                }
                AgentEvent::Failed { error } | AgentEvent::CompactionFailed { error, .. }
                    if !self.replaying =>
                {
                    self.report_error(error.clone(), cx);
                }
                _ => {}
            },
            _ => {}
        }
    }

    pub(super) fn dispatch_slash(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let value = self.input.read(cx).value();
        let (name, arg) = value
            .trim()
            .split_once(char::is_whitespace)
            .unwrap_or((value.trim(), ""));
        let arg = arg.trim();
        if matches!(name, "/file" | "/diff" | "/changes" | "/undo") {
            if self.info.is_none() || !self.connected {
                self.report_error("请先连接工作区，再打开文件或差异", cx);
                return true;
            }
            let path = (!arg.is_empty()).then(|| arg.to_owned());
            if name == "/file" {
                self.show_files(path, window, cx);
            } else if name == "/undo" {
                self.show_checkpoints(path, window, cx);
            } else {
                self.show_review(path, window, cx);
            }
            self.input
                .update(cx, |input, cx| input.set_value("", window, cx));
            return true;
        }
        let action = match name {
            "/status" => Some(WorkspaceCommand::Status),
            "/context" => Some(WorkspaceCommand::Context),
            "/compact" => Some(WorkspaceCommand::Compact),
            "/skills" => Some(WorkspaceCommand::Skills),
            "/mcp" => Some(WorkspaceCommand::Mcp),
            "/diagnostics" => Some(WorkspaceCommand::Diagnostics),
            "/plan" if arg.is_empty() || arg == "view" => Some(WorkspaceCommand::Plan),
            "/plugins" => Some(WorkspaceCommand::Plugins),
            "/memory" if arg.is_empty() => Some(WorkspaceCommand::Memory),
            "/btw" if arg.is_empty() => Some(WorkspaceCommand::Ask),
            "/queue" if arg.is_empty() => Some(WorkspaceCommand::InputQueue),
            _ => None,
        };
        if let Some(action) = action {
            self.execute(action, window, cx);
        } else {
            let Some(info) = &self.info else { return true };
            let session = info.id.clone();
            let request = match name {
                "/btw" => Request::Ask {
                    session,
                    question: arg.into(),
                    follow_up: None,
                },
                "/cd" if !arg.is_empty() => Request::SessionAction {
                    session,
                    action: SessionAction::ChangeDirectory { path: arg.into() },
                },
                "/model" if arg == "refresh" => {
                    if self.command(Command::RefreshModels, cx) {
                        self.input
                            .update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    return true;
                }
                "/model" if !arg.is_empty() => Request::SetModel {
                    session,
                    model: arg.into(),
                    effort: None,
                },
                "/reasoning" if !arg.is_empty() => Request::SetModel {
                    session,
                    model: info.model.clone(),
                    effort: Some(arg.into()),
                },
                "/task" if !arg.is_empty() => Request::SessionAction {
                    session,
                    action: SessionAction::Task {
                        task: TaskAction::Add { title: arg.into() },
                    },
                },
                "/tasks" | "/memory" => match orbit_client::completion::project_action(name, arg) {
                    Ok(action) => Request::SessionAction { session, action },
                    Err(error) => {
                        self.report_error(error.to_string(), cx);
                        return true;
                    }
                },
                "/mode" if matches!(arg, "plan" | "build") => Request::Plan {
                    session,
                    action: if arg == "plan" {
                        plan::Action::Enter
                    } else {
                        plan::Action::Leave
                    },
                },
                "/permissions" => {
                    let mode = match arg.to_ascii_lowercase().as_str() {
                        "ask" => orbit_protocol::Mode::Ask,
                        "auto" => orbit_protocol::Mode::Auto,
                        "yolo" => orbit_protocol::Mode::Yolo,
                        _ => {
                            self.report_error("审批模式：ask、auto、yolo", cx);
                            return true;
                        }
                    };
                    Request::SetMode { session, mode }
                }
                "/skill" if !arg.is_empty() => Request::SessionAction {
                    session,
                    action: SessionAction::LoadSkill { name: arg.into() },
                },
                "/resume" if !arg.is_empty() => {
                    self.open(Some(arg.into()), cx);
                    return true;
                }
                "/usage" => {
                    self.show_usage(window, cx);
                    return true;
                }
                "/help" => {
                    self.commands(window, cx);
                    return true;
                }
                _ => {
                    self.report_error("未知命令。使用 Ctrl+Shift+P 查看可用操作。", cx);
                    cx.notify();
                    return true;
                }
            };
            if !self.command(Command::Request(request), cx) {
                return true;
            }
        }
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        true
    }
}
