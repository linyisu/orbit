//! Durable queued text, with row-owned drafts and run/content compare-and-swap.
use super::*;
use gpui_kit::component::{
    IndexPath,
    list::{List, ListDelegate, ListItem, ListState},
    spinner::Spinner,
};
use orbit_protocol::input_queue::{Action, Entry, Snapshot, State};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Operation {
    List {
        after: Option<i64>,
    },
    Edit {
        id: i64,
        expected: String,
        content: String,
        run: Option<i64>,
    },
    Remove {
        id: i64,
        expected: String,
        run: Option<i64>,
    },
}

impl Operation {
    fn id(&self) -> Option<i64> {
        match self {
            Self::List { .. } => None,
            Self::Edit { id, .. } | Self::Remove { id, .. } => Some(*id),
        }
    }

    fn action(&self) -> Action {
        match self {
            Self::List { after } => Action::List { after: *after },
            Self::Edit {
                id,
                expected,
                content,
                run,
            } => Action::Edit {
                id: *id,
                expected: expected.clone(),
                content: content.clone(),
                run: *run,
            },
            Self::Remove { id, expected, run } => Action::Remove {
                id: *id,
                expected: expected.clone(),
                run: *run,
            },
        }
    }
}

struct Requests {
    pending: Option<Operation>,
    serial: u64,
    connected: bool,
    needs_refresh: bool,
}

impl Requests {
    fn new() -> Self {
        Self {
            pending: None,
            serial: 0,
            connected: true,
            needs_refresh: true,
        }
    }

    fn ready(&self) -> bool {
        self.connected && !self.needs_refresh && self.pending.is_none()
    }

    fn begin(&mut self, operation: Operation) -> Option<u64> {
        if !self.connected
            || self.pending.is_some()
            || (self.needs_refresh && !matches!(operation, Operation::List { after: None }))
        {
            return None;
        }
        self.serial += 1;
        self.pending = Some(operation);
        Some(self.serial)
    }

    fn accepts(&self, serial: u64) -> bool {
        self.connected && self.serial == serial && self.pending.is_some()
    }

    fn finish(&mut self, serial: u64) -> bool {
        if !self.accepts(serial) {
            return false;
        }
        self.pending = None;
        true
    }

    fn disconnected(&mut self) -> Option<Operation> {
        self.serial += 1;
        self.connected = false;
        self.needs_refresh = true;
        self.pending.take()
    }
}

fn valid_page(after: Option<i64>, entries: &[Entry], next: Option<i64>) -> bool {
    let mut previous = after.unwrap_or(0);
    for entry in entries {
        if entry.id <= previous {
            return false;
        }
        previous = entry.id;
    }
    next.is_none() || (!entries.is_empty() && next == entries.last().map(|entry| entry.id))
}

fn valid_entry(entry: &Entry, run: Option<i64>) -> bool {
    entry.id > 0
        && run.is_none_or(|run| run > 0)
        && match entry.state {
            State::Queued => run.is_some(),
            State::Idle => run.is_none(),
            State::Claimed | State::Finishing | State::Unavailable => true,
        }
}

/// Only an acknowledged edit of the still-current draft may normalize its text.
fn acknowledged_draft<'a>(current: &str, submitted: &str, stored: &'a str) -> Option<&'a str> {
    (current == submitted && stored == submitted.trim()).then_some(stored)
}

#[derive(Debug, PartialEq, Eq)]
enum DraftRefresh {
    Replace,
    Preserve,
    Conflict,
    Uncertain,
}

fn refresh_draft(current: &str, previous: &str, stored: &str, uncertain: bool) -> DraftRefresh {
    if uncertain {
        if current.trim() == stored {
            DraftRefresh::Preserve
        } else {
            DraftRefresh::Uncertain
        }
    } else if current == previous {
        DraftRefresh::Replace
    } else if previous != stored && current != stored {
        DraftRefresh::Conflict
    } else {
        DraftRefresh::Preserve
    }
}

fn keep_absent_draft(current: &str, previous: &str, uncertain: bool) -> bool {
    uncertain || current != previous
}

pub(super) struct InputQueue {
    id: EntityId,
    owner: WeakEntity<Workspace>,
    options: Options,
    session: String,
    workspace: String,
    rows: Vec<QueueRow>,
    selected: Option<i64>,
    list: Entity<ListState<QueueList>>,
    requests: Requests,
    run: Option<i64>,
    next: Option<i64>,
    loaded: bool,
    error: Option<String>,
    request: Task<()>,
}

struct QueueRow {
    entry: Entry,
    draft: Entity<TextareaState>,
    confirmed: bool,
    uncertain: bool,
    error: Option<String>,
    _subscription: Subscription,
}

impl Workspace {
    pub(super) fn open_input_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(info) = &self.info else { return };
        let panel = if let Some(panel) = &self.input_queue
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
            let connected = self.connected;
            let panel = cx.new(|cx| {
                InputQueue::new(owner, options, session, workspace, connected, window, cx)
            });
            self.input_queue = Some(panel.clone());
            panel
        };
        present(panel.clone(), window, cx);
        panel.read(cx).list.focus_handle(cx).focus(window, cx);
        let panel = panel.downgrade();
        // This action still leases Workspace. Scope checks and dispatch must
        // wait for that lease to end rather than reentering the parent entity.
        cx.defer(move |cx| {
            let _ = panel.update(cx, |panel, cx| panel.load(None, cx));
        });
    }
}

impl InputQueue {
    fn new(
        owner: WeakEntity<Workspace>,
        options: Options,
        session: String,
        workspace: String,
        connected: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let list_owner = cx.weak_entity();
        let mut requests = Requests::new();
        requests.connected = connected;
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
                    QueueList {
                        owner: list_owner,
                        items: Vec::new(),
                        selected: None,
                    },
                    window,
                    cx,
                )
            }),
            requests,
            run: None,
            next: None,
            loaded: false,
            error: (!connected).then(|| "连接已断开，恢复连接后可读取输入队列。".into()),
            request: Task::ready(()),
        }
    }

    fn belongs_to(&self, session: &str, workspace: &str, options: &Options) -> bool {
        self.session == session && self.workspace == workspace && self.options.same_host(options)
    }

    fn current_scope(&self, cx: &App) -> bool {
        self.requests.connected
            && self.owner.upgrade().is_some_and(|owner| {
                let workspace = owner.read(cx);
                workspace.connected
                    && workspace
                        .input_queue
                        .as_ref()
                        .is_some_and(|panel| panel.entity_id() == self.id)
                    && workspace.info.as_ref().is_some_and(|info| {
                        self.belongs_to(&info.id, &info.workspace, &workspace.options)
                    })
            })
    }

    pub(super) fn disconnected(&mut self, cx: &mut Context<Self>) {
        if let Some(operation) = self.requests.disconnected()
            && let Some(id) = operation.id()
            && let Some(row) = self.rows.iter_mut().find(|row| row.entry.id == id)
        {
            row.uncertain = true;
            row.error = Some("操作结果尚未确认；草稿已保留，重连后先刷新真实状态。".into());
        }
        self.request = Task::ready(());
        self.error = Some("连接已断开，输入内容与本地草稿已保留。".into());
        cx.notify();
    }

    /// Parent-leased hook: it never reads Workspace or dispatches a request.
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
        self.requests.connected = true;
        self.requests.needs_refresh = true;
        self.error = Some("连接已恢复，正在刷新输入队列。未确认的操作不会自动重发。".into());
        let panel = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = panel.update(cx, |panel, cx| {
                if panel.current_scope(cx) {
                    panel.load(None, cx);
                }
            });
        });
        cx.notify();
    }

    fn dispatch(
        &self,
        action: Action,
        cx: &mut Context<Self>,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<Response, String>>, String> {
        if !self.current_scope(cx) {
            return Err("会话连接已改变，请重新打开输入队列。".into());
        }
        let request = Request::InputQueue {
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
            return Err("连接繁忙或已断开，请重试刷新。".into());
        }
        Ok(response)
    }

    fn load(&mut self, after: Option<i64>, cx: &mut Context<Self>) {
        if after.is_some() && (!self.loaded || after != self.next) {
            return;
        }
        self.start(Operation::List { after }, cx);
    }

    fn start(&mut self, operation: Operation, cx: &mut Context<Self>) {
        let Some(serial) = self.requests.begin(operation.clone()) else {
            return;
        };
        self.error = None;
        if let Some(id) = operation.id()
            && let Some(row) = self.rows.iter_mut().find(|row| row.entry.id == id)
        {
            row.error = None;
        }
        let panel = cx.weak_entity();
        // Always dispatch after the current entity leases end, including
        // callers reached through a Workspace action or reconnect callback.
        cx.defer(move |cx| {
            let _ = panel.update(cx, |panel, cx| panel.send(serial, operation, cx));
        });
        cx.notify();
    }

    fn send(&mut self, serial: u64, operation: Operation, cx: &mut Context<Self>) {
        if !self.requests.accepts(serial) {
            return;
        }
        let response = match self.dispatch(operation.action(), cx) {
            Ok(response) => response,
            Err(error) => {
                self.requests.finish(serial);
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.request = cx.spawn(async move |panel, cx| {
            let result = response
                .await
                .unwrap_or_else(|_| Err("连接已中断，无法确认结果。请刷新队列后再操作。".into()));
            let _ = panel.update_in(cx, |panel, window, cx| {
                if !panel.current_scope(cx) || !panel.requests.accepts(serial) {
                    return;
                }
                panel.requests.finish(serial);
                panel.receive(operation, result, window, cx);
                cx.notify();
            });
        });
        cx.notify();
    }

    fn receive(
        &mut self,
        operation: Operation,
        result: Result<Response, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (&operation, result) {
            (
                Operation::List { after },
                Ok(Response::InputQueue(Snapshot::List { entries, next, run })),
            ) if valid_page(*after, &entries, next)
                && entries.iter().all(|entry| valid_entry(entry, run))
                && run.is_none_or(|run| run > 0)
                && (after.is_none() || self.run == run) =>
            {
                if after.is_none() {
                    for row in &mut self.rows {
                        row.confirmed = false;
                    }
                    self.run = run;
                }
                self.merge(entries, window, cx);
                self.next = next;
                self.loaded = true;
                self.requests.needs_refresh = false;
                if next.is_none() {
                    self.rows.retain_mut(|row| {
                        if !row.confirmed {
                            if !keep_absent_draft(
                                &row.draft.read(cx).value(),
                                &row.entry.content,
                                row.uncertain,
                            ) {
                                return false;
                            }
                            row.entry.state = State::Unavailable;
                            row.confirmed = true;
                            row.error =
                                Some("此输入已离开队列，本地草稿已保留，可查看和复制。".into());
                        }
                        true
                    });
                }
                self.sync_list(window, cx);
            }
            (
                Operation::Edit { id, content, .. },
                Ok(Response::InputQueue(Snapshot::Edited { entry })),
            ) if entry.id == *id
                && entry.content == content.trim()
                && entry.editable()
                && valid_entry(&entry, self.run) =>
            {
                if let Some(row) = self.rows.iter_mut().find(|row| row.entry.id == *id) {
                    if let Some(stored) =
                        acknowledged_draft(&row.draft.read(cx).value(), content, &entry.content)
                    {
                        row.draft
                            .update(cx, |draft, cx| draft.set_value(stored, window, cx));
                    }
                    row.entry = entry;
                    row.confirmed = true;
                    row.uncertain = false;
                    row.error = None;
                }
                self.refresh_after_mutation(cx);
            }
            (
                Operation::Remove { id, .. },
                Ok(Response::InputQueue(Snapshot::Removed { id: received })),
            ) if received == *id => {
                let removed = self.rows.iter().position(|row| row.entry.id == *id);
                self.rows.retain(|row| row.entry.id != *id);
                if self.selected == Some(*id) {
                    self.selected = removed.and_then(|index| {
                        self.rows
                            .get(index.min(self.rows.len().saturating_sub(1)))
                            .map(|row| row.entry.id)
                    });
                }
                self.sync_list(window, cx);
                self.refresh_after_mutation(cx);
            }
            (_, outcome) => {
                let error = match outcome {
                    Err(error) => error,
                    Ok(_) => "队列回执不匹配或任务已改变，请刷新真实状态。".into(),
                };
                // Failed reads cannot establish a current run/CAS baseline,
                // and failed mutations may have reached the service already.
                self.requests.needs_refresh = true;
                if let Some(id) = operation.id()
                    && let Some(row) = self.rows.iter_mut().find(|row| row.entry.id == id)
                {
                    row.uncertain = true;
                    row.error = Some(error.clone());
                }
                self.error = Some(error);
            }
        }
    }

    fn refresh_after_mutation(&mut self, cx: &mut Context<Self>) {
        self.requests.needs_refresh = true;
        let panel = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = panel.update(cx, |panel, cx| panel.load(None, cx));
        });
    }

    fn merge(&mut self, entries: Vec<Entry>, window: &mut Window, cx: &mut Context<Self>) {
        for entry in entries {
            if let Some(row) = self.rows.iter_mut().find(|row| row.entry.id == entry.id) {
                let refresh = refresh_draft(
                    &row.draft.read(cx).value(),
                    &row.entry.content,
                    &entry.content,
                    row.uncertain,
                );
                if refresh == DraftRefresh::Replace {
                    row.draft.update(cx, |draft, cx| {
                        draft.set_value(entry.content.clone(), window, cx)
                    });
                }
                row.entry = entry;
                row.confirmed = true;
                row.uncertain = refresh == DraftRefresh::Uncertain;
                row.error = match refresh {
                    DraftRefresh::Conflict => Some("服务内容已变化，本地草稿已保留。请核对后再保存。".into()),
                    DraftRefresh::Uncertain => Some("已加载最新内容，上次操作结果仍无法判定。本地草稿已保留，请核对后手动提交。".into()),
                    _ => None,
                };
                continue;
            }
            let draft = cx.new(|cx| {
                let mut draft = TextareaState::new(window, cx).submit_on_enter(false);
                draft.set_value(entry.content.clone(), window, cx);
                draft
            });
            let id = entry.id;
            let subscription = cx.subscribe_in(&draft, window, move |panel, _, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    if let Some(row) = panel.rows.iter_mut().find(|row| row.entry.id == id)
                        && !row.uncertain
                    {
                        row.error = None;
                    }
                    cx.notify();
                }
            });
            self.rows.push(QueueRow {
                entry,
                draft,
                confirmed: true,
                uncertain: false,
                error: None,
                _subscription: subscription,
            });
        }
        self.rows.sort_by_key(|row| row.entry.id);
    }

    fn sync_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self
            .list
            .read(cx)
            .selected_index()
            .and_then(|index| self.list.read(cx).delegate().items.get(index.row))
            .map(|item| item.id);
        if let Some(id) = previous
            && self.rows.iter().any(|row| row.entry.id == id)
        {
            self.selected = Some(id);
        }
        if !self
            .rows
            .iter()
            .any(|row| Some(row.entry.id) == self.selected)
        {
            self.selected = self.rows.first().map(|row| row.entry.id);
        }
        let selected = self
            .rows
            .iter()
            .position(|row| Some(row.entry.id) == self.selected)
            .map(IndexPath::new);
        let items = self
            .rows
            .iter()
            .map(|row| QueueSummary {
                id: row.entry.id,
                title: row
                    .entry
                    .content
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("空白输入")
                    .chars()
                    .take(160)
                    .collect::<String>()
                    .into(),
                state: if row.confirmed {
                    state_label(row.entry.state)
                } else {
                    "等待加载最新状态"
                },
            })
            .collect();
        self.list.update(cx, |list, cx| {
            list.delegate_mut().items = items;
            if list.selected_index() != selected || previous != self.selected {
                list.set_selected_index(selected, window, cx);
                list.scroll_to_selected_item(window, cx);
            }
            cx.notify();
        });
    }

    fn save(&mut self, id: i64, cx: &mut Context<Self>) {
        if !self.requests.ready() {
            return;
        }
        let Some(row) = self.rows.iter_mut().find(|row| row.entry.id == id) else {
            return;
        };
        if !row.confirmed || !row.entry.editable() {
            return;
        }
        let content = row.draft.read(cx).value().to_string();
        if content.trim().is_empty() || content.trim().len() > 1_048_576 {
            row.error = Some("输入内容需要 1–1048576 字节。".into());
            cx.notify();
            return;
        }
        let operation = Operation::Edit {
            id,
            expected: row.entry.content.clone(),
            content,
            run: self.run,
        };
        self.start(operation, cx);
    }

    fn confirm_remove(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if !self.requests.ready() {
            return;
        }
        let Some(row) = self.rows.iter().find(|row| row.entry.id == id) else {
            return;
        };
        if !row.confirmed || !row.entry.editable() {
            return;
        }
        let operation = Operation::Remove {
            id,
            expected: row.entry.content.clone(),
            run: self.run,
        };
        let serial = self.requests.serial;
        let panel = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let panel = panel.clone();
            let operation = operation.clone();
            dialog.title(format!("取消输入 #{id}？"))
                .child("这条尚未被 Agent 读取的文字将从队列中移除，无法恢复。")
                .on_ok(|_, _, _| false)
                .footer(h_flex().w_full().flex_wrap().justify_end().gap_2()
                    .child(Button::new("keep-queued-input").label("保留输入")
                        .on_click(|_, window, cx| window.close_dialog(cx)))
                    .child(Button::new("remove-queued-input").danger().label("取消输入")
                        .on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            let operation = operation.clone();
                            // Confirmation may have outlived a reconnect or refresh.
                            let _ = panel.update(cx, |panel, cx| {
                                let matching = panel.requests.ready()
                                    && panel.requests.serial == serial
                                    && panel.rows.iter().any(|row| {
                                    row.entry.id == id && row.confirmed && row.entry.editable()
                                        && matches!(&operation, Operation::Remove { expected, run, .. }
                                            if *run == panel.run && expected == &row.entry.content)
                                });
                                if matching {
                                    panel.start(operation, cx);
                                } else {
                                    panel.error = Some("队列状态已改变，请刷新后重新确认取消。".into());
                                    cx.notify();
                                }
                            });
                        })))
        });
    }

    fn editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self
            .rows
            .iter()
            .find(|row| Some(row.entry.id) == self.selected)
        else {
            return div()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("选择一条输入查看内容。关闭面板不会取消输入。")
                .into_any_element();
        };
        let id = row.entry.id;
        let editable = row.confirmed && row.entry.editable();
        let mutable = editable && self.requests.ready();
        let dirty = row.draft.read(cx).value().as_ref() != row.entry.content;
        let pending = self
            .requests
            .pending
            .as_ref()
            .is_some_and(|operation| operation.id() == Some(id));
        let original = row.entry.content.clone();
        let draft = row.draft.clone();
        let compact = window.viewport_size().height < window.rem_size() * 36.;
        let state = if row.uncertain || !row.confirmed || self.requests.needs_refresh {
            "等待刷新"
        } else {
            state_label(row.entry.state)
        };
        v_flex()
            .id("input-queue-editor")
            .tab_group()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(div().text_sm().child(format!("输入 #{id} · {state}")))
            .child(
                Textarea::new(&row.draft)
                    .h(if compact { rems(4.) } else { rems(7.) })
                    .readonly(!editable || pending)
                    .aria_label(format!("输入 #{id} 的内容")),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("copy-queued-input")
                            .small()
                            .ghost()
                            .label("复制原文")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(original.clone()))
                            }),
                    )
                    .when(editable, |view| {
                        view.child(
                            Button::new("restore-queued-input")
                                .small()
                                .label("还原草稿")
                                .disabled(pending || !dirty)
                                .on_click(cx.listener(move |panel, _, window, cx| {
                                    if let Some(row) =
                                        panel.rows.iter().find(|row| row.entry.id == id)
                                    {
                                        let original = row.entry.content.clone();
                                        draft.update(cx, |draft, cx| {
                                            draft.set_value(original, window, cx)
                                        });
                                    }
                                })),
                        )
                        .child(
                            Button::new("cancel-queued-input")
                                .small()
                                .label("取消输入…")
                                .disabled(!mutable)
                                .on_click(cx.listener(move |panel, _, window, cx| {
                                    panel.confirm_remove(id, window, cx)
                                })),
                        )
                        .child(
                            Button::new("save-queued-input")
                                .small()
                                .primary()
                                .label(if pending { "正在处理" } else { "保存" })
                                .disabled(!mutable || !dirty)
                                .on_click(cx.listener(move |panel, _, _, cx| panel.save(id, cx))),
                        )
                    }),
            )
            .into_any_element()
    }
}

fn present(panel: Entity<InputQueue>, window: &mut Window, cx: &mut App) {
    window.open_sheet(cx, move |sheet, window, cx| {
        // Dismissing this sheet does not drop the panel-owned drafts or pending
        // receipts. The Workspace retains the entity for the same scope.
        let editor = panel.update(cx, |panel, cx| panel.editor(window, cx));
        sheet
            .title("输入队列")
            .size(
                (window.rem_size() * 42.)
                    .min((window.viewport_size().width - window.rem_size() * 2.).max(px(0.))),
            )
            .child(panel.clone())
            .footer(editor)
    });
}

impl Render for InputQueue {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = matches!(self.requests.pending, Some(Operation::List { .. }));
        let focus = self.list.focus_handle(cx);
        let compact = window.viewport_size().height < window.rem_size() * 36.;
        let row = self
            .rows
            .iter()
            .find(|row| Some(row.entry.id) == self.selected);
        // Sheet owns the body viewport. This content uses natural height and
        // never introduces a percentage-height body inside that scroll owner.
        v_flex()
            .id("input-queue")
            .tab_group()
            .w_full()
            .min_w_0()
            .py_2()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!("{} 条输入 · 按提交顺序", self.rows.len())),
                    )
                    .child(
                        Button::new("refresh-input-queue")
                            .small()
                            .ghost()
                            .label("刷新")
                            .disabled(!self.requests.connected || self.requests.pending.is_some())
                            .on_click(cx.listener(|panel, _, _, cx| panel.load(None, cx))),
                    ),
            )
            .when(loading, |view| {
                view.child(
                    h_flex()
                        .gap_2()
                        .child(Spinner::new().small())
                        .child("正在加载输入队列…"),
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
                        .h(if compact { rems(5.) } else { rems(12.) })
                        .max_h(window.viewport_size().height * 0.3)
                        .min_h_0()
                        .min_w_0()
                        .flex_shrink_0()
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
                    div()
                        .text_sm()
                        .whitespace_normal()
                        .text_color(cx.theme().muted_foreground)
                        .child(if !row.confirmed {
                            "此输入尚未加载最新状态。加载后续页或刷新后才能修改；本地草稿仍可复制。"
                        } else {
                            state_explanation(row.entry.state)
                        }),
                )
                .when_some(
                    row.error
                        .clone()
                        .filter(|error| self.error.as_ref() != Some(error)),
                    |view, error| {
                        view.child(
                            div()
                                .text_sm()
                                .whitespace_normal()
                                .text_color(cx.theme().danger)
                                .child(error),
                        )
                    },
                )
                .when(row.uncertain, |view| {
                    view.child(
                        div()
                            .text_sm()
                            .whitespace_normal()
                            .text_color(cx.theme().warning)
                            .child("上次操作结果尚未确认。先刷新队列，再决定是否重新提交。"),
                    )
                })
            })
            .when(self.rows.is_empty() && self.loaded && !loading, |view| {
                view.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("暂无等待处理的输入。任务运行时发送的后续消息会显示在这里。"),
                )
            })
            .when_some(self.next, |view, after| {
                view.child(
                    Button::new("load-more-queued-inputs")
                        .small()
                        .label("加载后续输入")
                        .disabled(
                            self.requests.pending.is_some()
                                || !self.requests.connected
                                || self.requests.needs_refresh,
                        )
                        .on_click(cx.listener(move |panel, _, _, cx| panel.load(Some(after), cx))),
                )
            })
    }
}

fn state_label(state: State) -> &'static str {
    match state {
        State::Queued => "等待读取",
        State::Claimed => "Agent 已读取",
        State::Finishing => "任务正在结束",
        State::Idle => "等待下次任务",
        State::Unavailable => "当前不可修改",
    }
}

fn state_explanation(state: State) -> &'static str {
    match state {
        State::Queued => "Agent 尚未读取，可保存修改或取消这条输入。",
        State::Idle => "这条输入保留在会话中，等待下次任务处理。可以修改或取消。",
        State::Claimed => "Agent 已领取这条输入，无法再编辑或撤销。可以查看和复制内容。",
        State::Finishing => "Agent 正在结束这一轮任务，队列暂时只读。刷新后可查看最新状态。",
        State::Unavailable => "这条输入已离开队列，或由其他连接处理。当前只读，本地草稿已保留。",
    }
}

struct QueueSummary {
    id: i64,
    title: SharedString,
    state: &'static str,
}
struct QueueList {
    owner: WeakEntity<InputQueue>,
    items: Vec<QueueSummary>,
    selected: Option<IndexPath>,
}

impl ListDelegate for QueueList {
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
            ListItem::new(SharedString::from(format!("queued-input-{}", item.id)))
                .h_16()
                .px_3()
                .confirmed(self.selected == Some(index))
                .accessibility_label(format!("输入 #{}，{}，{}", item.id, item.state, item.title))
                .child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .gap_1()
                        .child(div().text_sm().text_ellipsis().child(item.title.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("#{} · {}", item.id, item.state)),
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
            .map(|item| item.id);
        let owner = self.owner.clone();
        cx.defer(move |cx| {
            let _ = owner.update(cx, |panel, cx| {
                let actual = panel.list.read(cx).selected_index().and_then(|index| {
                    panel
                        .list
                        .read(cx)
                        .delegate()
                        .items
                        .get(index.row)
                        .map(|item| item.id)
                });
                if actual != id
                    || id.is_some_and(|id| !panel.rows.iter().any(|row| row.entry.id == id))
                {
                    return;
                }
                panel.selected = id;
                cx.notify();
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DraftRefresh, Operation, Requests, acknowledged_draft, keep_absent_draft, refresh_draft,
        valid_entry, valid_page,
    };
    use orbit_protocol::input_queue::{Action, Entry, State};

    fn ready() -> Requests {
        let mut requests = Requests::new();
        let serial = requests.begin(Operation::List { after: None }).unwrap();
        requests.finish(serial);
        requests.needs_refresh = false;
        requests
    }

    #[test]
    fn reconnect_requires_refresh_and_never_replays_an_unconfirmed_edit() {
        let mut requests = ready();
        let edit = Operation::Edit {
            id: 72,
            expected: "  original\n".into(),
            content: "draft\nsecond line".into(),
            run: Some(8),
        };
        let old = requests.begin(edit.clone()).unwrap();
        assert_eq!(requests.disconnected(), Some(edit.clone()));
        requests.connected = true;
        assert!(requests.pending.is_none());
        assert!(requests.begin(edit).is_none());
        let current = requests.begin(Operation::List { after: None }).unwrap();
        assert!(!requests.finish(old));
        assert!(requests.accepts(current));
        assert!(requests.finish(current));
    }

    #[test]
    fn cancelled_or_late_receipts_cannot_clear_a_new_request() {
        let mut requests = ready();
        let old = requests
            .begin(Operation::Remove {
                id: 11,
                expected: "exact \n".into(),
                run: None,
            })
            .unwrap();
        requests.disconnected();
        requests.connected = true;
        let new = requests.begin(Operation::List { after: None }).unwrap();
        assert!(!requests.finish(old));
        assert!(matches!(
            requests.pending,
            Some(Operation::List { after: None })
        ));
        assert!(requests.finish(new));
        assert!(!requests.finish(new));
    }

    #[test]
    fn mutations_send_real_id_run_and_exact_untrimmed_compare_value() {
        let operation = Operation::Edit {
            id: 992,
            expected: "  stored\n\n".into(),
            content: " replacement\n".into(),
            run: Some(312),
        };
        match operation.action() {
            Action::Edit {
                id,
                expected,
                content,
                run,
            } => {
                assert_eq!(id, 992);
                assert_eq!(expected, "  stored\n\n");
                assert_eq!(content, " replacement\n");
                assert_eq!(run, Some(312));
            }
            _ => panic!("expected edit"),
        }
    }

    #[test]
    fn pagination_rejects_nonadvancing_ids_and_cursor_mismatches() {
        let entry = |id| Entry {
            id,
            content: "input".into(),
            state: State::Queued,
        };
        assert!(valid_page(None, &[entry(4), entry(10)], Some(10)));
        assert!(valid_page(Some(10), &[entry(12)], None));
        assert!(!valid_page(Some(10), &[entry(10)], Some(10)));
        assert!(!valid_page(None, &[entry(4), entry(4)], None));
        assert!(!valid_page(None, &[], Some(4)));
        assert!(!valid_page(None, &[entry(4)], Some(5)));
    }

    #[test]
    fn acknowledged_normalization_preserves_a_newer_local_draft() {
        assert_eq!(
            acknowledged_draft(" edited\n", " edited\n", "edited"),
            Some("edited")
        );
        assert_eq!(
            acknowledged_draft("newer unsent draft", " edited\n", "edited"),
            None
        );
        assert_eq!(
            acknowledged_draft(" edited\n", " edited\n", "unexpected"),
            None
        );
    }

    #[test]
    fn refresh_updates_clean_text_but_preserves_local_or_unknown_edits() {
        assert_eq!(refresh_draft("A", "A", "C", false), DraftRefresh::Replace);
        assert_eq!(
            refresh_draft("local", "A", "C", false),
            DraftRefresh::Conflict
        );
        assert_eq!(
            refresh_draft("local", "A", "A", false),
            DraftRefresh::Preserve
        );
        assert_eq!(refresh_draft("A", "A", "C", true), DraftRefresh::Uncertain);
        assert_eq!(
            refresh_draft(" C\n", "A", "C", true),
            DraftRefresh::Preserve
        );
    }

    #[test]
    fn completed_pagination_only_retains_unsaved_or_unconfirmed_absent_text() {
        assert!(!keep_absent_draft("A", "A", false));
        assert!(keep_absent_draft("local", "A", false));
        assert!(keep_absent_draft("A", "A", true));
    }

    #[test]
    fn contradictory_mutable_state_and_run_cannot_enable_controls() {
        let entry = |state| Entry {
            id: 9,
            content: "input".into(),
            state,
        };
        assert!(valid_entry(&entry(State::Queued), Some(4)));
        assert!(valid_entry(&entry(State::Idle), None));
        assert!(!valid_entry(&entry(State::Queued), None));
        assert!(!valid_entry(&entry(State::Idle), Some(4)));
        assert!(!valid_entry(&entry(State::Queued), Some(0)));
    }
}
