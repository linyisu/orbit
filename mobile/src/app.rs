//! Android navigation and view state. Transport and persisted event interpretation are shared.
use crate::components::{button, field, markdown, page_header, primary_button};
use base64::Engine;
use nsh_base::{keyboard_avoiding_view, nav_page, nav_stack};
use nsh_core::{storage::Storage, ui::prelude::*};
use orbit_client::remote::{
    self, Command, Event, Pairing, Permissions, Response, Session,
    presentation::{Conversation, Item, Kind},
    vault,
};
use tokio::sync::mpsc;

#[path = "workspace.rs"]
mod workspace;

const VAULT_KEY: &str = "orbit.connections.v2";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Closed,
    Menu,
    Computers,
    Settings,
    Projects,
    Workspace,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkState {
    Disconnected,
    Connecting,
    Connected,
    Recovering,
}

impl LinkState {
    fn attached(self) -> bool {
        matches!(self, Self::Connected | Self::Recovering)
    }
}

#[derive(Clone, Copy)]
struct AppState {
    owner: Owner,
    inspector: workspace::State,
    panel: Signal<Panel>,
    status: Signal<String>,
    link: Signal<LinkState>,
    workspace: Signal<String>,
    device: Signal<String>,
    permissions: Signal<Option<Permissions>>,
    sessions: Signal<Vec<Session>>,
    active: Signal<Option<String>>,
    conversation: Signal<Conversation>,
    approvals: Signal<Vec<(u64, String, String)>>,
    detail: Signal<Option<Item>>,
    draft: Signal<String>,
    home_draft: Signal<String>,
    opening_draft: Signal<Option<String>>,
    pending: Signal<Option<String>>,
    scanning: Signal<bool>,
    preview: Signal<String>,
    pairing_busy: Signal<bool>,
    attachments: Signal<Vec<remote::Attachment>>,
    about: Signal<bool>,
    contents: Signal<vault::Contents>,
    save_revision: Signal<u64>,
    connection_revision: Signal<u64>,
    questions: Signal<Vec<remote::user_input::Record>>,
    sender: Stored<Option<mpsc::Sender<Command>>>,
    worker: Stored<Option<tokio::task::JoinHandle<()>>>,
}

impl AppState {
    fn spawn(self, future: impl std::future::Future<Output = ()> + 'static) {
        // Connections and saves outlive the page that initiated them.
        self.owner.run(|| {
            spawn(future);
        });
    }

    fn paired(self, pairing: Pairing) {
        self.contents.update(|contents| {
            contents.remember(pairing.clone());
        });
        self.save();
        self.connect(pairing);
    }

    fn pair(self, code: String) {
        if self.pairing_busy.get_untracked() {
            return;
        }
        self.pairing_busy.set(true);
        self.link.set(LinkState::Connecting);
        self.status.set("正在连接电脑…".into());
        let saved = self
            .contents
            .with_untracked(|c| c.devices.clone());
        let name = crate::platform::device_name();
        self.spawn(async move {
            let result =
                background(async move { remote::pair_named(&code, &saved, &name).await }).await;
            self.pairing_busy.set(false);
            match result {
                Ok(Ok(pairing)) => self.paired(pairing),
                Ok(Err(error)) => self
                    .status
                    .set(format!("配对未完成：{error}。请在电脑重新生成二维码。")),
                Err(_) => self
                    .status
                    .set("配对已中断，请重试。".into()),
            }
        });
    }

    fn scan(self) {
        if self.scanning.get_untracked() || self.pairing_busy.get_untracked() {
            return;
        }
        self.scanning.set(true);
        self.preview.set(String::new());
        self.spawn(async move {
            match background(crate::platform::camera_permission()).await {
                Ok(Ok(())) => {}
                result => {
                    self.scanning.set(false);
                    self.status.set(match result {
                        Ok(Err(error)) => error,
                        _ => "无法打开相机".into(),
                    });
                    return;
                }
            }
            let (send, mut receive) = mpsc::channel(2);
            let worker = background(async move {
                tokio::task::spawn_blocking(move || crate::platform::scan(send)).await
            });
            while self.scanning.get_untracked() {
                tokio::select! {
                    value = receive.recv() => match value {
                        Some(Ok(crate::platform::Scan::Preview(bytes))) => {
                            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
                            self.preview.set(format!("data:image/png;base64,{encoded}"));
                        }
                        Some(Ok(crate::platform::Scan::Code(code))) => {
                            self.scanning.set(false);
                            self.preview.set(String::new());
                            self.status.set("已识别二维码，正在连接电脑…".into());
                            timer(std::time::Duration::from_millis(100)).await;
                            self.pair(code);
                            break;
                        }
                        Some(Err(error)) => {
                            self.status.set(error);
                            self.scanning.set(false);
                            break;
                        }
                        None => {
                            self.scanning.set(false);
                            break;
                        }
                    },
                    _ = timer(std::time::Duration::from_millis(200)) => {}
                }
            }
            drop(receive);
            let _ = worker.await;
        });
    }

    fn select_attachment(self) {
        if self.pending.get_untracked().is_some() {
            return;
        }
        let session = self.active.get_untracked();
        let device = self.device.get_untracked();
        self.spawn(async move {
            match background(crate::platform::pick_file()).await {
                Ok(Ok(Some(file))) => {
                    if self.active.get_untracked() != session
                        || self.device.get_untracked() != device
                    {
                        return;
                    }
                    if self
                        .attachments
                        .with_untracked(|files| {
                            files
                                .iter()
                                .any(|f| f.name == file.name)
                        })
                    {
                        self.status
                            .set("已选择同名附件，请先移除后再重新选择。".into());
                        return;
                    }
                    if self
                        .attachments
                        .with_untracked(|files| {
                            files.len() >= 8
                                || files
                                    .iter()
                                    .map(|f| f.bytes.len())
                                    .sum::<usize>()
                                    + file.bytes.len()
                                    > 20 * 1024 * 1024
                        })
                    {
                        self.status
                            .set("最多选择 8 个附件，总大小不超过 20 MiB。".into());
                    } else {
                        self.attachments
                            .update(|files| files.push(file));
                    }
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => self.status.set(error),
                Err(_) => self
                    .status
                    .set("无法打开文件选择器".into()),
            }
        });
    }
    fn command(self, command: Command) {
        if self.link.get_untracked() != LinkState::Connected {
            self.pending.set(None);
            self.status
                .set("正在恢复连接，请稍后重试。草稿已保留。".into());
            return;
        }
        let result = self.sender.with(|sender| {
            sender
                .as_ref()
                .map(|s| s.try_send(command))
        });
        if !matches!(result, Some(Ok(()))) {
            self.pending.set(None);
            self.status
                .set("连接尚未就绪，请重新连接电脑。".into());
        }
    }

    fn save(self) {
        self.save_revision
            .update(|revision| *revision += 1);
        let revision = self.save_revision.get_untracked();
        let contents = self.contents.get_untracked();
        self.spawn(async move {
            timer(std::time::Duration::from_millis(400)).await;
            if self.save_revision.get_untracked() != revision {
                return;
            }
            match blocking(move || vault::encode_private(&contents)).await {
                Ok(Ok(encoded)) => {
                    if self.save_revision.get_untracked() != revision {
                        return;
                    }
                    if Storage::set_item(VAULT_KEY, &encoded).is_err() {
                        self.status
                            .set("手机存储空间不足，连接和草稿未保存。".into());
                    }
                }
                Ok(Err(error)) => self.status.set(error.to_string()),
                Err(_) => self.status.set("无法保存连接。".into()),
            }
        });
    }

    fn disconnect(self) {
        self.connection_revision
            .update(|revision| *revision += 1);
        self.sender.replace(None);
        self.worker.with(|worker| {
            if let Some(worker) = worker.take() {
                worker.abort();
            }
        });
        self.link.set(LinkState::Disconnected);
        self.active.set(None);
        self.detail.set(None);
        self.attachments.set(Vec::new());
    }

    fn connect(self, pairing: Pairing) {
        self.disconnect();
        self.device.set(pairing.device.clone());
        self.pending.set(None);
        let revision = self.connection_revision.get_untracked();
        self.link.set(LinkState::Connecting);
        self.status.set("正在连接电脑…".into());
        let (sender, commands) = mpsc::channel(16);
        let (events, mut receiver) = mpsc::channel(64);
        self.sender.replace(Some(sender));
        self.spawn(async move {
            if self.connection_revision.get_untracked() != revision {
                return;
            }
            self.worker
                .replace(Some(background(remote::observe(pairing, commands, events))));
            while let Some(event) = receiver.recv().await {
                if self.connection_revision.get_untracked() != revision {
                    break;
                }
                match event {
                    Event::Connected {
                        workspace,
                        permissions,
                    } => {
                        self.contents.update(|contents| {
                            contents.set_workspace(&self.device.get_untracked(), workspace.clone());
                            contents.last_device = Some(self.device.get_untracked());
                        });
                        self.save();
                        self.workspace.set(workspace);
                        self.permissions.set(Some(permissions));
                        if !self.link.get_untracked().attached() {
                            self.panel.set(Panel::Closed);
                        }
                        self.link.set(LinkState::Connected);
                        self.status.set(String::new());
                        if self.active.get_untracked().is_none()
                            && let Some(id) = self.contents.with_untracked(|c| {
                                c.last_sessions
                                    .get(&self.device.get_untracked())
                                    .cloned()
                            })
                        {
                            self.command(Command::Open(Some(id)));
                        }
                    }
                    Event::Response(Response::Sessions(sessions)) => self.sessions.set(sessions),
                    Event::Response(Response::Opened(info)) => {
                        self.workspace
                            .set(info.workspace.clone());
                        self.inspector
                            .profile
                            .set(Some(info.clone()));
                        self.attachments.set(Vec::new());
                        self.conversation
                            .set(Conversation::default());
                        self.approvals.set(Vec::new());
                        self.questions.set(Vec::new());
                        let draft = self
                            .opening_draft
                            .get_untracked()
                            .unwrap_or_else(|| {
                                self.contents.with_untracked(|c| {
                                    c.draft(&self.device.get_untracked(), &info.id)
                                })
                            });
                        if self
                            .opening_draft
                            .get_untracked()
                            .is_some()
                        {
                            self.home_draft.set(String::new());
                            self.opening_draft.set(None);
                        }
                        self.draft.set(draft);
                        self.contents.update(|contents| {
                            contents
                                .last_sessions
                                .insert(self.device.get_untracked(), info.id.clone());
                        });
                        self.save();
                        if self.active.get_untracked().as_ref() != Some(&info.id) {
                            self.panel.set(Panel::Closed);
                        }
                        self.active.set(Some(info.id));
                    }
                    Event::Response(Response::Profile(info)) => {
                        self.inspector.profile.set(Some(*info))
                    }
                    Event::Response(Response::Error(error)) => {
                        self.pending.set(None);
                        self.inspector
                            .response
                            .set(std::sync::Arc::new(Response::Error(error.clone())));
                        self.status.set(error);
                    }
                    Event::Page { session, page } => {
                        if self.active.get_untracked().as_ref() == Some(&session) {
                            self.inspector
                                .planning
                                .set(page.planning.clone());
                            for update in &page.updates {
                                if let remote::protocol::Update::Profile(info) = update {
                                    self.inspector
                                        .profile
                                        .set(Some(*info.clone()));
                                }
                            }
                            self.conversation
                                .update(|view| view.apply(&page));
                            self.questions.set(
                                page.user_inputs
                                    .into_iter()
                                    .filter(|record| {
                                        record.status == remote::user_input::Status::Pending
                                    })
                                    .collect(),
                            );
                            self.approvals.set(
                                page.approvals
                                    .iter()
                                    .map(|a| {
                                        let (title, content) =
                                            remote::presentation::approval(&a.preview);
                                        (a.id, title, content)
                                    })
                                    .collect(),
                            );
                        }
                    }
                    Event::Submitted => {
                        self.attachments.set(Vec::new());
                        if self.pending.get_untracked().as_ref()
                            == Some(&self.draft.get_untracked())
                        {
                            self.draft.set(String::new());
                        }
                        self.pending.set(None);
                    }
                    Event::Disconnected(error) => {
                        if self.link.get_untracked().attached() {
                            self.link.set(LinkState::Recovering);
                        }
                        self.pending.set(None);
                        self.status.set(format!(
                            "正在恢复连接：{error}。草稿已保留，未确认的发送请恢复后核对。"
                        ));
                    }
                    Event::Response(response) => self
                        .inspector
                        .response
                        .set(std::sync::Arc::new(response)),
                }
            }
        });
    }
}

#[component]
pub fn App() -> impl IntoElement {
    let app = AppState {
        owner: Owner::current(),
        inspector: workspace::State::new(),
        panel: signal(Panel::Closed),
        status: signal(String::new()),
        link: signal(LinkState::Disconnected),
        workspace: signal(String::new()),
        device: signal(String::new()),
        permissions: signal(None),
        sessions: signal(Vec::new()),
        active: signal(None),
        conversation: signal(Conversation::default()),
        approvals: signal(Vec::new()),
        detail: signal(None),
        draft: signal(String::new()),
        home_draft: signal(String::new()),
        opening_draft: signal(None),
        pending: signal(None),
        scanning: signal(false),
        preview: signal(String::new()),
        pairing_busy: signal(false),
        attachments: signal(Vec::new()),
        about: signal(false),
        contents: signal(
            Storage::get_item(VAULT_KEY)
                .and_then(|encoded| vault::decode_private(&encoded).ok())
                .unwrap_or_default(),
        ),
        save_revision: signal(0),
        connection_revision: signal(0),
        questions: signal(Vec::new()),
        sender: stored(None),
        worker: stored(None),
    };
    cleanup(move || {
        app.worker.with(|worker| {
            if let Some(worker) = worker.take() {
                worker.abort();
            }
        })
    });
    effect(move || {
        let draft = app.draft.get();
        let active = app.active.get();
        if let Some(active) = active {
            app.contents.update(|contents| {
                contents.set_draft(&app.device.get_untracked(), &active, draft);
            });
            app.save();
        }
    });
    effect(move || {
        app.conversation.with(|conversation| {
            if let Some(selected) = app.detail.get_untracked()
                && let Some(current) = conversation
                    .items
                    .iter()
                    .find(|item| item.id == selected.id)
            {
                app.detail.set(Some(current.clone()));
            }
        });
    });
    if let Some(pairing) = app.contents.with_untracked(|c| {
        c.devices
            .iter()
            .find(|p| Some(&p.device) == c.last_device.as_ref())
            .cloned()
    }) {
        app.connect(pairing);
    }
    nav_stack()
        .flex_1()
        .child(
            nav_page()
                .key("home")
                .title("Orbit")
                .header_shown(false)
                .child(
                    view()
                        .flex_1()
                        .child(
                            when(move || app.link.get().attached(), move || home(app))
                                .otherwise(move || connection_home(app)),
                        )
                        .child(when(
                            move || app.link.get().attached() && app.panel.get() == Panel::Menu,
                            move || session_drawer(app),
                        )),
                ),
        )
        .child(when(
            move || app.link.get().attached() && app.panel.get() == Panel::Computers,
            move || {
                nav_page()
                    .key("computers")
                    .title("我的电脑")
                    .header(page_header("我的电脑", move || {
                        app.panel.set(Panel::Closed)
                    }))
                    .on_dismiss(move || app.panel.set(Panel::Closed))
                    .child(devices(app))
            },
        ))
        .child(when(
            move || app.link.get().attached() && app.panel.get() == Panel::Projects,
            move || {
                nav_page()
                    .key("projects")
                    .title("选择项目")
                    .header(page_header("选择项目", move || {
                        app.panel.set(Panel::Closed)
                    }))
                    .on_dismiss(move || app.panel.set(Panel::Closed))
                    .child(workspace::projects(app))
            },
        ))
        .child(when(
            move || app.link.get().attached() && app.panel.get() == Panel::Workspace,
            move || {
                nav_page()
                    .key("workspace")
                    .title("工作区")
                    .header(page_header("工作区", move || {
                        app.panel.set(Panel::Closed)
                    }))
                    .on_dismiss(move || app.panel.set(Panel::Closed))
                    .child(workspace::view(app))
            },
        ))
        .child(when(
            move || {
                app.link.get().attached()
                    && app.panel.get() == Panel::Workspace
                    && workspace::detail_open(app)
            },
            move || {
                nav_page()
                    .key("workspace-detail")
                    .title(workspace::title(app))
                    .header(page_header(workspace::title(app), move || {
                        workspace::close_detail(app)
                    }))
                    .on_dismiss(move || workspace::close_detail(app))
                    .child(workspace::view(app))
            },
        ))
        .child(when(
            move || app.panel.get() == Panel::Settings,
            move || {
                nav_page()
                    .key("settings")
                    .title("设置")
                    .header(page_header("设置", move || app.panel.set(Panel::Closed)))
                    .on_dismiss(move || app.panel.set(Panel::Closed))
                    .child(settings(app))
            },
        ))
        .child(when(
            move || app.detail.get().is_some(),
            move || {
                nav_page()
                    .key("tool")
                    .title("工具详情")
                    .header(page_header("工具详情", move || app.detail.set(None)))
                    .on_dismiss(move || app.detail.set(None))
                    .child(
                        scroll().flex_1().child(
                            view()
                                .p_4()
                                .gap_4()
                                .child(
                                    text(move || {
                                        app.detail.with(|i| {
                                            i.as_ref()
                                                .map(|i| i.title.clone())
                                                .unwrap_or_default()
                                        })
                                    })
                                    .text_size(px(20.0)),
                                )
                                .child(markdown(move || {
                                    app.detail.with(|i| {
                                        i.as_ref()
                                            .map(Item::details)
                                            .unwrap_or_default()
                                    })
                                }))
                                .child(button("复制输出", move || {
                                    if let Some(item) = app.detail.get_untracked() {
                                        set_clipboard(item.content);
                                    }
                                })),
                        ),
                    )
            },
        ))
        .child(when(move || app.about.get(), move || licenses(app)))
        .child(when(
            move || app.scanning.get(),
            move || {
                nav_page()
                    .key("scan")
                    .title("扫描电脑上的二维码")
                    .header(page_header("扫描二维码", move || {
                        app.scanning.set(false)
                    }))
                    .on_dismiss(move || app.scanning.set(false))
                    .child(
                        view()
                            .flex_1()
                            .w_full()
                            .p_4()
                            .gap_4()
                            .child(text("在电脑 Orbit 中打开设置 → 手机连接 → 配对手机").w_full())
                            .child(when(
                                move || !app.preview.get().is_empty(),
                                move || {
                                    image(move || nsh_core::ui::ImageSrc::from(app.preview.get()))
                                        .fade_duration(0.0)
                                        .w_full()
                                        .flex_1()
                                },
                            ))
                            .child(button("取消扫码", move || app.scanning.set(false))),
                    )
            },
        ))
}

fn connection_home(app: AppState) -> impl IntoElement {
    nsh_base::SafeAreaView::element(nsh_base::SafeAreaViewProps::default())
        .flex_1()
        .w_full()
        .bg(tokens()
            .color("background")
            .unwrap_or_default())
        .child(
            view()
                .flex_row()
                .items_center()
                .justify_between()
                .px_6()
                .py_3()
                .child(
                    text("Orbit")
                        .text_size(px(24.0))
                        .font_weight(FontWeight::MEDIUM),
                )
                .child(crate::components::icon_button(
                    "设置",
                    crate::components::Icon::Settings,
                    move || app.panel.set(Panel::Settings),
                )),
        )
        .child(devices(app))
}

fn home(app: AppState) -> impl IntoElement {
    use crate::components::{Icon, composer_field, icon, icon_button};
    nsh_base::SafeAreaView::element(nsh_base::SafeAreaViewProps::default())
        .flex_1()
        .w_full()
        .bg(tokens()
            .color("background")
            .unwrap_or_default())
        .child(
            view()
                .flex_row()
                .items_center()
                .px_4()
                .py_3()
                .gap_3()
                .child(icon_button("菜单", Icon::Menu, move || {
                    app.command(Command::Refresh);
                    app.panel.set(Panel::Menu)
                }))
                .child(
                    view()
                        .flex_1()
                        .items_center()
                        .gap_1()
                        .child(
                            text("Orbit")
                                .text_size(px(19.0))
                                .font_weight(FontWeight::MEDIUM),
                        )
                        .child(
                            text(move || {
                                if app.link.get() == LinkState::Recovering {
                                    "重连中"
                                } else if app.link.get().attached() {
                                    "已连接"
                                } else {
                                    "未连接"
                                }
                            })
                            .text_size(px(11.0))
                            .text_color(
                                tokens()
                                    .color("muted.foreground")
                                    .unwrap_or_default(),
                            ),
                        ),
                )
                .child(icon_button("工作区", Icon::Folder, move || {
                    app.panel
                        .set(if app.active.get_untracked().is_some() {
                            Panel::Workspace
                        } else {
                            Panel::Projects
                        })
                })),
        )
        .child(
            when(
                move || app.active.get().is_some(),
                move || conversation(app),
            )
            .otherwise(move || {
                keyboard_avoiding_view()
                    .flex_1()
                    .w_full()
                    .child(banner(app))
                    .child(
                        view()
                            .flex_1()
                            .w_full()
                            .items_center()
                            .justify_center()
                            .gap_4()
                            .p_6()
                            .child(
                                image(format!(
                                    "data:image/png;base64,{}",
                                    base64::engine::general_purpose::STANDARD.encode(
                                        include_bytes!("../../assets/branding/orbit-128.png")
                                    )
                                ))
                                .w(px(52.0))
                                .h(px(52.0)),
                            )
                            .child(
                                text("从这里，继续你的工作")
                                    .text_size(px(22.0))
                                    .font_weight(FontWeight::MEDIUM),
                            )
                            .child(
                                text("电脑执行，手机随行。")
                                    .text_size(px(14.0))
                                    .text_color(
                                        tokens()
                                            .color("muted.foreground")
                                            .unwrap_or_default(),
                                    ),
                            ),
                    )
                    .child(
                        view()
                            .px_4()
                            .pb_4()
                            .gap_3()
                            .child(
                                view()
                                    .w_full()
                                    .p_3()
                                    .gap_2()
                                    .rounded(px(26.0))
                                    .border_1()
                                    .border_color(
                                        tokens()
                                            .color("border")
                                            .unwrap_or_default(),
                                    )
                                    .bg(tokens()
                                        .color("background")
                                        .unwrap_or_default())
                                    .child(composer_field(app.home_draft))
                                    .child(
                                        view()
                                            .flex_row()
                                            .items_center()
                                            .gap_2()
                                            .child(icon_button(
                                                "添加附件",
                                                Icon::Add,
                                                move || {
                                                    app.panel.set(Panel::Computers);
                                                },
                                            ))
                                            .child(
                                                nsh_base::Button::element(nsh_base::ButtonProps {
                                                    accessibility_label: Some(SharedString::from(
                                                        "连接电脑",
                                                    ))
                                                    .into(),
                                                    ..Default::default()
                                                })
                                                .on_event(move |event: &nsh_base::ButtonEvent| {
                                                    if matches!(event, nsh_base::ButtonEvent::Press)
                                                    {
                                                        app.panel.set(Panel::Computers);
                                                    }
                                                })
                                                .flex_row()
                                                .items_center()
                                                .gap_2()
                                                .h(px(40.0))
                                                .px_3()
                                                .rounded(px(20.0))
                                                .bg(tokens()
                                                    .color("muted.background")
                                                    .unwrap_or_default())
                                                .child(icon(Icon::Computer, 16.0))
                                                .child(
                                                    text(move || {
                                                        if app.link.get().attached() {
                                                            "切换电脑"
                                                        } else {
                                                            "连接电脑"
                                                        }
                                                    })
                                                    .text_size(px(13.0)),
                                                ),
                                            )
                                            .child(view().flex_1())
                                            .child(icon_button(
                                                "开始会话",
                                                Icon::Send,
                                                move || {
                                                    if app.link.get_untracked().attached() {
                                                        app.opening_draft.set(Some(
                                                            app.home_draft.get_untracked(),
                                                        ));
                                                        app.command(Command::Open(None));
                                                    } else {
                                                        app.panel.set(Panel::Computers);
                                                    }
                                                },
                                            )),
                                    ),
                            )
                            .child(
                                text("任务在电脑上运行，离开手机也会继续。")
                                    .w_full()
                                    .text_center()
                                    .text_size(px(11.0))
                                    .text_color(
                                        tokens()
                                            .color("muted.foreground")
                                            .unwrap_or_default(),
                                    ),
                            ),
                    )
            }),
        )
}

fn menu(app: AppState) -> impl IntoElement {
    when(move || app.link.get().attached(), move || sessions(app)).otherwise(move || {
        view()
            .flex_1()
            .p_6()
            .child(text("连接电脑后，这里会显示项目中的历史会话。"))
    })
}

fn settings(app: AppState) -> impl IntoElement {
    scroll().flex_1().w_full().child(
        view()
            .w_full()
            .p_6()
            .gap_4()
            .child(
                text("连接与安全")
                    .text_size(px(18.0))
                    .font_weight(FontWeight::MEDIUM),
            )
            .child(button("管理电脑", move || {
                app.panel.set(Panel::Computers)
            }))
            .child(when(
                move || app.link.get().attached(),
                move || {
                    button("断开当前连接", move || {
                        app.contents
                            .update(|c| c.last_device = None);
                        app.save();
                        app.disconnect();
                    })
                },
            ))
            .child(
                text("工作区中可切换模型、审批模式和管理任务。关闭手机不会停止电脑上的任务。")
                    .w_full()
                    .text_size(px(14.0))
                    .text_color(
                        tokens()
                            .color("muted.foreground")
                            .unwrap_or_default(),
                    ),
            )
            .child(
                text("关于")
                    .text_size(px(18.0))
                    .font_weight(FontWeight::MEDIUM),
            )
            .child(text("Orbit · 0.1.0").w_full())
            .child(button("开源许可", move || app.about.set(true))),
    )
}

fn banner(app: AppState) -> impl IntoElement {
    when(
        move || !app.status.get().is_empty(),
        move || {
            view()
                .p_4()
                .gap_2()
                .bg(tokens()
                    .color("secondary.background")
                    .unwrap_or_default())
                .child(
                    text(move || app.status.get()).text_color(
                        tokens()
                            .color("foreground")
                            .unwrap_or_default(),
                    ),
                )
        },
    )
}

fn devices(app: AppState) -> impl IntoElement {
    keyboard_avoiding_view()
        .flex_1()
        .w_full()
        .bg(tokens()
            .color("background")
            .unwrap_or_default())
        .child(
            scroll().flex_1().w_full().child(
                view()
                    .w_full()
                    .p_6()
                    .gap_6()
                    .child(
                        view()
                            .w_full()
                            .gap_3()
                            .child(
                                text("连接你的电脑")
                                    .w_full()
                                    .text_size(px(28.0))
                                    .font_weight(FontWeight::MEDIUM),
                            )
                            .child(
                                text(
                                    "在手机上继续会话、查看结果和发送文件。任务始终在电脑上运行。",
                                )
                                .w_full()
                                .text_size(px(16.0))
                                .text_color(
                                    tokens()
                                        .color("muted.foreground")
                                        .unwrap_or_default(),
                                ),
                            ),
                    )
                    .child(banner(app))
                    .child(primary_button("扫描配对二维码", move || app.scan()))
                    .child(
                        text("在电脑 Orbit 设置中开启手机连接并显示二维码。配对一次，即可在手机上切换这台电脑的项目。")
                            .w_full()
                            .text_size(px(14.0))
                            .text_color(
                                tokens()
                                    .color("muted.foreground")
                                    .unwrap_or_default(),
                            ),
                    )
                    .child(when(
                        move || !app.contents.with(|c| c.devices.is_empty()),
                        move || {
                            view().w_full().gap_4().child(each(
                                move || app.contents.with(|c| c.devices.clone()),
                                |p: &Pairing| p.device.clone(),
                                move |pairing, index| {
                                    view()
                                        .w_full()
                                        .gap_3()
                                        .py_4()
                                        .child(
                                            text(move || { let name = pairing.get().host_name; if name.is_empty() { format!("我的电脑 {}", index.get() + 1) } else { name } })
                                                .w_full()
                                                .text_size(px(18.0)),
                                        )
                                        .child(button("连接电脑", move || {
                                            app.connect(pairing.get_untracked())
                                        }))
                                        .child(button("移除连接", move || {
                                            let id = pairing.get_untracked().device;
                                            app.contents
                                                .update(|c| c.devices.retain(|p| p.device != id));
                                            app.save();
                                        }))
                                },
                            ))
                        },
                    )),
            ),
        )
}

fn sessions(app: AppState) -> impl IntoElement {
    let search = signal(String::new());
    let expanded = signal(None::<std::collections::BTreeSet<String>>);
    view()
        .flex_1()
        .min_h(px(0.0))
        .child(
            view()
                .px_4()
                .gap_2()
                .child(button("新任务", move || {
                    app.active.set(None);
                    app.panel.set(Panel::Closed);
                }))
                .child(field(search, "搜索会话")),
        )
        .child(
            scroll()
                .flex_1()
                .child(dynamic(move || {
                    let mut body = view().p_4().gap_2();
                    let sessions = app.sessions.get();
                    let paths: std::collections::BTreeSet<_> = sessions
                        .iter()
                        .map(|s| s.workspace.clone())
                        .collect();
                    let projects = app.contents.with(|c| {
                        c.projects
                            .get(&app.device.get())
                            .cloned()
                            .unwrap_or_default()
                    });
                    let mut paths: Vec<_> = paths.into_iter().collect();
                    paths.sort_by_key(|path| {
                        (
                            !projects.is_pinned(path),
                            projects.position(path),
                            path.clone(),
                        )
                    });
                    let query = search.get().to_lowercase();
                    let mut section = None;
                    for path in paths {
                        let items: Vec<_> = sessions
                            .iter()
                            .filter(|s| {
                                s.workspace == path && s.title.to_lowercase().contains(&query)
                            })
                            .cloned()
                            .collect();
                        if items.is_empty() {
                            continue;
                        }
                        let pinned = projects.is_pinned(&path);
                        if section != Some(pinned) {
                            body = body.child(
                                text(if pinned { "置顶" } else { "项目" }).text_size(px(13.0)),
                            );
                            section = Some(pinned);
                        }
                        let pin_path = path.clone();
                        let name = path
                            .rsplit(['/', '\\'])
                            .find(|s| !s.is_empty())
                            .unwrap_or(&path)
                            .to_owned();
                        let is_open = expanded.with(|set| {
                            set.as_ref()
                                .map_or(path == app.workspace.get(), |set| set.contains(&path))
                        }) || !query.is_empty();
                        let toggle = path.clone();
                        body = body.child(
                            view()
                                .min_h(px(44.0))
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .py_2()
                                .label(format!("项目 {name}"))
                                .on_press(move || {
                                    expanded.update(|value| {
                                        let set = value.get_or_insert_with(|| {
                                            std::collections::BTreeSet::from([app
                                                .workspace
                                                .get_untracked()])
                                        });
                                        if !set.remove(&toggle) {
                                            set.insert(toggle.clone());
                                        }
                                    })
                                })
                                .child(crate::components::icon(
                                    if is_open {
                                        crate::components::Icon::FolderOpen
                                    } else {
                                        crate::components::Icon::Folder
                                    },
                                    18.0,
                                ))
                                .child(text(name).flex_1())
                                .child(crate::components::icon_button(
                                    if pinned {
                                        "取消置顶"
                                    } else {
                                        "置顶项目"
                                    },
                                    crate::components::Icon::Pin,
                                    move || {
                                        app.contents.update(|c| {
                                            c.projects
                                                .entry(app.device.get_untracked())
                                                .or_default()
                                                .set_pin(&pin_path, !pinned)
                                        });
                                        app.save();
                                    },
                                )),
                        );
                        if is_open {
                            for item in items {
                                body = body.child(
                                    view()
                                        .min_h(px(44.0))
                                        .pl_6()
                                        .py_2()
                                        .label(item.title.clone())
                                        .on_press(move || {
                                            app.command(Command::Open(Some(item.id.clone())))
                                        })
                                        .child(text(item.title).text_size(px(15.0))),
                                );
                            }
                        }
                    }
                    body
                })),
        )
}

fn session_drawer(app: AppState) -> impl IntoElement {
    let offset = animated_value(-untracked(window).width.0 * 0.84);
    offset.spring(0.0, Spring::default());
    let close = move || app.panel.set(Panel::Closed);
    nsh_base::dialog()
        .animation(ModalAnimation::None)
        .close_on_escape(false)
        .close_on_backdrop_press(false)
        .close_on_swipe(false)
        .on_cancel(close)
        .child(
            view()
                .absolute()
                .inset_0()
                .bg(nsh_core::ui::Color::rgba(0, 0, 0, 102))
                .label("关闭会话菜单")
                .focusable(false)
                .on_press(close),
        )
        .child(
            view()
                .absolute()
                .left_0()
                .top_0()
                .bottom(keyboard_height)
                .w(move || px((window().width.0 * 0.84).min(360.0)))
                .pt(move || px(safe_area().top.0 + 12.0))
                .pb(move || px(safe_area().bottom.0 + 12.0))
                .bg(tokens()
                    .color("background")
                    .unwrap_or_default())
                .bind_translate_x(&offset)
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .px_4()
                        .pb_4()
                        .child(
                            text("Orbit")
                                .text_size(px(24.0))
                                .font_weight(FontWeight::MEDIUM),
                        )
                        .child(crate::components::icon_button(
                            "关闭菜单",
                            crate::components::Icon::Close,
                            close,
                        )),
                )
                .child(menu(app))
                .child(
                    view().px_4().pt_2().child(
                        view()
                            .h(px(44.0))
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .label("设置")
                            .on_press(move || app.panel.set(Panel::Settings))
                            .child(crate::components::icon(
                                crate::components::Icon::Settings,
                                20.0,
                            ))
                            .child(text("设置")),
                    ),
                ),
        )
}

fn conversation(app: AppState) -> impl IntoElement {
    let viewport = element_ref();
    let following = signal(true);
    effect(move || {
        app.conversation.with(|c| {
            let _ = c.items.len();
        });
        if following.get_untracked() {
            spawn(async move {
                next_frame().await;
                viewport.scroll_to_end(false);
            });
        }
    });
    keyboard_avoiding_view()
        .flex_1()
        .bg(tokens()
            .color("background")
            .unwrap_or_default())
        .child(banner(app))
        .child(
            scroll()
                .element_ref(&viewport)
                .on_scroll(move |event: &nsh_core::ui::ScrollEvent| {
                    following.set(
                        event.content_size.height.0 - event.offset.y.0 - event.size.height.0 < 64.0,
                    );
                })
                .flex_1()
                .child(
                    view()
                        .p_4()
                        .gap_4()
                        .child(each(
                            move || {
                                app.conversation
                                    .with(|c| c.items.clone())
                            },
                            |i: &Item| i.id.clone(),
                            move |item, _| {
                                let kind = item.get_untracked().kind;
                                if kind == Kind::Tool {
                                    view()
                                        .p_3()
                                        .gap_2()
                                        .rounded(px(12.0))
                                        .bg(tokens()
                                            .color("secondary.background")
                                            .unwrap_or_default())
                                        .child(text(move || {
                                            item.with(|i| format!("{} · {}", i.title, i.state))
                                        }))
                                        .label("查看输出")
                                        .on_press(move || {
                                            app.detail
                                                .set(Some(item.get_untracked()))
                                        })
                                        .child(text("查看输出 ›").text_size(px(13.0)))
                                        .into_element()
                                } else if kind == Kind::Thought {
                                    let expanded = signal(false);
                                    view()
                                        .gap_2()
                                        .child(
                                            view()
                                                .min_h(px(40.0))
                                                .w_full()
                                                .label("思考过程")
                                                .on_press(move || {
                                                    expanded.set(!expanded.get_untracked())
                                                })
                                                .child(
                                                    text(move || {
                                                        if expanded.get() {
                                                            "思考过程 ⌄"
                                                        } else {
                                                            "思考过程 ›"
                                                        }
                                                    })
                                                    .text_size(px(14.0)),
                                                ),
                                        )
                                        .child(when(
                                            move || expanded.get(),
                                            move || {
                                                markdown(move || item.with(|i| i.content.clone()))
                                            },
                                        ))
                                        .into_element()
                                } else if kind == Kind::User {
                                    view()
                                        .w_full()
                                        .items_end()
                                        .child(
                                            view()
                                                .max_w(move || px(window().width.0 * 0.82))
                                                .p_3()
                                                .rounded(px(18.0))
                                                .bg(tokens()
                                                    .color("secondary.background")
                                                    .unwrap_or_default())
                                                .child(markdown(move || {
                                                    item.with(|i| i.content.clone())
                                                })),
                                        )
                                        .into_element()
                                } else {
                                    view()
                                        .w_full()
                                        .child(markdown(move || item.with(|i| i.content.clone())))
                                        .into_element()
                                }
                            },
                        ))
                        .child(each(
                            move || app.approvals.get(),
                            |a: &(u64, String, String)| a.0,
                            move |approval, _| {
                                view()
                                    .p_4()
                                    .gap_3()
                                    .rounded(px(16.0))
                                    .bg(tokens()
                                        .color("secondary.background")
                                        .unwrap_or_default())
                                    .child(text(move || approval.with(|a| a.1.clone())))
                                    .child(markdown(move || approval.with(|a| a.2.clone())))
                                    .child(when(
                                        move || {
                                            app.permissions
                                                .with(|p| p.is_some_and(|p| p.approve))
                                        },
                                        move || {
                                            view()
                                                .flex_row()
                                                .gap_3()
                                                .child(button("拒绝", move || {
                                                    app.command(Command::Approve {
                                                        id: approval.get_untracked().0,
                                                        allow: false,
                                                    })
                                                }))
                                                .child(button("允许本次", move || {
                                                    app.command(Command::Approve {
                                                        id: approval.get_untracked().0,
                                                        allow: true,
                                                    })
                                                }))
                                        },
                                    ))
                            },
                        ))
                        .child(each(
                            move || app.questions.get(),
                            |record: &remote::user_input::Record| record.id.clone(),
                            move |record, _| question_form(app, record.get_untracked()),
                        )),
                ),
        )
        .child(when(
            move || !following.get(),
            move || {
                view()
                    .w_full()
                    .items_center()
                    .py_1()
                    .child(crate::components::icon_button(
                        "回到最新",
                        crate::components::Icon::Down,
                        move || {
                            following.set(true);
                            viewport.scroll_to_end(true);
                        },
                    ))
            },
        ))
        .child(
            view()
                .mx_4()
                .mb_4()
                .p_3()
                .gap_3()
                .rounded(px(26.0))
                .border_1()
                .border_color(
                    tokens()
                        .color("border")
                        .unwrap_or_default(),
                )
                .child(crate::components::composer_field(app.draft))
                .child(when(
                    move || {
                        app.permissions
                            .with(|p| p.is_some_and(|p| p.control))
                    },
                    move || {
                        view()
                            .w_full()
                            .gap_2()
                            .child(each(
                                move || app.attachments.get(),
                                |file: &remote::Attachment| file.name.clone(),
                                move |file, _| {
                                    text(move || {
                                        file.with(|file| {
                                            format!(
                                                "{} · {} KB",
                                                file.name,
                                                file.bytes.len().div_ceil(1024)
                                            )
                                        })
                                    })
                                    .w_full()
                                },
                            ))
                            .child(when(
                                move || !app.attachments.get().is_empty(),
                                move || {
                                    button("移除附件", move || {
                                        if app.pending.get_untracked().is_none() {
                                            app.attachments.set(Vec::new());
                                        }
                                    })
                                },
                            ))
                    },
                ))
                .child(when(
                    move || app.pending.get().is_some(),
                    || text("正在等待电脑确认…"),
                ))
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .gap_3()
                        .child(when(
                            move || {
                                app.permissions
                                    .with(|p| p.is_some_and(|p| p.control))
                            },
                            move || {
                                crate::components::icon_button(
                                    "添加附件",
                                    crate::components::Icon::Add,
                                    move || app.select_attachment(),
                                )
                            },
                        ))
                        .child(
                            text(move || {
                                app.conversation
                                    .with(|conversation| conversation.model.clone())
                            })
                            .flex_1()
                            .text_size(px(12.0))
                            .text_color(
                                tokens()
                                    .color("muted.foreground")
                                    .unwrap_or_default(),
                            ),
                        )
                        .child(
                            when(
                                move || {
                                    app.permissions
                                        .with(|p| p.is_some_and(|p| p.control))
                                },
                                move || {
                                    when(
                                        move || app.conversation.with(|c| c.busy),
                                        move || {
                                            crate::components::toolbar_button("停止", move || {
                                                app.command(Command::Stop)
                                            })
                                        },
                                    )
                                    .otherwise(move || {
                                        crate::components::icon_button(
                                            "发送",
                                            crate::components::Icon::Send,
                                            move || {
                                                let text = app.draft.get_untracked();
                                                if (!text.trim().is_empty()
                                                    || !app
                                                        .attachments
                                                        .get_untracked()
                                                        .is_empty())
                                                    && app.pending.get_untracked().is_none()
                                                {
                                                    app.pending.set(Some(text.clone()));
                                                    app.command(Command::SendAttachments {
                                                        request_id: remote::request_id(),
                                                        text,
                                                        attachments: app
                                                            .attachments
                                                            .get_untracked(),
                                                    });
                                                }
                                            },
                                        )
                                    })
                                },
                            )
                            .otherwise(|| text("此设备仅有查看权限")),
                        ),
                ),
        )
}

fn question_form(app: AppState, record: remote::user_input::Record) -> impl IntoElement {
    use remote::user_input::{Answer, Kind};
    let answers = signal(
        record
            .form
            .questions
            .iter()
            .map(|q| Answer {
                question_id: q.id.clone(),
                ..Default::default()
            })
            .collect::<Vec<_>>(),
    );
    let form = record.form.clone();
    let request = record.id.clone();
    let decline = record.id.clone();
    view()
        .p_4()
        .gap_3()
        .rounded(px(16.0))
        .bg(tokens()
            .color("secondary.background")
            .unwrap_or_default())
        .child(text("需要你的回答").text_size(px(18.0)))
        .child(each(
            move || form.questions.clone(),
            |q: &remote::user_input::Question| q.id.clone(),
            move |question, index| {
                let question = question.get_untracked();
                let position = index.get_untracked();
                let custom = signal(String::new());
                let kind = question.kind;
                effect(move || {
                    let value = custom.get();
                    answers.update(|answers| {
                        answers[position].text = value;
                        if kind == Kind::Single && !answers[position].text.is_empty() {
                            answers[position].selected.clear();
                        }
                    });
                });
                view()
                    .gap_2()
                    .child(text(question.prompt))
                    .child(each(
                        move || question.options.clone(),
                        |option: &remote::user_input::OptionItem| option.id.clone(),
                        move |option, _| {
                            let id = option.get_untracked().id;
                            let choice = id.clone();
                            nsh_base::Button::element(nsh_base::ButtonProps::default())
                                .on_event(move |event: &nsh_base::ButtonEvent| {
                                    if matches!(event, nsh_base::ButtonEvent::Press) {
                                        answers.update(|answers| {
                                            let selected = &mut answers[position].selected;
                                            if selected.contains(&choice) {
                                                selected.retain(|s| s != &choice);
                                            } else {
                                                if kind == Kind::Single {
                                                    selected.clear();
                                                }
                                                selected.push(choice.clone());
                                            }
                                        });
                                        if kind == Kind::Single {
                                            custom.set(String::new());
                                        }
                                    }
                                })
                                .min_h(px(48.0))
                                .p_3()
                                .border_1()
                                .border_color(
                                    tokens()
                                        .color("border")
                                        .unwrap_or_default(),
                                )
                                .child(text(move || {
                                    format!(
                                        "{} {}",
                                        if answers.with(|a| a[position].selected.contains(&id)) {
                                            "●"
                                        } else {
                                            "○"
                                        },
                                        option.with(|o| o.label.clone())
                                    )
                                }))
                                .child(text(move || option.with(|o| o.description.clone())))
                        },
                    ))
                    .child(when(
                        move || question.allow_custom || kind == Kind::Text,
                        move || field(custom, "填写回答").multiline(true),
                    ))
            },
        ))
        .child(when(
            move || {
                app.permissions
                    .with(|p| p.is_some_and(|p| p.control))
            },
            move || {
                let record = record.clone();
                let request = request.clone();
                let decline = decline.clone();
                view()
                    .flex_row()
                    .gap_3()
                    .child(button("提交回答", move || {
                        let value = answers.get_untracked();
                        match record.form.validate_answers(&value) {
                            Ok(()) => app.command(Command::Answer {
                                id: request.clone(),
                                answers: Some(value),
                            }),
                            Err(error) => app.status.set(error),
                        }
                    }))
                    .child(button("暂不回答", move || {
                        app.command(Command::Answer {
                            id: decline.clone(),
                            answers: None,
                        })
                    }))
            },
        ))
}

fn licenses(app: AppState) -> impl IntoElement {
    nav_page()
        .key("licenses")
        .title("开源许可")
        .header(page_header("开源许可", move || app.about.set(false)))
        .on_dismiss(move || app.about.set(false))
        .child(
            scroll().flex_1().child(
                view()
                    .w_full()
                    .p_4()
                    .gap_4()
                    .child(text(concat!(
                        "NSH · Apache-2.0\nhttps://github.com/longbridge/nsh\n",
                        "固定提交 d5839d7a302e2061d54c58090588f0f953e366d0"
                    )))
                    .child(text(include_str!("../licenses/NSH-APACHE-2.0.txt")))
                    .child(text("iroh · BSD-3-Clause · https://iroh.computer"))
                    .child(text(include_str!("../licenses/IROH-BSD-3-Clause.txt")))
                    .child(text("rqrr · MIT / ISC · https://github.com/WanzenBug/rqrr"))
                    .child(text(include_str!("../licenses/RQRR-MIT.txt")))
                    .child(text(include_str!("../licenses/RQRR-ISC.txt")))
                    .child(text(
                        "qrcode · MIT · https://github.com/kennytm/qrcode-rust",
                    ))
                    .child(text(include_str!("../licenses/QRCODE-MIT.txt"))),
            ),
        )
}
