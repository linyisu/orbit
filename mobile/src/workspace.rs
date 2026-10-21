//! Mobile workspace views using the same typed operations as the desktop.
use super::*;
use remote::protocol::{self as p, Request as R};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(super) struct State {
    pub profile: Signal<Option<p::SessionInfo>>,
    pub planning: Signal<p::plan::State>,
    pub response: Signal<Arc<Response>>,
    screen: Signal<Option<Screen>>,
    path: Signal<String>,
    input: Signal<String>,
}

impl State {
    pub fn new() -> Self {
        Self {
            profile: signal_with(None, SignalEquals::Never),
            planning: signal(p::plan::State::default()),
            response: signal_with(Arc::new(Response::Ok), SignalEquals::Never),
            screen: signal(None),
            path: signal(String::new()),
            input: signal(String::new()),
        }
    }
}

fn send(app: AppState, request: R) {
    app.inspector
        .response
        .set(Arc::new(Response::Ok));
    app.command(Command::Action(Box::new(request)));
}

fn session(app: AppState) -> String {
    app.active
        .get_untracked()
        .unwrap_or_default()
}

fn row(label: String, action: impl Fn() + 'static) -> impl IntoElement {
    nsh_core::ui::view()
        .min_h(px(48.0))
        .py_3()
        .px_2()
        .label(label.clone())
        .on_press(action)
        .child(text(label).w_full().text_size(px(16.0)))
}

pub(super) fn projects(app: AppState) -> impl IntoElement {
    scroll().flex_1().child(
        nsh_core::ui::view()
            .p_4()
            .gap_3()
            .child(text("打开电脑上的项目"))
            .child(field(app.inspector.path, "电脑上的目录路径"))
            .child(button("打开项目", move || {
                app.command(Command::Workspace(app.inspector.path.get_untracked()));
            }))
            .child(banner(app))
            .child(text("最近项目"))
            .child(dynamic(move || {
                let mut body = nsh_core::ui::view().gap_2();
                let paths: std::collections::BTreeSet<_> = app
                    .sessions
                    .get()
                    .into_iter()
                    .map(|s| s.workspace)
                    .collect();
                for path in paths {
                    let selected = path.clone();
                    body = body.child(row(path, move || {
                        app.inspector.path.set(selected.clone());
                        app.command(Command::Workspace(selected.clone()));
                    }));
                }
                body
            }))
            .child(text("所有会话"))
            .child(dynamic(move || {
                let mut body = nsh_core::ui::view().gap_1();
                for item in app.sessions.get() {
                    body = body.child(row(
                        format!("{}\n{}", item.title, item.workspace),
                        move || {
                            app.command(Command::Open(Some(item.id.clone())));
                        },
                    ));
                }
                body
            })),
    )
}

fn files(app: AppState, path: String) {
    let path = if path.trim().is_empty() {
        ".".into()
    } else {
        path
    };
    send(
        app,
        R::Files {
            session: session(app),
            expected_workspace: Some(app.workspace.get_untracked()),
            action: p::files::Action::List {
                path,
                offset: 0,
                include_hidden: true,
            },
        },
    );
}
fn review(app: AppState, action: p::review::Action) {
    send(
        app,
        R::Review {
            session: session(app),
            expected_workspace: app.workspace.get_untracked(),
            action,
        },
    );
}
fn tasks(app: AppState, action: p::project::TaskAction) {
    send(
        app,
        R::Tasks {
            session: session(app),
            expected_workspace: Some(app.workspace.get_untracked()),
            action,
        },
    );
}
fn agents(app: AppState, action: p::subagents::Action) {
    send(
        app,
        R::Subagents {
            session: session(app),
            expected_workspace: app.workspace.get_untracked(),
            action,
        },
    );
}
fn queue(app: AppState, action: p::input_queue::Action) {
    send(
        app,
        R::InputQueue {
            session: session(app),
            expected_workspace: app.workspace.get_untracked(),
            action,
        },
    );
}
fn resources(app: AppState, action: p::resources::Action) {
    send(
        app,
        R::Resources {
            session: session(app),
            expected_workspace: Some(app.workspace.get_untracked()),
            action,
        },
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Files,
    Changes,
    History,
    Models,
    Tasks,
    Agents,
    Queue,
    Skills,
    Mcp,
    Memories,
    Plugins,
    Plan,
    Permissions,
}

fn open(app: AppState, screen: Screen) {
    app.inspector.screen.set(Some(screen));
    app.inspector
        .response
        .set(Arc::new(Response::Ok));
    match screen {
        Screen::Files => files(app, ".".into()),
        Screen::Changes => review(
            app,
            p::review::Action::Status {
                offset: 0,
                include_patch: true,
            },
        ),
        Screen::History => review(
            app,
            p::review::Action::History {
                revision: "HEAD".into(),
                offset: 0,
            },
        ),
        Screen::Models => send(
            app,
            R::ConfigureModels {
                session: session(app),
                configuration: None,
            },
        ),
        Screen::Tasks => tasks(
            app,
            p::project::TaskAction::List {
                include_completed: true,
            },
        ),
        Screen::Agents => agents(app, p::subagents::Action::List { offset: 0 }),
        Screen::Queue => queue(app, p::input_queue::Action::List { after: None }),
        Screen::Skills => send(
            app,
            R::Library {
                action: p::library::Action::Skills(p::resources::Action::Skills),
            },
        ),
        Screen::Mcp => resources(app, p::resources::Action::Mcp),
        Screen::Memories => resources(app, p::resources::Action::Memories),
        Screen::Plugins => send(
            app,
            R::Library {
                action: p::library::Action::Plugins(p::plugins::Action::List),
            },
        ),
        Screen::Plan | Screen::Permissions => {}
    }
}

pub(super) fn view(app: AppState) -> impl IntoElement {
    scroll().flex_1().child(
        nsh_core::ui::view()
            .p_4()
            .gap_3()
            .child(banner(app))
            .child(dynamic(move || {
                let mut body = nsh_core::ui::view().gap_3();
                if app.active.get().is_none() {
                    return body.child(text("先打开或新建会话，再管理该工作区。"));
                }
                let Some(screen) = app.inspector.screen.get() else {
                    body = body.child(text("项目内容").text_size(px(13.0)));
                    for (label, screen) in [
                        ("文件", Screen::Files),
                        ("变更", Screen::Changes),
                        ("历史", Screen::History),
                        ("模型", Screen::Models),
                        ("计划", Screen::Plan),
                        ("工具审批", Screen::Permissions),
                        ("任务", Screen::Tasks),
                        ("子 Agent", Screen::Agents),
                        ("待发送消息", Screen::Queue),
                        ("技能", Screen::Skills),
                        ("MCP", Screen::Mcp),
                        ("会话记忆", Screen::Memories),
                        ("插件", Screen::Plugins),
                    ] {
                        if screen == Screen::Models {
                            body = body.child(text("会话设置").text_size(px(13.0)));
                        }
                        if screen == Screen::Tasks {
                            body = body.child(text("任务管理").text_size(px(13.0)));
                        }
                        if screen == Screen::Skills {
                            body = body.child(text("扩展").text_size(px(13.0)));
                        }
                        body = body.child(row(label.into(), move || open(app, screen)));
                    }
                    return body;
                };
                body = body.child(
                    nsh_core::ui::view()
                        .flex_row()
                        .justify_end()
                        .child(crate::components::icon_button(
                            "刷新",
                            crate::components::Icon::Refresh,
                            move || open(app, screen),
                        )),
                );
                match screen {
                    Screen::Models => {
                        if let Some(profile) = app.inspector.profile.get() {
                            body = body.child(text(format!("当前模型：{}", profile.model)));
                            for model in profile.models {
                                let name = model.clone();
                                body = body.child(row(model, move || {
                                    send(
                                        app,
                                        R::SetModel {
                                            session: session(app),
                                            model: name.clone(),
                                            effort: None,
                                        },
                                    )
                                }));
                            }
                            for effort in profile.reasoning_efforts {
                                let model = profile.model.clone();
                                let selected = effort.clone();
                                body = body.child(row(
                                    format!("推理强度 · {effort}"),
                                    move || {
                                        send(
                                            app,
                                            R::SetModel {
                                                session: session(app),
                                                model: model.clone(),
                                                effort: Some(selected.clone()),
                                            },
                                        )
                                    },
                                ));
                            }
                        }
                    }
                    Screen::Plan => {
                        let state = app.inspector.planning.get();
                        body = body
                            .child(button("进入规划", move || {
                                send(
                                    app,
                                    R::Plan {
                                        session: session(app),
                                        action: p::plan::Action::Enter,
                                    },
                                )
                            }))
                            .child(button("返回执行", move || {
                                send(
                                    app,
                                    R::Plan {
                                        session: session(app),
                                        action: p::plan::Action::Leave,
                                    },
                                )
                            }));
                        if let Some(plan) = state.plan {
                            body = body.child(markdown(move || plan.markdown.clone()));
                            if plan.status == p::plan::Status::Review {
                                body = body
                                    .child(field(app.inspector.input, "计划修改意见"))
                                    .child(button("反馈", move || {
                                        send(
                                            app,
                                            R::Plan {
                                                session: session(app),
                                                action: p::plan::Action::Feedback {
                                                    generation: state.generation,
                                                    text: app.inspector.input.get_untracked(),
                                                },
                                            },
                                        )
                                    }))
                                    .child(button("批准计划", move || {
                                        send(
                                            app,
                                            R::Plan {
                                                session: session(app),
                                                action: p::plan::Action::Approve {
                                                    generation: state.generation,
                                                },
                                            },
                                        )
                                    }));
                            }
                        }
                    }
                    Screen::Permissions => {
                        if let Some(profile) = app.inspector.profile.get() {
                            body = body.child(text(format!("当前审批模式：{:?}", profile.mode)));
                        }
                        body = body
                            .child(button("询问", move || {
                                send(
                                    app,
                                    R::SetMode {
                                        session: session(app),
                                        mode: p::Mode::Ask,
                                    },
                                )
                            }))
                            .child(button("自动", move || {
                                send(
                                    app,
                                    R::SetMode {
                                        session: session(app),
                                        mode: p::Mode::Auto,
                                    },
                                )
                            }))
                            .child(button("全部允许", move || {
                                send(
                                    app,
                                    R::SetMode {
                                        session: session(app),
                                        mode: p::Mode::Yolo,
                                    },
                                )
                            }));
                    }
                    _ => {}
                }
                body.child(result(app, app.inspector.response.get()))
            })),
    )
}

fn result(app: AppState, response: Arc<Response>) -> impl IntoElement {
    let mut body = nsh_core::ui::view().gap_3();
    match response.as_ref() {
        Response::Report(report) => {
            let content = report.text.clone();
            body = body
                .child(text(report.title.clone()))
                .child(markdown(move || content.clone()));
        }
        Response::File(content) => {
            let content = match base64::engine::general_purpose::STANDARD.decode(content) {
                Ok(bytes) => match String::from_utf8(bytes) {
                    Ok(text) => format!("```\n{text}\n```"),
                    Err(_) => "此文件不是 UTF-8 文本。".into(),
                },
                Err(_) => "文件内容解码失败，请重新读取。".into(),
            };
            body = body.child(markdown(move || content.clone()));
        }
        Response::Files(p::files::Snapshot::Directory {
            path,
            entries,
            next_offset,
            ..
        }) => {
            let parent = std::path::Path::new(path)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|| ".".into());
            body = body
                .child(text(path.clone()))
                .child(button("上级目录", move || files(app, parent.clone())));
            for entry in entries.clone() {
                body = body.child(row(entry.path.clone(), move || match entry.kind {
                    p::files::EntryKind::Directory => files(app, entry.path.clone()),
                    p::files::EntryKind::File => send(
                        app,
                        R::ReadFile {
                            session: session(app),
                            path: entry.path.clone(),
                            allow_missing: false,
                        },
                    ),
                }));
            }
            if let Some(offset) = *next_offset {
                let path = path.clone();
                body = body.child(button("下一页", move || {
                    send(
                        app,
                        R::Files {
                            session: session(app),
                            expected_workspace: Some(app.workspace.get_untracked()),
                            action: p::files::Action::List {
                                path: path.clone(),
                                offset,
                                include_hidden: true,
                            },
                        },
                    )
                }));
            }
        }
        Response::Review(
            p::review::Snapshot::Status { patch, .. } | p::review::Snapshot::Commit { patch, .. },
        ) => {
            let content = format!(
                "```diff\n{}\n```",
                patch
                    .clone()
                    .unwrap_or_else(|| "没有变更".into())
            );
            body = body.child(markdown(move || content.clone()));
        }
        Response::Review(p::review::Snapshot::History { commits, next, .. }) => {
            for commit in commits.clone() {
                body = body.child(row(
                    format!(
                        "{}\n{} · {}",
                        commit.subject, commit.author, commit.short_id
                    ),
                    move || {
                        review(
                            app,
                            p::review::Action::Commit {
                                revision: commit.id.clone(),
                                include_patch: true,
                            },
                        );
                    },
                ));
            }
            if let Some(offset) = *next {
                body = body.child(button("更早的提交", move || {
                    review(
                        app,
                        p::review::Action::History {
                            revision: "HEAD".into(),
                            offset,
                        },
                    )
                }));
            }
        }
        Response::Tasks(snapshot) => {
            body = body
                .child(field(app.inspector.input, "新任务标题"))
                .child(button("添加任务", move || {
                    tasks(
                        app,
                        p::project::TaskAction::Add {
                            title: app.inspector.input.get_untracked(),
                        },
                    )
                }));
            for task in snapshot.tasks.clone() {
                body = body
                    .child(text(format!("{} · {}", task.title, task.status.as_str())))
                    .child(text(task.details))
                    .child(button("标记完成", move || {
                        tasks(
                            app,
                            p::project::TaskAction::SetStatus {
                                id: task.id,
                                status: p::project::TaskStatus::Completed,
                            },
                        )
                    }))
                    .child(button("重新开始", move || {
                        tasks(
                            app,
                            p::project::TaskAction::SetStatus {
                                id: task.id,
                                status: p::project::TaskStatus::InProgress,
                            },
                        )
                    }));
            }
        }
        Response::Subagents(p::subagents::Snapshot::List {
            agents: items,
            next,
        }) => {
            body = body.child(field(app.inspector.input, "给子 Agent 的消息"));
            for agent in items.clone() {
                let id = agent.id.clone();
                let read = agent.id.clone();
                body = body
                    .child(text(format!("{} · {}", agent.task, agent.status)))
                    .child(button("查看输出", move || {
                        send(
                            app,
                            R::ReadSubagent {
                                session: session(app),
                                agent: read.clone(),
                                after: 0,
                            },
                        )
                    }))
                    .child(button("发送消息", move || {
                        agents(
                            app,
                            p::subagents::Action::Send {
                                id: id.clone(),
                                message: app.inspector.input.get_untracked(),
                            },
                        )
                    }))
                    .child(button("停止", move || {
                        agents(
                            app,
                            p::subagents::Action::Cancel {
                                id: agent.id.clone(),
                                run: agent.run,
                            },
                        )
                    }));
            }
            if let Some(offset) = *next {
                body = body.child(button("下一页", move || {
                    agents(app, p::subagents::Action::List { offset })
                }));
            }
        }
        Response::Subagent(page) => {
            let mut conversation = Conversation::default();
            conversation.apply(page);
            for item in conversation.items {
                body = body.child(markdown(move || item.content.clone()));
            }
        }
        Response::InputQueue(p::input_queue::Snapshot::List { entries, run, next }) => {
            for entry in entries.clone() {
                let expected = entry.content.clone();
                let run = *run;
                body = body.child(text(entry.content.clone()));
                if entry.editable() {
                    body = body
                        .child(button("引导当前任务", move || {
                            queue(
                                app,
                                p::input_queue::Action::Steer {
                                    id: entry.id,
                                    expected: expected.clone(),
                                    run,
                                },
                            )
                        }))
                        .child(button("移除", move || {
                            queue(
                                app,
                                p::input_queue::Action::Remove {
                                    id: entry.id,
                                    expected: entry.content.clone(),
                                    run,
                                },
                            )
                        }));
                }
            }
            if let Some(after) = *next {
                body = body.child(button("下一页", move || {
                    queue(app, p::input_queue::Action::List { after: Some(after) })
                }));
            }
        }
        Response::Resources(p::resources::Snapshot::Skills(skills))
        | Response::Library(p::library::Snapshot::Skills(p::resources::Snapshot::Skills(skills))) => {
            for skill in skills.clone() {
                let name = skill.name.clone();
                body = body
                    .child(text(skill.title))
                    .child(text(skill.description))
                    .child(button("查看技能", move || {
                        send(
                            app,
                            R::Library {
                                action: p::library::Action::Skills(
                                    p::resources::Action::ReadSkill { name: name.clone() },
                                ),
                            },
                        )
                    }))
                    .child(button("准备应用", move || {
                        send(
                            app,
                            R::Library {
                                action: p::library::Action::ExportSkill {
                                    name: skill.name.clone(),
                                },
                            },
                        )
                    }));
            }
        }
        Response::Library(p::library::Snapshot::Bundle(bundle)) => {
            let bundle = bundle.clone();
            body = body
                .child(text(format!("应用 {} 到当前会话", bundle.extension.name)))
                .child(button("应用", move || {
                    resources(
                        app,
                        p::resources::Action::ApplyExtension {
                            bundle: Box::new(bundle.clone()),
                        },
                    )
                }));
        }
        Response::Resources(p::resources::Snapshot::Skill { content, .. })
        | Response::Library(p::library::Snapshot::Skills(p::resources::Snapshot::Skill {
            content,
            ..
        })) => {
            let content = content.clone();
            body = body.child(markdown(move || content.clone()));
        }
        Response::Resources(p::resources::Snapshot::Mcp(servers)) => {
            for server in servers {
                body = body
                    .child(text(format!(
                        "{} · {}",
                        server.name,
                        if server.connected {
                            "已连接"
                        } else {
                            "未连接"
                        }
                    )))
                    .child(text(server.detail.clone()));
                for tool in &server.tools {
                    body = body.child(text(format!("{} · {}", tool.name, tool.description)));
                }
            }
        }
        Response::Resources(p::resources::Snapshot::Memories(memories)) => {
            body = body
                .child(field(app.inspector.input, "添加会话记忆"))
                .child(button("添加", move || {
                    resources(
                        app,
                        p::resources::Action::Memory(p::project::MemoryAction::Add {
                            category: p::project::MemoryCategory::Fact,
                            content: app.inspector.input.get_untracked(),
                        }),
                    )
                }));
            for memory in memories.clone() {
                body = body
                    .child(text(memory.content))
                    .child(button("移除记忆", move || {
                        resources(
                            app,
                            p::resources::Action::Memory(p::project::MemoryAction::Forget {
                                id: memory.id,
                            }),
                        )
                    }));
            }
        }
        Response::Library(p::library::Snapshot::Plugins(catalog)) | Response::Plugins(catalog) => {
            body = body
                .child(field(app.inspector.input, "插件市场地址或电脑目录"))
                .child(button("添加市场", move || {
                    send(
                        app,
                        R::Library {
                            action: p::library::Action::Plugins(p::plugins::Action::Add {
                                source: app.inspector.input.get_untracked(),
                            }),
                        },
                    )
                }));
            for marketplace in &catalog.marketplaces {
                let source = marketplace.source.clone();
                let remove = source.clone();
                body = body
                    .child(text(marketplace.name.clone()))
                    .child(button("刷新市场", move || {
                        send(
                            app,
                            R::Library {
                                action: p::library::Action::Plugins(p::plugins::Action::Refresh {
                                    source: source.clone(),
                                }),
                            },
                        )
                    }))
                    .child(button("移除市场", move || {
                        send(
                            app,
                            R::Library {
                                action: p::library::Action::Plugins(p::plugins::Action::Remove {
                                    source: remove.clone(),
                                }),
                            },
                        )
                    }));
                for plugin in &marketplace.plugins {
                    body = body.child(text(format!("{}\n{}", plugin.name, plugin.description)));
                }
            }
        }
        Response::Error(error) => body = body.child(text(error.clone())),
        _ => {}
    }
    body
}

pub(super) fn detail_open(app: AppState) -> bool {
    app.inspector.screen.get().is_some()
}
pub(super) fn close_detail(app: AppState) {
    app.inspector.screen.set(None);
}
pub(super) fn title(app: AppState) -> &'static str {
    match app.inspector.screen.get() {
        Some(Screen::Files) => "文件",
        Some(Screen::Changes) => "变更",
        Some(Screen::History) => "历史",
        Some(Screen::Models) => "模型",
        Some(Screen::Tasks) => "任务",
        Some(Screen::Agents) => "子 Agent",
        Some(Screen::Queue) => "待发送消息",
        Some(Screen::Skills) => "技能",
        Some(Screen::Mcp) => "MCP",
        Some(Screen::Memories) => "会话记忆",
        Some(Screen::Plugins) => "插件",
        Some(Screen::Plan) => "计划",
        Some(Screen::Permissions) => "工具审批",
        None => "工作区",
    }
}
