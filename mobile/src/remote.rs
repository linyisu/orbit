//! Android remote-controller view; transport and polling belong to orbit-client.

use gpui_kit::{
    component::{
        ActiveTheme, Disableable, Sizable, StyledExt,
        button::{Button, ButtonVariants},
        input::{Textarea, TextareaState},
    },
    prelude::*,
    *,
};
use orbit_client::remote::{self, Command, Event, Response};
use std::sync::Arc;

pub struct Remote {
    pairing: Entity<TextareaState>,
    pairing_data: Option<remote::Pairing>,
    draft: Entity<TextareaState>,
    runtime: Arc<tokio::runtime::Runtime>,
    sender: Option<tokio::sync::mpsc::Sender<Command>>,
    worker: Option<tokio::task::JoinHandle<()>>,
    receive: Task<()>,
    status: String,
    sessions: Vec<remote::Session>,
    active: Option<remote::SessionInfo>,
    entries: Vec<String>,
    approvals: Vec<(u64, String)>,
    sending: Option<String>,
    control: bool,
    approve: bool,
    scroll: ScrollHandle,
    _activation: Subscription,
}

impl Remote {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            pairing: cx
                .new(|cx| TextareaState::new(window, cx).placeholder("粘贴电脑生成的设备连接资料")),
            pairing_data: None,
            draft: cx
                .new(|cx| TextareaState::new(window, cx).placeholder("向电脑上的 Orbit 发送消息")),
            runtime: Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("mobile network runtime"),
            ),
            sender: None,
            worker: None,
            receive: Task::ready(()),
            status: "连接资料只在本次运行中使用，请勿公开分享。".into(),
            sessions: Vec::new(),
            active: None,
            entries: Vec::new(),
            approvals: Vec::new(),
            sending: None,
            control: false,
            approve: false,
            scroll: ScrollHandle::new(),
            _activation: cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() && this.sender.is_some() {
                    this.sender = None;
                    if let Some(worker) = this.worker.take() {
                        worker.abort();
                    }
                    this.status =
                        "已暂停手机连接，电脑任务继续运行。返回后请重新连接并核对状态。".into();
                    cx.notify();
                }
            }),
        }
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.pairing.read(cx).value();
        let pairing = if text.trim().is_empty() {
            self.pairing_data.clone()
        } else {
            serde_json::from_str::<remote::Pairing>(&text).ok()
        };
        let pairing = match pairing {
            Some(pairing) => pairing,
            None => {
                self.status = "连接资料格式无效，请完整复制电脑生成的内容。".into();
                cx.notify();
                return;
            }
        };
        self.disconnect(cx);
        self.pairing_data = Some(pairing.clone());
        self.status = "正在验证电脑身份…".into();
        let (sender, commands) = tokio::sync::mpsc::channel(8);
        let (events, mut receiver) = tokio::sync::mpsc::channel(8);
        self.sender = Some(sender);
        self.worker = Some(
            self.runtime
                .spawn(remote::observe(pairing, commands, events)),
        );
        self.receive = cx.spawn(async move |weak, cx| {
            while let Some(event) = receiver.recv().await {
                if weak
                    .update_in(cx, |view, window, cx| view.event(event, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.pairing
            .update(cx, |input, cx| input.set_value("", window, cx));
        window.blur(cx);
        cx.notify();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.sender = None;
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
        self.receive = Task::ready(());
        self.active = None;
        self.sessions.clear();
        self.approvals.clear();
        self.status = "已断开，电脑上的任务可能仍在运行。".into();
        cx.notify();
    }

    fn send(&mut self, command: Command, cx: &mut Context<Self>) -> bool {
        let result = self
            .sender
            .as_ref()
            .is_some_and(|sender| sender.try_send(command).is_ok());
        if !result {
            self.status = "连接不可用，操作尚未发送。".into();
        }
        cx.notify();
        result
    }

    fn event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            Event::Connected {
                workspace,
                permissions,
            } => {
                self.status = format!("已连接 · {workspace}");
                self.control = permissions.control;
                self.approve = permissions.approve;
                self.sending = None;
            }
            Event::Disconnected(message) => {
                self.sender = None;
                self.status =
                    format!("连接已断开：{message}。发送结果可能尚未确认，请先核对电脑会话。");
            }
            Event::Response(Response::Sessions(sessions)) => self.sessions = sessions,
            Event::Response(Response::Opened(info)) => {
                self.active = Some(info);
                self.entries.clear();
                self.approvals.clear();
                self.scroll = ScrollHandle::new();
            }
            Event::Page { session, page } => {
                if self
                    .active
                    .as_ref()
                    .is_none_or(|active| active.id != session)
                {
                    return;
                }
                for update in page.updates {
                    match update {
                        remote::Update::Started { prompt, .. } => self
                            .entries
                            .push(format!("你：{prompt}")),
                        remote::Update::Profile(info) => self.active = Some(*info),
                        remote::Update::Report(report) => self.entries.push(report.text),
                        remote::Update::Finished { status, .. } => self
                            .entries
                            .push(format!("任务状态：{status}")),
                        remote::Update::Agent(event) => self
                            .entries
                            .push(serde_json::to_string_pretty(&event).unwrap_or_default()),
                    }
                }
                self.approvals = page
                    .approvals
                    .into_iter()
                    .map(|approval| {
                        (
                            approval.id,
                            serde_json::to_string_pretty(&approval.preview).unwrap_or_default(),
                        )
                    })
                    .collect();
                const MAX_VISIBLE_EVENTS: usize = 256;
                const MAX_EVENT_CHARS: usize = 16_384;
                for entry in &mut self.entries {
                    if let Some((offset, _)) = entry
                        .char_indices()
                        .nth(MAX_EVENT_CHARS)
                    {
                        entry.truncate(offset);
                        entry.push_str("\n…预览已截断");
                    }
                }
                if self.entries.len() > MAX_VISIBLE_EVENTS {
                    self.entries
                        .drain(..self.entries.len() - MAX_VISIBLE_EVENTS);
                }
            }
            Event::Submitted => {
                if self
                    .sending
                    .take()
                    .is_some_and(|sent| self.draft.read(cx).value().as_str() == sent)
                {
                    self.draft
                        .update(cx, |input, cx| input.set_value("", window, cx));
                }
            }
            Event::Response(Response::Error(error)) => {
                self.status = error;
                self.sending = None;
            }
            Event::Response(_) => {}
        }
        cx.notify();
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}

impl Render for Remote {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut body = div().v_flex().gap_3();
        if self.sender.is_none() {
            body = body
                .child(Textarea::new(&self.pairing).aria_label("设备连接资料"))
                .child(
                    Button::new("connect")
                        .label(if self.pairing_data.is_some() {
                            "重新连接"
                        } else {
                            "连接电脑"
                        })
                        .large()
                        .on_click(cx.listener(|this, _, window, cx| this.connect(window, cx))),
                );
        } else if self.active.is_none() {
            body = body.child(
                Button::new("new-session")
                    .label("新会话")
                    .large()
                    .disabled(!self.control)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.send(Command::Open(None), cx);
                    })),
            );
            for session in &self.sessions {
                let id = session.id.clone();
                body = body.child(
                    Button::new(SharedString::from(format!("session-{id}")))
                        .label(session.title.clone())
                        .large()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.send(Command::Open(Some(id.clone())), cx);
                        })),
                );
            }
        } else {
            for (index, entry) in self.entries.iter().enumerate() {
                // The first transport integration preserves complete events for inspection.
                body = body.child(
                    div()
                        .id(index)
                        .p_3()
                        .bg(cx.theme().muted)
                        .child(entry.clone()),
                );
            }
            for (id, preview) in &self.approvals {
                let id = *id;
                body = body.child(
                    div()
                        .v_flex()
                        .gap_2()
                        .child(preview.clone())
                        .child(
                            Button::new(SharedString::from(format!("reject-{id}")))
                                .label("拒绝")
                                .large()
                                .disabled(!self.approve)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.send(Command::Approve { id, allow: false }, cx);
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("allow-{id}")))
                                .label("允许此次")
                                .large()
                                .disabled(!self.approve)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.send(Command::Approve { id, allow: true }, cx);
                                })),
                        ),
                );
            }
        }
        div()
            .size_full()
            .v_flex()
            .map(|view| {
                #[cfg(target_os = "android")]
                if let Some(window) = gpui_mobile::android::jni::platform()
                    .and_then(|platform| platform.primary_window())
                {
                    let insets = window.safe_area_insets_logical();
                    return view
                        .pt(px(insets.top))
                        .pb(px(insets.bottom))
                        .pl(px(insets.left))
                        .pr(px(insets.right));
                }
                view
            })
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                div()
                    .h_flex()
                    .p_4()
                    .gap_3()
                    .child("Orbit 远控")
                    .when(self.sender.is_some(), |view| {
                        view.child(
                            Button::new("disconnect")
                                .label("断开")
                                .ghost()
                                .on_click(cx.listener(|this, _, _, cx| this.disconnect(cx))),
                        )
                    }),
            )
            .child(
                div()
                    .px_4()
                    .pb_3()
                    .child(self.status.clone()),
            )
            .child(
                div()
                    .id("remote-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(div().p_4().child(body)),
            )
            .when(self.active.is_some(), |view| {
                view.child(
                    div()
                        .v_flex()
                        .p_4()
                        .gap_2()
                        .child(Textarea::new(&self.draft).aria_label("消息"))
                        .child(
                            Button::new("send")
                                .label("发送")
                                .large()
                                .disabled(
                                    !self.control
                                        || self.sending.is_some()
                                        || self.sender.is_none(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let text = this.draft.read(cx).value().to_string();
                                    if !text.trim().is_empty()
                                        && this.send(
                                            Command::Send {
                                                request_id: remote::request_id(),
                                                text: text.clone(),
                                            },
                                            cx,
                                        )
                                    {
                                        this.sending = Some(text);
                                    }
                                })),
                        )
                        .child(
                            Button::new("stop")
                                .label("停止当前响应")
                                .large()
                                .disabled(!self.control || self.sender.is_none())
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.send(Command::Stop, cx);
                                })),
                        ),
                )
            })
    }
}
