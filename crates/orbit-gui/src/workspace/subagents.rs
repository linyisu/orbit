//! Retained controls for children already delegated by this parent session.
use super::*;
use gpui_kit::component::{
    IndexPath,
    list::{List, ListDelegate, ListItem, ListState},
    spinner::Spinner,
    text::TextViewState,
};
use orbit_protocol::subagents::{Action, Agent, Snapshot};
use std::collections::HashMap;
mod state;
use state::{Operation, Requests, Uncertain};

pub(super) struct ChildAgents {
    id: EntityId,
    owner: WeakEntity<Workspace>,
    options: Options,
    session: String,
    workspace: String,
    rows: Vec<AgentRow>,
    selected: Option<String>,
    list: Entity<ListState<AgentList>>,
    requests: Requests,
    loaded: bool,
    open: bool,
    error: Option<String>,
    request: Task<()>,
    poll: Task<()>,
}

struct AgentRow {
    agent: Agent,
    draft: Entity<TextareaState>,
    text: Entity<TextViewState>,
    error: Option<String>,
    receipt: Option<String>,
    stopping: Option<i64>,
    uncertain_send: bool,
    uncertain_cancel: Option<i64>,
    _subscription: Subscription,
}

impl Workspace {
    pub(super) fn open_child_agents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(info) = &self.info else { return };
        let panel = if let Some(panel) = &self.child_agents
            && panel
                .read(cx)
                .belongs_to(&info.id, &info.workspace, &self.options)
        {
            panel.clone()
        } else {
            let owner = cx.weak_entity();
            let options = self.options.clone();
            let session = info.id.clone();
            let workspace = info.workspace.clone();
            let panel =
                cx.new(|cx| ChildAgents::new(owner, options, session, workspace, window, cx));
            self.child_agents = Some(panel.clone());
            panel
        };
        panel.update(cx, |panel, _| panel.open = true);
        present(panel.clone(), window, cx);
        panel.read(cx).list.focus_handle(cx).focus(window, cx);
        let panel = panel.downgrade();
        // Workspace is leased by this action; requests must start after it is
        // released so their scope checks cannot reenter the owner.
        cx.defer(move |cx| {
            let _ = panel.update(cx, |panel, cx| {
                panel.refresh(cx);
                panel.start_poll(cx);
            });
        });
    }
}

impl ChildAgents {
    fn new(
        owner: WeakEntity<Workspace>,
        options: Options,
        session: String,
        workspace: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let list_owner = cx.weak_entity();
        Self {
            id: cx.entity().entity_id(),
            owner,
            options,
            session,
            workspace,
            rows: Vec::new(),
            selected: None,
            list: cx.new(|cx| {
                ListState::new(
                    AgentList {
                        owner: list_owner,
                        items: Vec::new(),
                        selected: None,
                    },
                    window,
                    cx,
                )
            }),
            requests: Requests::new(),
            loaded: false,
            open: true,
            error: None,
            request: Task::ready(()),
            poll: Task::ready(()),
        }
    }

    fn belongs_to(&self, session: &str, workspace: &str, options: &Options) -> bool {
        self.session == session && self.workspace == workspace && self.options.same_host(options)
    }

    pub(super) fn disconnected(&mut self, cx: &mut Context<Self>) {
        let pending = self.requests.disconnected();
        self.request = Task::ready(());
        self.poll = Task::ready(());
        match pending {
            Some(Operation::Send { id, .. }) => {
                if let Some(row) = self.rows.iter_mut().find(|row| row.agent.id == id) {
                    row.uncertain_send = true;
                    row.error = None;
                    row.receipt = None;
                }
            }
            Some(Operation::Cancel { id, run }) => {
                if let Some(row) = self.rows.iter_mut().find(|row| row.agent.id == id) {
                    row.uncertain_cancel = Some(run);
                    row.error = None;
                    row.receipt = None;
                }
            }
            _ => {}
        }
        self.error =
            Some("连接已中断，列表与消息草稿已保留。重新连接后请刷新并确认操作结果。".into());
        cx.notify();
    }

    /// Called while Workspace is leased. Rebinding never reads its owner and
    /// keeps retained row/input entities only for exactly the same domain scope.
    pub(super) fn rebind(
        &mut self,
        session: &str,
        workspace: &str,
        options: &Options,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.belongs_to(session, workspace, options) {
            return false;
        }
        self.disconnected(cx);
        self.options = options.clone();
        true
    }

    pub(super) fn reconnected(&mut self, cx: &mut Context<Self>) {
        self.requests.reconnected();
        self.error = Some("连接已恢复，正在刷新真实子任务状态。未确认的操作不会自动重发。".into());
        let panel = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = panel.update(cx, |panel, cx| {
                if panel.current_scope(cx) {
                    panel.refresh(cx);
                    panel.start_poll(cx);
                }
            });
        });
        cx.notify();
    }

    fn current_scope(&self, cx: &App) -> bool {
        self.requests.connected
            && self.owner.upgrade().is_some_and(|owner| {
                let workspace = owner.read(cx);
                workspace.connected
                    && workspace
                        .child_agents
                        .as_ref()
                        .is_some_and(|panel| panel.entity_id() == self.id)
                    && workspace.info.as_ref().is_some_and(|info| {
                        self.belongs_to(&info.id, &info.workspace, &workspace.options)
                    })
            })
    }

    fn dispatch(
        &self,
        action: Action,
        cx: &mut Context<Self>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<Response, String>>, String> {
        if !self.current_scope(cx) {
            return Err("会话连接已改变，请重新打开子 Agent 列表。".into());
        }
        let request = Request::Subagents {
            session: self.session.clone(),
            expected_workspace: self.workspace.clone(),
            action,
        };
        let (result, response) = tokio::sync::oneshot::channel();
        let sent = self
            .owner
            .update(cx, |workspace, _| {
                workspace.backend.as_ref().is_some_and(|backend| {
                    backend.send(Command::Settings { request, result }).is_ok()
                })
            })
            .unwrap_or(false);
        if !sent {
            return Err("连接繁忙或已断开，请重试。".into());
        }
        Ok(response)
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(serial) = self.requests.begin(Operation::Refresh) else {
            return;
        };
        self.error = None;
        self.request = cx.spawn(async move |panel, cx| {
            let mut agents = Vec::new();
            let mut offset = 0;
            let outcome = loop {
                let response = match panel.update(cx, |panel, cx| {
                    if !panel.requests.accepts(serial) {
                        return Err("此前连接的列表请求已失效。".into());
                    }
                    panel.dispatch(Action::List { offset }, cx)
                }) {
                    Ok(Ok(response)) => response,
                    Ok(Err(error)) => break Err(error),
                    Err(_) => return,
                };
                match response
                    .await
                    .unwrap_or_else(|_| Err("连接已中断，请重试加载子 Agent。".into()))
                {
                    Ok(Response::Subagents(Snapshot::List { agents: page, next })) => {
                        agents.extend(page);
                        match next {
                            Some(next) if next > offset && next <= 20_000 => offset = next,
                            Some(_) => break Err("子 Agent 分页响应无效，请重试。".into()),
                            None => break Ok(agents),
                        }
                    }
                    Ok(_) => break Err("服务未返回子 Agent 列表，请更新服务后重试。".into()),
                    Err(error) => break Err(error),
                }
            };
            let _ = panel.update_in(cx, |panel, window, cx| {
                if !panel.current_scope(cx) || !panel.requests.accepts(serial) {
                    return;
                }
                match outcome {
                    Ok(agents) => {
                        panel.requests.refreshed(serial);
                        panel.receive(agents, window, cx);
                        panel.loaded = true;
                    }
                    Err(error) => {
                        panel.requests.finish(serial);
                        panel.error = Some(error);
                    }
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    fn receive(&mut self, agents: Vec<Agent>, window: &mut Window, cx: &mut Context<Self>) {
        let mut old: HashMap<_, _> = std::mem::take(&mut self.rows)
            .into_iter()
            .map(|row| (row.agent.id.clone(), row))
            .collect();
        for agent in agents {
            if self.rows.iter().any(|row| row.agent.id == agent.id) {
                continue;
            }
            let row = if let Some(mut row) = old.remove(&agent.id) {
                if row.agent.task != agent.task {
                    row.text
                        .update(cx, |text, cx| text.set_text(&agent.task, cx));
                }
                if let Some(run) = row.stopping {
                    if agent.run != run {
                        row.stopping = None;
                        row.receipt = Some("子 Agent 已开始另一轮任务，请检查最新状态。".into());
                    } else if terminal(&agent.status) {
                        row.stopping = None;
                        row.receipt = Some(format!("任务已结束：{}", status_label(&agent.status)));
                    }
                }
                if let Some(run) = row.uncertain_cancel {
                    if agent.run != run {
                        row.uncertain_cancel = None;
                        row.receipt = Some(
                            "原停止请求所属任务已结束；当前是另一轮任务，请检查最新状态。".into(),
                        );
                    } else if terminal(&agent.status) {
                        row.uncertain_cancel = None;
                        row.receipt =
                            Some(format!("已确认任务终态：{}", status_label(&agent.status)));
                    }
                }
                row.agent = agent;
                row
            } else {
                let draft = cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .placeholder("向所选子 Agent 发送消息…")
                        .auto_grow(2, 4)
                        .submit_on_enter(true)
                });
                let id = agent.id.clone();
                let subscription = cx.subscribe_in(
                    &draft,
                    window,
                    move |panel, _, event, window, cx| match event {
                        InputEvent::Change => {
                            if let Some(row) = panel.rows.iter_mut().find(|row| row.agent.id == id)
                            {
                                row.error = None;
                                row.receipt = None;
                            }
                            cx.notify();
                        }
                        InputEvent::PressEnter { shift: false, .. } => panel.send(&id, window, cx),
                        _ => {}
                    },
                );
                AgentRow {
                    text: cx.new(|cx| TextViewState::markdown(&agent.task, cx)),
                    agent,
                    draft,
                    error: None,
                    receipt: None,
                    stopping: None,
                    uncertain_send: false,
                    uncertain_cancel: None,
                    _subscription: subscription,
                }
            };
            self.rows.push(row);
        }
        // List selection is already updated by native keyboard navigation;
        // read it before replacing summaries so an async refresh cannot undo it.
        let selected = {
            let list = self.list.read(cx);
            list.selected_index()
                .and_then(|index| list.delegate().items.get(index.row))
                .map(|item| item.id.to_string())
        };
        if let Some(selected) = selected {
            self.selected = Some(selected);
        }
        if !self
            .rows
            .iter()
            .any(|row| Some(&row.agent.id) == self.selected.as_ref())
        {
            self.selected = self.rows.first().map(|row| row.agent.id.clone());
        }
        let selected = self
            .rows
            .iter()
            .position(|row| Some(&row.agent.id) == self.selected.as_ref())
            .map(IndexPath::new);
        let items = self
            .rows
            .iter()
            .map(|row| AgentSummary {
                id: row.agent.id.clone().into(),
                task: row.agent.task.clone().into(),
                description: format!(
                    "{} · {}",
                    role_label(&row.agent.role),
                    status_label(&row.agent.status)
                )
                .into(),
            })
            .collect();
        self.list.update(cx, |list, cx| {
            list.delegate_mut().items = items;
            if list.selected_index() != selected {
                list.set_selected_index(selected, window, cx);
            }
            cx.notify();
        });
    }

    fn send(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.requests.can_mutate() {
            return;
        }
        let Some(row) = self.rows.iter_mut().find(|row| row.agent.id == id) else {
            return;
        };
        if row.stopping.is_some() {
            return;
        }
        let message = row.draft.read(cx).value().trim().to_owned();
        if message.is_empty() || message.len() > 32_768 {
            row.error = Some("消息需要 1–32768 字节。".into());
            cx.notify();
            return;
        }
        let uncertain = row.uncertain_send;
        let operation = Operation::Send {
            id: id.into(),
            message,
        };
        if uncertain {
            self.confirm_retry(operation, window, cx);
        } else {
            self.mutate(operation, cx);
        }
    }

    fn stop(&mut self, id: &str, run: i64, window: &mut Window, cx: &mut Context<Self>) {
        if !self.requests.can_mutate() {
            return;
        }
        let Some(row) = self.rows.iter().find(|row| row.agent.id == id) else {
            return;
        };
        if row.stopping.is_some() || row.agent.run != run || row.agent.status != "running" {
            return;
        }
        let uncertain = row.uncertain_cancel == Some(run);
        let operation = Operation::Cancel { id: id.into(), run };
        if uncertain {
            self.confirm_retry(operation, window, cx);
        } else {
            self.mutate(operation, cx);
        }
    }

    fn confirm_retry(&mut self, operation: Operation, window: &mut Window, cx: &mut Context<Self>) {
        let panel = cx.weak_entity();
        let sending = matches!(operation, Operation::Send { .. });
        crate::dialog::open(window, cx, move |dialog, _, _| {
            let panel = panel.clone();
            let operation = operation.clone();
            dialog
                .title(if sending {
                    "确认再次发送消息？"
                } else {
                    "确认再次请求停止？"
                })
                .child(if sending {
                    "上次消息是否送达尚未确认。请先查看对话，确认需要再次发送当前草稿。"
                } else {
                    "上次停止请求的结果尚未确认。列表已刷新，再次请求只针对当前显示的同一轮任务。"
                })
                .on_ok(|_, _, _| false)
                .footer(
                    h_flex()
                        .flex_wrap()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("keep-unknown-agent-operation")
                                .label("返回检查")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("retry-unknown-agent-operation")
                                .label(if sending {
                                    "确认重发"
                                } else {
                                    "再次停止"
                                })
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = panel.update(cx, |panel, cx| {
                                        panel.mutate(operation.clone(), cx)
                                    });
                                }),
                        ),
                )
        });
    }

    fn mutate(&mut self, operation: Operation, cx: &mut Context<Self>) {
        if !self.requests.can_mutate() || !self.current_scope(cx) {
            return;
        }
        // A dialog may outlive a list refresh or a draft edit. Bind its
        // confirmation to the exact current row, message, and sampled run.
        let valid = match &operation {
            Operation::Send { id, message } => self.rows.iter().any(|row| {
                row.agent.id == *id
                    && row.stopping.is_none()
                    && row.draft.read(cx).value().trim() == message
            }),
            Operation::Cancel { id, run } => self.rows.iter().any(|row| {
                row.agent.id == *id
                    && row.stopping.is_none()
                    && row.agent.run == *run
                    && row.agent.status == "running"
            }),
            Operation::Refresh => false,
        };
        if !valid {
            return;
        }
        let (id, action) = match &operation {
            Operation::Send { id, message } => (
                id.clone(),
                Action::Send {
                    id: id.clone(),
                    message: message.clone(),
                },
            ),
            Operation::Cancel { id, run } => (
                id.clone(),
                Action::Cancel {
                    id: id.clone(),
                    run: *run,
                },
            ),
            Operation::Refresh => return,
        };
        let Some(serial) = self.requests.begin(operation.clone()) else {
            return;
        };
        let response = match self.dispatch(action, cx) {
            Ok(response) => response,
            Err(error) => {
                self.requests.finish(serial);
                if let Some(row) = self.rows.iter_mut().find(|row| row.agent.id == id) {
                    row.error = Some(error);
                }
                cx.notify();
                return;
            }
        };
        if let Some(row) = self.rows.iter_mut().find(|row| row.agent.id == id) {
            row.error = None;
            row.receipt = None;
        }
        self.request = cx.spawn(async move |panel, cx| {
            let result = response.await.unwrap_or_else(|_| {
                Err("连接已中断，无法确认子 Agent 操作结果；请先刷新状态。".into())
            });
            let _ = panel.update_in(cx, |panel, window, cx| {
                if !panel.current_scope(cx) || !panel.requests.accepts(serial) {
                    return;
                }
                panel.requests.finish(serial);
                let Some(row) = panel.rows.iter_mut().find(|row| row.agent.id == id) else {
                    return;
                };
                match (&operation, result) {
                    (
                        Operation::Send { message, .. },
                        Ok(Response::Subagents(Snapshot::Sent {
                            id: received,
                            status,
                            ..
                        })),
                    ) if received == id && matches!(status.as_str(), "queued" | "running") => {
                        row.uncertain_send = false;
                        if row.draft.read(cx).value().trim() == message {
                            row.draft
                                .update(cx, |input, cx| input.set_value("", window, cx));
                        }
                        row.receipt = Some(
                            if status == "queued" {
                                "消息已排队，等待子 Agent 处理。"
                            } else {
                                "已启动后续任务，等待结果。"
                            }
                            .into(),
                        );
                    }
                    (
                        Operation::Cancel { run, .. },
                        Ok(Response::Subagents(Snapshot::Stopping {
                            id: received,
                            run: received_run,
                        })),
                    ) if received == id && received_run == *run => {
                        row.uncertain_cancel = None;
                        row.stopping = Some(*run);
                        row.receipt = Some("已请求停止，等待任务终态…".into());
                    }
                    (_, outcome) => {
                        panel.requests.needs_refresh = true;
                        match &operation {
                            Operation::Send { .. } => row.uncertain_send = true,
                            Operation::Cancel { run, .. } => row.uncertain_cancel = Some(*run),
                            Operation::Refresh => {}
                        }
                        row.error = Some(match outcome {
                            Ok(_) => "服务操作回执不匹配，请先刷新子 Agent 状态。".into(),
                            Err(error) => error,
                        });
                    }
                }
                let panel = cx.weak_entity();
                cx.defer(move |cx| {
                    let _ = panel.update(cx, |panel, cx| panel.refresh(cx));
                });
                cx.notify();
            });
        });
        cx.notify();
    }

    fn inspect(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.current_scope(cx) || !self.rows.iter().any(|row| row.agent.id == id) {
            return;
        }
        let session = self.session.clone();
        let agent = id.to_owned();
        let _ = self.owner.update(cx, |workspace, cx| {
            workspace.command(
                Command::Request(Request::ReadSubagent {
                    session,
                    agent,
                    after: 0,
                }),
                cx,
            );
        });
    }

    fn start_poll(&mut self, cx: &mut Context<Self>) {
        self.poll = cx.spawn(async move |panel, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let keep = panel
                    .update(cx, |panel, cx| {
                        if !panel.current_scope(cx) {
                            return false;
                        }
                        let visible = panel
                            .owner
                            .upgrade()
                            .is_some_and(|owner| owner.read(cx).visible);
                        if panel.open
                            && visible
                            && panel.requests.can_mutate()
                            && panel
                                .rows
                                .iter()
                                .any(|row| row.agent.status == "running" || row.stopping.is_some())
                        {
                            panel.refresh(cx);
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        });
    }

    fn composer(&mut self, _: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let row = self
            .rows
            .iter()
            .find(|row| Some(&row.agent.id) == self.selected.as_ref())?;
        let id = row.agent.id.clone();
        let inspect_id = id.clone();
        let stop_id = id.clone();
        let run = row.agent.run;
        let sending = matches!(&self.requests.pending, Some(Operation::Send { id: target, .. }) if target == &id);
        let stopping = row.stopping.is_some()
            || matches!(&self.requests.pending, Some(Operation::Cancel { id: target, .. }) if target == &id);
        let ready = !row.draft.read(cx).value().trim().is_empty();
        let can_mutate = self.requests.can_mutate();
        Some(
            v_flex()
                .id("child-agent-composer")
                .tab_group()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .gap_2()
                .child(
                    Textarea::new(&row.draft)
                        .disabled(sending)
                        .aria_label("向所选子 Agent 发送消息"),
                )
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .flex_wrap()
                        .gap_2()
                        .child(
                            Button::new("inspect-selected-agent")
                                .small()
                                .label("查看对话")
                                .disabled(!self.requests.connected)
                                .on_click(cx.listener(move |panel, _, _, cx| {
                                    panel.inspect(&inspect_id, cx)
                                })),
                        )
                        .when(row.agent.status == "running" || stopping, |view| {
                            view.child(
                                Button::new("stop-selected-agent")
                                    .small()
                                    .danger()
                                    .label(if stopping {
                                        "等待停止…"
                                    } else if row.uncertain_cancel == Some(run) {
                                        "确认后停止"
                                    } else {
                                        "停止任务"
                                    })
                                    .disabled(!can_mutate || stopping)
                                    .on_click(cx.listener(move |panel, _, window, cx| {
                                        panel.stop(&stop_id, run, window, cx)
                                    })),
                            )
                        })
                        .child(
                            Button::new("send-selected-agent")
                                .small()
                                .primary()
                                .label(if sending {
                                    "发送中…"
                                } else if row.uncertain_send {
                                    "确认后重发"
                                } else {
                                    "发送消息"
                                })
                                .disabled(!can_mutate || stopping || !ready)
                                .on_click(cx.listener(move |panel, _, window, cx| {
                                    panel.send(&id, window, cx)
                                })),
                        ),
                )
                .into_any_element(),
        )
    }
}

fn present(panel: Entity<ChildAgents>, window: &mut Window, cx: &mut App) {
    window.open_sheet(cx, move |sheet, window, cx| {
        let close = panel.downgrade();
        // The native Sheet owns the body's scroll viewport and the fixed
        // composer footer. Rendering never reads or updates Workspace.
        let composer = panel.update(cx, |panel, cx| panel.composer(window, cx));
        sheet
            .title("已委派的子 Agent")
            .size(
                (window.rem_size() * 48.)
                    .min((window.viewport_size().width - window.rem_size() * 2.).max(px(0.))),
            )
            .child(panel.clone())
            .when_some(composer, |sheet, composer| sheet.footer(composer))
            .on_close(move |_, _, cx| {
                // Dismissing navigation never cancels a child or discards a
                // message draft; pending mutation receipts still reconcile.
                let _ = close.update(cx, |panel, _| panel.open = false);
            })
    });
}

impl Render for ChildAgents {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pending = self.requests.pending.is_some();
        let refreshing = matches!(self.requests.pending, Some(Operation::Refresh));
        let focus = self.list.focus_handle(cx);
        let row = self
            .rows
            .iter()
            .find(|row| Some(&row.agent.id) == self.selected.as_ref());
        let compact = window.viewport_size().height < window.rem_size() * 40.;
        let list_height = if compact { rems(5.) } else { rems(12.) };
        let list_max_height = window.viewport_size().height * if compact { 0.2 } else { 0.3 };
        v_flex()
            .id("child-agent-controls")
            .tab_group()
            .w_full()
            .min_w_0()
            .py_2()
            .gap_3()
            .when(compact, |view| view.gap_2())
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{} 项子任务 · 关闭面板后继续运行", self.rows.len())),
                    )
                    .child(
                        Button::new("refresh-child-agents")
                            .small()
                            .ghost()
                            .label(if self.error.is_some() {
                                "重试刷新"
                            } else {
                                "刷新"
                            })
                            .disabled(pending || !self.requests.connected)
                            .on_click(cx.listener(|panel, _, _, cx| panel.refresh(cx))),
                    ),
            )
            .when(refreshing, |view| {
                view.child(
                    h_flex()
                        .gap_2()
                        .child(Spinner::new().small())
                        .child("正在更新子任务状态…"),
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
            })
            .when(!self.rows.is_empty(), |view| {
                view.child(
                    div()
                        .h(list_height)
                        .max_h(list_max_height)
                        .flex_shrink_0()
                        .min_w_0()
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded_lg()
                        .track_focus(&focus)
                        .tab_index(0)
                        .child(List::new(&self.list)),
                )
            })
            .when_some(row, |view, row| {
                view.child(
                    v_flex()
                        .id(SharedString::from(format!(
                            "child-agent-detail-{}",
                            row.agent.id
                        )))
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child(format!(
                                    "{} · {}",
                                    role_label(&row.agent.role),
                                    status_label(&row.agent.status)
                                )),
                        )
                        .child(
                            div()
                                .text_sm()
                                .whitespace_normal()
                                .text_color(cx.theme().muted_foreground)
                                .child(row.agent.workspace.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .whitespace_normal()
                                .text_color(cx.theme().muted_foreground)
                                .child(row.agent.model.clone()),
                        )
                        .child(
                            TextView::new(&row.text)
                                .w_full()
                                .selectable(true)
                                .style(crate::theme::document_style()),
                        )
                        .when_some(row.error.clone(), |view, error| {
                            view.child(
                                div()
                                    .text_sm()
                                    .whitespace_normal()
                                    .text_color(cx.theme().danger)
                                    .child(error),
                            )
                        })
                        .when_some(row.receipt.clone(), |view, receipt| {
                            view.child(
                                div()
                                    .text_sm()
                                    .whitespace_normal()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(receipt),
                            )
                        })
                        .when(row.uncertain_send, |view| {
                            view.child(
                                div()
                                    .text_sm()
                                    .whitespace_normal()
                                    .text_color(cx.theme().warning)
                                    .child(Uncertain::Send.message()),
                            )
                        })
                        .when_some(row.uncertain_cancel, |view, run| {
                            view.child(
                                div()
                                    .text_sm()
                                    .whitespace_normal()
                                    .text_color(cx.theme().warning)
                                    .child(Uncertain::Cancel { run }.message()),
                            )
                        }),
                )
            })
            .when(row.is_none() && self.loaded, |view| {
                view.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("当前会话尚未委派子 Agent。"),
                )
            })
    }
}

struct AgentSummary {
    id: SharedString,
    task: SharedString,
    description: SharedString,
}
struct AgentList {
    owner: WeakEntity<ChildAgents>,
    items: Vec<AgentSummary>,
    selected: Option<IndexPath>,
}
impl ListDelegate for AgentList {
    type Item = ListItem;
    fn items_count(&self, _: usize, _: &App) -> usize {
        self.items.len()
    }
    fn render_item(
        &mut self,
        index: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let item = self.items.get(index.row)?;
        Some(
            ListItem::new(item.id.clone())
                .h_16()
                .px_3()
                .confirmed(self.selected == Some(index))
                .accessibility_label(format!("{}，{}", item.task, item.description))
                .child(
                    v_flex()
                        .min_w_0()
                        .w_full()
                        .gap_1()
                        .child(div().text_sm().text_ellipsis().child(item.task.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(item.description.clone()),
                        ),
                ),
        )
    }
    fn set_selected_index(
        &mut self,
        selected: Option<IndexPath>,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        if self.selected == selected {
            return;
        }
        self.selected = selected;
        let id = selected
            .and_then(|index| self.items.get(index.row))
            .map(|item| item.id.to_string());
        let owner = self.owner.clone();
        cx.defer(move |cx| {
            let _ = owner.update(cx, |panel, cx| {
                let current = {
                    let list = panel.list.read(cx);
                    list.selected_index()
                        .and_then(|index| list.delegate().items.get(index.row))
                        .map(|item| item.id.to_string())
                };
                if current == id {
                    panel.selected = id;
                    cx.notify();
                }
            });
        });
    }
}

fn terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "interrupted")
}
fn status_label(status: &str) -> &'static str {
    match status {
        "running" => "运行中",
        "completed" => "已完成",
        "failed" => "失败",
        "cancelled" => "已停止",
        "interrupted" => "已中断",
        "idle" => "尚未启动",
        _ => "状态未知",
    }
}
fn role_label(role: &str) -> &str {
    match role {
        "explore" => "探索",
        "review" => "评审",
        "develop" => "开发",
        _ => role,
    }
}
