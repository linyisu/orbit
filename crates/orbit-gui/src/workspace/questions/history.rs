//! Read-only, cursor-backed Agent question history. Live answer forms are never
//! replaced by history pages, including while an older page is loading.
use super::super::*;
#[path = "history/state.rs"]
mod state;
use gpui_kit::component::{
    IndexPath,
    list::{List, ListDelegate, ListItem, ListState},
    spinner::Spinner,
    text::TextViewState,
};
use orbit_protocol::user_input::{Record, Status};
use state::HistoryState;

pub(in crate::workspace) struct QuestionHistory {
    id: EntityId,
    owner: WeakEntity<Workspace>,
    options: Options,
    session: String,
    workspace: String,
    state: HistoryState,
    details: Vec<Entity<HistoryDetail>>,
    list: Entity<ListState<HistoryList>>,
    closed: bool,
    request: Task<()>,
}

impl Workspace {
    pub(in crate::workspace) fn open_question_history(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = &self.info else { return };
        if let Some(panel) = &self.question_history
            && !panel.read(cx).closed
            && panel
                .read(cx)
                .belongs_to(&info.id, &info.workspace, &self.options)
        {
            let panel = panel.clone();
            present(panel.clone(), window, cx);
            panel
                .read(cx)
                .list
                .focus_handle(cx)
                .focus(window, cx);
            return;
        }
        let owner = cx.weak_entity();
        let options = self.options.clone();
        let session = info.id.clone();
        let workspace = info.workspace.clone();
        let panel =
            cx.new(|cx| QuestionHistory::new(owner, options, session, workspace, window, cx));
        self.question_history = Some(panel.clone());
        present(panel.clone(), window, cx);
        // Workspace is currently leased. Wait until it is released before the
        // panel checks the captured scope and sends its correlated request.
        let panel = panel.downgrade();
        cx.defer(move |cx| {
            let _ = panel.update(cx, |panel, cx| panel.load(cx));
        });
    }
}

impl QuestionHistory {
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
            state: HistoryState::default(),
            details: Vec::new(),
            list: cx.new(|cx| {
                ListState::new(
                    HistoryList {
                        owner: list_owner,
                        items: Vec::new(),
                        selected: None,
                    },
                    window,
                    cx,
                )
            }),
            closed: false,
            request: Task::ready(()),
        }
    }

    fn belongs_to(&self, session: &str, workspace: &str, options: &Options) -> bool {
        self.session == session && self.workspace == workspace && self.options.same_host(options)
    }

    fn current_scope(&self, cx: &App) -> bool {
        self.owner
            .upgrade()
            .is_some_and(|owner| {
                let workspace = owner.read(cx);
                workspace.connected
                    && workspace
                        .question_history
                        .as_ref()
                        .is_some_and(|panel| panel.entity_id() == self.id)
                    && workspace
                        .info
                        .as_ref()
                        .is_some_and(|info| {
                            self.belongs_to(&info.id, &info.workspace, &workspace.options)
                        })
            })
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if self.closed || self.state.pending.is_some() {
            return;
        }
        if !self.current_scope(cx) {
            self.state.error = Some("会话连接已改变，请重新打开问答历史。".into());
            cx.notify();
            return;
        }
        let Some(page) = self.state.begin() else {
            return;
        };
        let request = Request::UserInputHistory {
            session: self.session.clone(),
            before: page.before,
        };
        let (result, response) = tokio::sync::oneshot::channel();
        let sent = self
            .owner
            .update(cx, |workspace, _| {
                workspace
                    .backend
                    .as_ref()
                    .is_some_and(|backend| {
                        backend
                            .send(Command::Settings { request, result })
                            .is_ok()
                    })
            })
            .unwrap_or(false);
        if !sent {
            self.state
                .receive(page, Err("连接繁忙或已断开，请重试。".into()));
            cx.notify();
            return;
        }
        self.request = cx.spawn(async move |panel, cx| {
            let result = response
                .await
                .unwrap_or_else(|_| Err("连接已中断，请重试加载历史。".into()))
                .and_then(|response| match response {
                    Response::UserInputs(history) => Ok(history),
                    _ => Err("服务未返回问答历史，请更新服务后重试。".into()),
                });
            let _ = panel.update_in(cx, |panel, window, cx| {
                // Entity identity is the viewer generation. Reopening the same
                // session after reconnect must not accept this viewer's reply.
                if panel.closed || !panel.current_scope(cx) {
                    return;
                }
                if panel.state.receive(page, result) {
                    panel.sync_list(window, cx);
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    fn sync_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (index, record) in self.state.records.iter().enumerate() {
            if let Some(detail) = self.details.get(index) {
                detail.update(cx, |detail, cx| {
                    if detail.record != *record {
                        detail.text.update(cx, |text, cx| {
                            text.set_text(&record_text(record), cx);
                        });
                        detail.record = record.clone();
                        cx.notify();
                    }
                });
            } else {
                self.details
                    .push(cx.new(|cx| HistoryDetail {
                        record: record.clone(),
                        text: cx.new(|cx| TextViewState::markdown(&record_text(record), cx)),
                    }));
            }
        }
        let selected = self
            .state
            .records
            .iter()
            .position(|record| Some(&record.id) == self.state.selected.as_ref())
            .map(IndexPath::new);
        let items = self
            .state
            .records
            .iter()
            .map(|record| HistorySummary {
                id: record.id.clone().into(),
                title: record
                    .form
                    .questions
                    .iter()
                    .map(|question| question.title.as_str())
                    .collect::<Vec<_>>()
                    .join(" · ")
                    .into(),
                status: record.status,
                source: record.source.clone().map(Into::into),
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

    fn close(&mut self) {
        self.closed = true;
        self.state.invalidate();
        self.request = Task::ready(());
    }
}

fn present(panel: Entity<QuestionHistory>, window: &mut Window, cx: &mut App) {
    window.open_sheet(cx, move |sheet, window, _| {
        let close = panel.downgrade();
        sheet
            .title("Agent 问答历史")
            .size(
                (window.rem_size() * 48.)
                    .min((window.viewport_size().width - window.rem_size() * 2.).max(px(0.))),
            )
            .child(div().size_full().child(panel.clone()))
            .on_close(move |_, _, cx| {
                let _ = close.update(cx, |panel, _| panel.close());
            })
    });
}

impl Render for QuestionHistory {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = self.state.pending.is_some();
        let complete = self.state.loaded && self.state.next.is_none();
        let focus = self.list.focus_handle(cx);
        let selected = self
            .state
            .records
            .iter()
            .position(|record| Some(&record.id) == self.state.selected.as_ref())
            .and_then(|index| self.details.get(index).cloned());
        v_flex()
            .id("agent-question-history")
            .tab_group()
            .size_full()
            .min_w_0()
            .min_h_0()
            .gap_3()
            .p_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("已加载 {} 条 · 从新到旧", self.state.records.len())),
            )
            .when(!self.state.records.is_empty(), |view| {
                view.child(
                    div()
                        .h(rems(12.))
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
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .when_some(selected, |view, detail| view.child(detail))
                    .when(self.state.records.is_empty() && !loading, |view| {
                        view.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(if self.state.loaded {
                                    "暂无问答记录"
                                } else {
                                    "加载历史后可查看 Agent 的问题与已提交的答案。"
                                }),
                        )
                    }),
            )
            .child(
                v_flex()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .pt_3()
                    .when_some(self.state.error.clone(), |view, error| {
                        view.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child(
                                    orbit_client::directories::display_diagnostic(error.as_ref())
                                        .into_owned(),
                                ),
                        )
                    })
                    .when(loading, |view| {
                        view.child(
                            h_flex()
                                .gap_2()
                                .child(Spinner::new().small())
                                .child(if self.state.loaded {
                                    "正在加载更早记录…"
                                } else {
                                    "正在加载问答历史…"
                                }),
                        )
                    })
                    .when(!complete, |view| {
                        view.child(
                            Button::new("load-question-history")
                                .small()
                                .w_full()
                                .label(if self.state.error.is_some() {
                                    "重试加载"
                                } else if self.state.loaded {
                                    "加载更早记录"
                                } else {
                                    "加载问答历史"
                                })
                                .disabled(loading)
                                .on_click(cx.listener(|panel, _, _, cx| panel.load(cx))),
                        )
                    })
                    .when(complete && !self.state.records.is_empty(), |view| {
                        view.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("已显示全部问答记录"),
                        )
                    }),
            )
    }
}

struct HistorySummary {
    id: SharedString,
    title: SharedString,
    status: Status,
    source: Option<SharedString>,
}

struct HistoryList {
    owner: WeakEntity<QuestionHistory>,
    items: Vec<HistorySummary>,
    selected: Option<IndexPath>,
}

impl ListDelegate for HistoryList {
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
                .accessibility_label(format!("{}，{}", item.title, status_label(item.status)))
                .child(
                    v_flex()
                        .min_w_0()
                        .w_full()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .text_ellipsis()
                                .child(item.title.clone()),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .text_ellipsis()
                                .child(match &item.source {
                                    Some(source) => {
                                        format!("{} · {source}", status_label(item.status))
                                    }
                                    None => status_label(item.status).to_owned(),
                                }),
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
                if !panel.closed {
                    panel.state.selected = id;
                    cx.notify();
                }
            });
        });
    }
}

struct HistoryDetail {
    record: Record,
    text: Entity<TextViewState>,
}

impl Render for HistoryDetail {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("question-history-detail")
            .size_full()
            .min_w_0()
            .overflow_y_scroll()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(status_label(self.record.status)),
            )
            .child(
                TextView::new(&self.text)
                    .w_full()
                    .selectable(true)
                    .style(crate::theme::document_style(cx))
                    .plugin(crate::math::MathPlugin::inline())
                    .plugin(crate::math::MathPlugin::display()),
            )
    }
}

fn status_label(status: Status) -> &'static str {
    match status {
        Status::Pending => "待回复",
        Status::Answered => "已回复",
        Status::Declined => "已跳过",
        Status::Cancelled => "已取消",
        Status::Interrupted => "已中断",
    }
}

fn record_text(record: &Record) -> String {
    record
        .form
        .questions
        .iter()
        .map(|question| {
            let answer = record
                .answers
                .iter()
                .find(|answer| answer.question_id == question.id);
            let mut text = format!("### {}\n\n{}", question.title, question.prompt);
            if let Some(answer) = answer {
                let choices = answer
                    .selected
                    .iter()
                    .map(|id| {
                        question
                            .options
                            .iter()
                            .find(|option| &option.id == id)
                            .map_or(id.as_str(), |option| option.label.as_str())
                    })
                    .collect::<Vec<_>>()
                    .join("、");
                text.push_str("\n\n**已提交的回复**");
                for value in [choices.as_str(), answer.text.as_str(), answer.note.as_str()] {
                    if !value.trim().is_empty() {
                        text.push_str("\n\n");
                        text.push_str(value);
                    }
                }
            } else {
                text.push_str("\n\n尚未提交回复。");
            }
            text
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}
