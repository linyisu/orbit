//! The current target owns fetching, immutable scan tokens and installation.
use super::*;
use gpui_kit::component::spinner::Spinner;
use orbit_protocol::resources::{McpCandidate, McpLaunch};

pub(super) enum Event {
    Prepared { name: String, launch: McpLaunch },
    Closed,
}
pub(super) struct Importer {
    owner: WeakEntity<Workspace>,
    session: String,
    workspace: String,
    source: Entity<InputState>,
    scan: Option<(String, Vec<McpCandidate>)>,
    selected: Option<String>,
    busy: bool,
    installing: bool,
    error: Option<String>,
    task: Task<()>,
    _source: Subscription,
}

impl EventEmitter<Event> for Importer {}
impl Importer {
    pub(super) fn new(
        owner: WeakEntity<Workspace>,
        session: String,
        workspace: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let source = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("owner/repo、Git URL、GitHub 子目录或目标机器上的目录")
        });
        let events = cx.subscribe(&source, |view: &mut Self, _, event, cx| {
            if matches!(event, InputEvent::Change) && !view.busy {
                view.scan = None;
                view.selected = None;
                view.error = None;
                cx.notify();
            }
        });
        source
            .focus_handle(cx)
            .focus(window, cx);
        Self {
            owner,
            session,
            workspace,
            source,
            scan: None,
            selected: None,
            busy: false,
            installing: false,
            error: None,
            task: Task::ready(()),
            _source: events,
        }
    }

    fn scan(&mut self, cx: &mut Context<Self>) {
        let source = self
            .source
            .read(cx)
            .value()
            .trim()
            .to_owned();
        if source.is_empty() || self.busy {
            return;
        }
        self.scan = None;
        self.selected = None;
        self.request(Action::ScanMcp { source }, cx);
    }

    fn install(&mut self, cx: &mut Context<Self>) {
        let Some((token, _)) = &self.scan else {
            return;
        };
        let Some(id) = &self.selected else {
            return;
        };
        if !self.busy {
            self.request(
                Action::InstallMcp {
                    token: token.clone(),
                    id: id.clone(),
                },
                cx,
            );
        }
    }

    fn request(&mut self, action: Action, cx: &mut Context<Self>) {
        let installing = matches!(action, Action::InstallMcp { .. });
        let session = self.session.clone();
        let workspace = self.workspace.clone();
        let (result, receiver) = tokio::sync::oneshot::channel();
        let sent =
            self.owner
                .update(cx, |view, _| {
                    anyhow::ensure!(
                        view.connected
                            && view.info.as_ref().is_some_and(
                                |info| info.id == session && info.workspace == workspace
                            ),
                        "连接或工作区已改变，请在当前项目重新导入"
                    );
                    view.backend
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("请先连接工作区"))?
                        .send(Command::Settings {
                            request: Request::Resources {
                                session: session.clone(),
                                expected_workspace: Some(workspace.clone()),
                                action,
                            },
                            result,
                        })
                })
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()));
        if let Err(error) = sent {
            self.error = Some(error);
            cx.notify();
            return;
        }
        self.busy = true;
        self.installing = installing;
        self.error = None;
        self.task = cx.spawn(async move |entity, cx| {
            let result = receiver
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            let _ = entity.update(cx, |view, cx| {
                view.busy = false;
                view.installing = false;
                let current = view
                    .owner
                    .upgrade()
                    .is_some_and(|owner| {
                        let owner = owner.read(cx);
                        owner.connected
                            && owner.info.as_ref().is_some_and(|info| {
                                info.id == session && info.workspace == workspace
                            })
                    });
                if !current {
                    view.scan = None;
                    view.error =
                        Some("连接或工作区已改变，请回到原项目核对安装结果，再重新扫描。".into());
                    cx.notify();
                    return;
                }
                match result {
                    Ok(Response::Resources(Snapshot::McpSource {
                        token, candidates, ..
                    })) if !installing => {
                        view.scan = Some((token, candidates));
                    }
                    Ok(Response::Resources(Snapshot::McpInstalled { name, launch }))
                        if installing =>
                    {
                        cx.emit(Event::Prepared { name, launch })
                    }
                    result => {
                        view.error = Some(match result {
                            Err(error) => error,
                            _ => "服务未返回 MCP 导入结果，请更新工作区服务后重试。".into(),
                        });
                        if installing {
                            view.scan = None;
                            view.selected = None;
                        }
                    }
                }
                cx.notify();
            });
        });
        cx.notify();
    }
}

impl Render for Importer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .child(
                h_flex().child(
                    Button::new("mcp-import-back")
                        .ghost()
                        .small()
                        .icon(IconName::ArrowLeft)
                        .label("返回 MCP 服务器")
                        .disabled(self.busy)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(Event::Closed))),
                ),
            )
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("从来源导入 MCP"),
            )
            .child(
                div()
                    .text_sm()
                    .whitespace_normal()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "在 {} 获取来源。识别 .mcp.json / mcp.json 配置、Node.js bin 和 Python \
                    project.scripts；扫描时不运行仓库代码。",
                        orbit_client::directories::display_path(&self.workspace)
                    )),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.source)
                                .aria_label("MCP 来源")
                                .disabled(self.busy),
                        ),
                    )
                    .child(
                        Button::new("mcp-scan-source")
                            .label("拉取并识别")
                            .disabled(
                                self.busy
                                    || self
                                        .source
                                        .read(cx)
                                        .value()
                                        .trim()
                                        .is_empty(),
                            )
                            .on_click(cx.listener(|view, _, _, cx| view.scan(cx))),
                    ),
            )
            .when(self.busy, |view| {
                view.child(
                    h_flex()
                        .gap_2()
                        .child(Spinner::new().small())
                        .child(if self.installing {
                            "正在安装依赖，完成后进入配置…"
                        } else {
                            "正在获取并识别来源…"
                        }),
                )
            })
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .text_sm()
                        .whitespace_normal()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            });
        if let Some((_, candidates)) = &self.scan {
            if candidates.is_empty() {
                content = content.child("未发现可识别的 stdio 启动入口。可选择仓库子目录，或返回手动添加。HTTP / OAuth 服务暂不支持。");
            }
            for candidate in candidates {
                let id = candidate.id.clone();
                content = content.child(
                    v_flex()
                        .gap_1()
                        .py_2()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            h_flex().child(
                                Button::new(SharedString::from(format!(
                                    "mcp-candidate:{}",
                                    candidate.id
                                )))
                                .ghost()
                                .label(candidate.name.clone())
                                .icon(if self.selected.as_ref() == Some(&candidate.id) {
                                    IconName::Check
                                } else {
                                    IconName::Plus
                                })
                                .disabled(self.busy)
                                .on_click(cx.listener(move |view, _, _, cx| {
                                    view.selected = Some(id.clone());
                                    cx.notify();
                                })),
                            ),
                        )
                        .child(
                            div()
                                .text_sm()
                                .whitespace_normal()
                                .text_color(cx.theme().muted_foreground)
                                .child(candidate.description.clone()),
                        ),
                );
            }
            if let Some(candidate) = candidates
                .iter()
                .find(|candidate| Some(&candidate.id) == self.selected.as_ref())
            {
                content = content.child(v_flex().gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("确认启动与安装命令"))
                    .child(div()
                        .text_sm()
                        .whitespace_normal()
                        .child(format!("启动：{} {}",
                        candidate
                        .launch
                        .command,
                        candidate
                        .launch
                        .args
                        .join(" "))))
                    .children(candidate
                        .install
                        .iter()
                        .map(|command| div()
                        .text_sm()
                        .whitespace_normal()
                        .child(command
                        .join(" "))))
                    .child(div()
                        .text_sm()
                        .whitespace_normal()
                        .text_color(cx
                        .theme()
                        .muted_foreground)
                        .child(if candidate
                            .install
                            .is_empty() {
                            "此入口无需自动安装依赖。继续后可编辑参数与环境变量，保存时默认保持禁用。"
                        } else {
                            "确认后会在目标机器执行以上安装命令及仓库\
                            构建脚本。完成后仍可编辑参数与环境变量，保存时默认保持禁用。" }))
                    .child(h_flex()
                        .child(Button::new("mcp-install-source")
                        .primary()
                        .label(if candidate
                        .install
                        .is_empty() {
                        "使用此配置"
                    } else {
                        "确认安装并配置"
                    })
                        .disabled(self
                            .busy)
                            .on_click(cx
                            .listener(|view,
                            _,
                            _,
                            cx| view
                            .install(cx))))));
            }
        }
        content
    }
}
