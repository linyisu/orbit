//! Phone access controls. The service owns invitations, permissions and devices.
use super::*;
use orbit_client::phone::{HostAction, HostStatus, Permissions};

pub(super) struct Phone {
    options: Options,
    runtime: Arc<tokio::runtime::Runtime>,
    status: HostStatus,
    qr: Option<Arc<Image>>,
    error: Option<String>,
    request_state: RequestState,
    task: Task<()>,
    poll: Task<()>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RequestState {
    Idle,
    Refresh,
    Mutation,
}

impl RequestState {
    fn begin(self, refresh: bool) -> Option<Self> {
        match (self, refresh) {
            (Self::Mutation, _) | (Self::Refresh, true) => None,
            (_, true) => Some(Self::Refresh),
            (_, false) => Some(Self::Mutation),
        }
    }
}

impl Phone {
    pub fn bind_workspace(&mut self, workspace: &str, cx: &mut Context<Self>) {
        let path = std::path::PathBuf::from(workspace);
        if self.options.workspace.as_ref() == Some(&path) {
            return;
        }
        self.task = Task::ready(());
        self.poll = Task::ready(());
        self.request_state = RequestState::Idle;
        self.options.workspace = Some(path);
        self.status = HostStatus::default();
        self.qr = None;
        self.request(HostAction::Status, cx);
    }

    pub fn new(
        options: &Options,
        runtime: Arc<tokio::runtime::Runtime>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            options: options.clone(),
            runtime,
            status: HostStatus::default(),
            qr: None,
            error: None,
            request_state: RequestState::Idle,
            task: Task::ready(()),
            poll: Task::ready(()),
        };
        view.request(HostAction::Status, cx);
        view
    }

    fn request(&mut self, action: HostAction, cx: &mut Context<Self>) {
        let Some(state) = self
            .request_state
            .begin(matches!(action, HostAction::Status))
        else {
            return;
        };
        self.request_state = state;
        self.poll = Task::ready(());
        let options = self.options.clone();
        let job = self.runtime.spawn(async move {
            orbit_client::phone::request(
                options.connection_options()?,
                options.config,
                options.database,
                options.auth_dir,
                action,
            )
            .await
        });
        self.task = cx.spawn(async move |owner, cx| {
            let result = job.await;
            let _ = owner.update(cx, |view, cx| {
                view.request_state = RequestState::Idle;
                match result {
                    Ok(Ok(status)) => {
                        if status.invitation != view.status.invitation {
                            view.qr = status
                                .invitation
                                .as_ref()
                                .and_then(|code| {
                                    let qr = qrcode::QrCode::new(code.as_bytes()).ok()?;
                                    let svg = qr
                                        .render::<qrcode::render::svg::Color>()
                                        .min_dimensions(320, 320)
                                        .build();
                                    Some(Arc::new(Image::from_bytes(
                                        ImageFormat::Svg,
                                        svg.into_bytes(),
                                    )))
                                });
                        }
                        view.status = status;
                        view.error = None;
                        if view.status.enabled && view.status.invitation.is_none() {
                            view.request(
                                HostAction::Invite {
                                    permissions: Permissions {
                                        control: true,
                                        approve: true,
                                    },
                                },
                                cx,
                            );
                            return;
                        }
                    }
                    Ok(Err(error)) => view.error = Some(format!("{error:#}")),
                    Err(error) => view.error = Some(error.to_string()),
                }
                if view.status.enabled {
                    view.poll = cx.spawn(async move |owner, cx| {
                        cx.background_executor()
                            .timer(Duration::from_secs(1))
                            .await;
                        let _ = owner.update(cx, |view, cx| view.request(HostAction::Status, cx));
                    });
                }
                cx.notify();
            });
        });
        if state == RequestState::Mutation {
            cx.notify();
        }
    }
}
impl Render for Phone {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = self.status.enabled;
        let mut body = v_flex()
            .w_full()
            .min_w_0()
            .gap_6()
            .child(
                h_flex()
                    .gap_4()
                    .justify_between()
                    .child(
                        v_flex()
                            .gap_1()
                            .child(if enabled { "已开启" } else { "未开启" })
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("手机可选择本机项目并操作会话，任务始终在电脑上运行。"),
                            ),
                    )
                    .child(
                        Button::new("phone-toggle")
                            .label(if enabled {
                                "关闭连接"
                            } else {
                                "开启连接"
                            })
                            .disabled(self.request_state == RequestState::Mutation)
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.request(
                                    if enabled {
                                        HostAction::Disable
                                    } else {
                                        HostAction::Enable
                                    },
                                    cx,
                                );
                            })),
                    ),
            )
            .when_some(self.error.clone(), |body, error| {
                body.child(
                    div()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            });
        if !enabled {
            return body;
        }
        body = body.when_some(self.qr.clone(), |body, qr| {
            body.child(
                v_flex()
                    .gap_3()
                    .child(img(qr).w(rems(16.)).h(rems(16.)))
                    .child("用手机 Orbit 扫码连接，识别后可放下手机等待连接完成。"),
            )
        });
        if !self.status.devices.is_empty() {
            body = body.child(
                div()
                    .font_semibold()
                    .child("已配对手机"),
            );
        }
        for (index, id) in self
            .status
            .devices
            .clone()
            .into_iter()
            .enumerate()
        {
            body = body.child(
                h_flex()
                    .justify_between()
                    .gap_4()
                    .child(
                        self.status
                            .device_names
                            .get(&id)
                            .cloned()
                            .unwrap_or_else(|| "手机".into()),
                    )
                    .child(
                        Button::new(("phone-revoke", index))
                            .label("撤销访问")
                            .on_click(cx.listener(move |view, _, _, cx| {
                                view.request(HostAction::Revoke { device: id.clone() }, cx)
                            })),
                    ),
            );
        }
        body
    }
}

#[cfg(test)]
mod tests {
    use super::RequestState;

    #[test]
    fn polling_does_not_disable_controls_and_mutations_supersede_refreshes() {
        let refresh = RequestState::Idle.begin(true).unwrap();
        assert_ne!(refresh, RequestState::Mutation);
        assert_eq!(refresh.begin(true), None);
        let mutation = refresh.begin(false).unwrap();
        assert_eq!(mutation, RequestState::Mutation);
        assert_eq!(mutation.begin(true), None);
        assert_eq!(mutation.begin(false), None);
        assert_eq!(RequestState::Idle.begin(true), Some(RequestState::Refresh));
    }
}
