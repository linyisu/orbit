//! The service owns the append-only question indices. Native rows retain those
//! indices, their text selection, and their scroll state across revisions.
use super::super::*;
use gpui_kit::component::{
    IndexPath,
    list::{List, ListDelegate, ListItem, ListState},
    message_scroller::{MessageScroller, MessageScrollerState},
    spinner::Spinner,
    text::TextViewState,
};
use orbit_protocol::{Questions, SideQuestion};

pub(in crate::workspace) struct SideQuestions {
    id: EntityId,
    owner: WeakEntity<Workspace>,
    options: Options,
    session: String,
    revision: u64,
    records: Vec<Entity<QuestionDetail>>,
    selected: Option<usize>,
    list: Entity<ListState<QuestionList>>,
    input: Entity<TextareaState>,
    new_question: bool,
    running: bool,
    pending: bool,
    stopping: Option<usize>,
    closed: bool,
    error: Option<SharedString>,
    width: Option<Pixels>,
    request: Task<()>,
    _subscription: Subscription,
}

impl Workspace {
    pub(in crate::workspace) fn open_side_questions(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = &self.info else { return };
        if let Some(panel) = &self.side_questions
            && panel.read(cx).belongs_to(&info.id, &self.options)
            && !panel.read(cx).closed
        {
            let panel = panel.clone();
            present(panel.clone(), window, cx);
            let input = panel.read(cx).input.clone();
            input.update(cx, |input, cx| input.focus(window, cx));
            return;
        }
        self.command(
            Command::Request(Request::OpenQuestions {
                session: info.id.clone(),
            }),
            cx,
        );
    }

    pub(in crate::workspace) fn show_questions(
        &mut self,
        questions: Questions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = &self.info else { return };
        let existing = self
            .side_questions
            .as_ref()
            .filter(|panel| panel.read(cx).belongs_to(&info.id, &self.options));
        // A queued polling response must never reopen a sheet the user closed.
        let open = questions.selected.is_some();
        if let Some(panel) = existing
            && !panel.read(cx).closed
        {
            panel.update(cx, |panel, cx| panel.receive(questions, window, cx));
            return;
        }
        if !open || questions.records.is_none() {
            return;
        }
        let owner = cx.weak_entity();
        let session = info.id.clone();
        let options = self.options.clone();
        let panel = cx.new(|cx| SideQuestions::new(owner, options, session, window, cx));
        panel.update(cx, |panel, cx| panel.receive(questions, window, cx));
        self.side_questions = Some(panel.clone());
        if self.visible {
            present(panel, window, cx);
        }
    }
}

impl SideQuestions {
    fn new(
        owner: WeakEntity<Workspace>,
        options: Options,
        session: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("输入旁路问题…")
                .auto_grow(2, 5)
                .submit_on_enter(true)
        });
        let subscription = cx.subscribe_in(&input, window, |panel, _, event, _, cx| match event {
            InputEvent::Change => {
                panel.error = None;
                cx.notify();
            }
            InputEvent::PressEnter { shift: false, .. } => panel.ask(cx),
            _ => {}
        });
        let list_owner = cx.weak_entity();
        Self {
            id: cx.entity().entity_id(),
            owner,
            options,
            session,
            revision: 0,
            records: Vec::new(),
            selected: None,
            list: cx.new(|cx| {
                ListState::new(
                    QuestionList {
                        owner: list_owner,
                        items: Vec::new(),
                        selected: None,
                    },
                    window,
                    cx,
                )
            }),
            input,
            new_question: false,
            running: false,
            pending: false,
            stopping: None,
            closed: false,
            error: None,
            width: None,
            request: Task::ready(()),
            _subscription: subscription,
        }
    }

    fn belongs_to(&self, session: &str, options: &Options) -> bool {
        self.session == session && self.options.same_host(options)
    }

    fn current_scope(&self, cx: &App) -> bool {
        self.owner.upgrade().is_some_and(|owner| {
            let workspace = owner.read(cx);
            workspace.connected
                && workspace
                    .side_questions
                    .as_ref()
                    .is_some_and(|panel| panel.entity_id() == self.id)
                && workspace.options.same_host(&self.options)
                && workspace
                    .info
                    .as_ref()
                    .is_some_and(|info| info.id == self.session)
        })
    }

    fn receive(&mut self, questions: Questions, window: &mut Window, cx: &mut Context<Self>) {
        // Workspace calls this while holding its own entity lease. The caller
        // has already checked the session/host; reading it here would reenter it.
        if self.closed || questions.revision < self.revision {
            return;
        }
        self.revision = questions.revision;
        self.running = questions.running;
        if let Some(records) = questions.records {
            // The live service only appends records. A shorter snapshot belongs
            // to a freshly opened viewer and must discard the old row states.
            if records.len() < self.records.len() {
                self.records.clear();
                self.selected = None;
            }
            for (index, record) in records.into_iter().enumerate() {
                if let Some(detail) = self.records.get(index) {
                    detail.update(cx, |detail, cx| detail.receive(record, cx));
                } else {
                    self.records
                        .push(cx.new(|cx| QuestionDetail::new(record, cx)));
                }
            }
        }
        if let Some(index) = questions
            .selected
            .filter(|index| *index < self.records.len())
        {
            self.selected = Some(index);
            self.new_question = false;
        } else if self.selected.is_none() {
            self.selected = self.records.len().checked_sub(1);
        }
        if self.stopping.is_some_and(|index| {
            self.records
                .get(index)
                .is_none_or(|record| record.read(cx).record.status != "running")
        }) || !self.running
        {
            self.stopping = None;
        }
        self.sync_list(window, cx);
        cx.notify();
    }

    fn sync_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = self
            .records
            .iter()
            .enumerate()
            .map(|(index, detail)| {
                let record = &detail.read(cx).record;
                QuestionSummary {
                    index,
                    id: format!("{}-{index}", record.thread_id).into(),
                    question: record.question.clone().into(),
                    status: record.status.clone().into(),
                }
            })
            .collect();
        let selected = self.selected.map(IndexPath::new);
        self.list.update(cx, |list, cx| {
            list.delegate_mut().items = items;
            list.set_selected_index(selected, window, cx);
            cx.notify();
        });
    }

    fn ask(&mut self, cx: &mut Context<Self>) {
        if self.pending || self.running || self.closed {
            return;
        }
        let question = self.input.read(cx).value().trim().to_string();
        if question.is_empty() {
            return;
        }
        if question.chars().count() > 32_768 {
            self.error = Some("旁路问题不能超过 32768 个字符。".into());
            cx.notify();
            return;
        }
        let follow_up = (!self.new_question).then_some(self.selected).flatten();
        self.send(
            Request::Ask {
                session: self.session.clone(),
                question,
                follow_up,
            },
            true,
            cx,
        );
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected else { return };
        if self.pending
            || self.stopping.is_some()
            || !self.running
            || self.closed
            || self
                .records
                .get(index)
                .is_none_or(|record| record.read(cx).record.status != "running")
        {
            return;
        }
        self.stopping = Some(index);
        self.send(
            Request::CancelQuestion {
                session: self.session.clone(),
                index,
            },
            false,
            cx,
        );
    }

    fn send(&mut self, request: Request, asking: bool, cx: &mut Context<Self>) {
        if !self.current_scope(cx) {
            self.error = Some("会话连接已改变，请重新打开旁路问答。".into());
            self.stopping = None;
            cx.notify();
            return;
        }
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
            self.error = Some("连接已中断，请重新连接后重试。".into());
            self.stopping = None;
            cx.notify();
            return;
        }
        self.pending = true;
        self.error = None;
        self.request = cx.spawn(async move |panel, cx| {
            let result = response
                .await
                .unwrap_or_else(|_| Err("连接已中断，无法确认旁路操作结果。".into()));
            let _ = panel.update_in(cx, |panel, window, cx| {
                if panel.closed || !panel.current_scope(cx) {
                    return;
                }
                panel.pending = false;
                match result {
                    Ok(Response::Questions(questions)) if asking => {
                        panel.receive(questions, window, cx);
                        panel
                            .input
                            .update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    // Cancel acknowledgement only accepts the request. Keep the
                    // stopping state until a revision confirms its final status.
                    Ok(Response::Ok) if !asking => {}
                    Ok(_) => {
                        panel.error = Some("服务未返回旁路操作结果，请更新服务后重试。".into());
                        panel.stopping = None;
                    }
                    Err(error) => {
                        panel.error = Some(error.into());
                        panel.stopping = None;
                    }
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.request = Task::ready(());
        if self.current_scope(cx) {
            let session = self.session.clone();
            let _ = self.owner.update(cx, |workspace, cx| {
                workspace.command(Command::Request(Request::CloseQuestions { session }), cx);
            });
        }
        self.records.clear();
    }
}

impl Render for SideQuestions {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let narrow = self
            .width
            .is_none_or(|width| width < window.rem_size() * 42.);
        let measured_width = self.width;
        let owner = cx.weak_entity();
        let ready = !self.input.read(cx).value().trim().is_empty();
        let selected_running = self
            .selected
            .and_then(|index| self.records.get(index))
            .is_some_and(|record| record.read(cx).record.status == "running");
        let focus = self.list.focus_handle(cx);
        v_flex()
            .id("side-questions")
            .tab_group()
            .relative()
            .size_full()
            .min_h_0()
            .min_w_0()
            .gap_3()
            .p_4()
            .on_action(cx.listener(|panel, _: &SendMessage, _, cx| panel.ask(cx)))
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} 条问答 · 关闭面板会结束本次旁路对话",
                                self.records.len()
                            )),
                    )
                    .child(
                        Button::new("side-new")
                            .small()
                            .ghost()
                            .icon(IconName::Plus)
                            .label("新问题")
                            .disabled(self.pending)
                            .on_click(cx.listener(|panel, _, window, cx| {
                                panel.new_question = true;
                                panel.input.update(cx, |input, cx| input.focus(window, cx));
                                cx.notify();
                            })),
                    ),
            )
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let width = bounds.size.width;
                        if width <= px(0.) || measured_width == Some(width) {
                            return;
                        }
                        let owner = owner.clone();
                        cx.defer(move |cx| {
                            let _ = owner.update(cx, |panel, cx| {
                                if panel.width != Some(width) {
                                    panel.width = Some(width);
                                    cx.notify();
                                }
                            });
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .gap_3()
                    .items_stretch()
                    .when(narrow, |row| row.flex_col())
                    .child(
                        div()
                            .min_h_0()
                            .min_w_0()
                            .border_1()
                            .border_color(cx.theme().border)
                            .rounded_lg()
                            .when(narrow, |list| list.w_full().h(rems(10.)))
                            .when(!narrow, |list| list.w(rems(16.)).flex_shrink_0())
                            .track_focus(&focus)
                            .tab_index(0)
                            .child(List::new(&self.list)),
                    )
                    .child(
                        div().flex_1().min_h_0().min_w_0().when_some(
                            self.selected
                                .and_then(|index| self.records.get(index).cloned()),
                            |detail, record| detail.child(record),
                        ),
                    ),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .child(
                v_flex()
                    .gap_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .pt_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(if self.new_question {
                                        "新旁路问题"
                                    } else {
                                        "继续所选问题"
                                    }),
                            )
                            .when(selected_running || self.stopping.is_some(), |row| {
                                row.child(
                                    Button::new("side-stop")
                                        .small()
                                        .label(if self.stopping.is_some() {
                                            "正在停止…"
                                        } else {
                                            "停止生成"
                                        })
                                        .disabled(self.pending || self.stopping.is_some())
                                        .on_click(cx.listener(|panel, _, _, cx| panel.stop(cx))),
                                )
                            }),
                    )
                    .child(
                        Textarea::new(&self.input)
                            .disabled(self.pending)
                            .aria_label("旁路问题"),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if self.running {
                                        "回答生成中，可以先写下一个问题"
                                    } else {
                                        "Enter 提问 · Shift+Enter 换行"
                                    }),
                            )
                            .child(
                                Button::new("side-send")
                                    .primary()
                                    .small()
                                    .label(if self.pending && self.stopping.is_none() {
                                        "发送中…"
                                    } else {
                                        "提问"
                                    })
                                    .disabled(self.pending || self.running || !ready)
                                    .on_click(cx.listener(|panel, _, _, cx| panel.ask(cx))),
                            ),
                    ),
            )
    }
}

struct QuestionDetail {
    record: SideQuestion,
    text: Entity<TextViewState>,
    scroll: Entity<MessageScrollerState>,
}

impl QuestionDetail {
    fn new(record: SideQuestion, cx: &mut Context<Self>) -> Self {
        Self {
            text: cx.new(|cx| TextViewState::markdown(&record.answer, cx)),
            scroll: cx.new(|cx| MessageScrollerState::new(1, cx)),
            record,
        }
    }

    fn receive(&mut self, record: SideQuestion, cx: &mut Context<Self>) {
        let changed = self.record.answer != record.answer
            || self.record.status != record.status
            || self.record.error != record.error;
        if self.record.answer != record.answer {
            self.text
                .update(cx, |text, cx| text.set_text(&record.answer, cx));
        }
        if changed {
            self.scroll
                .update(cx, |scroll, cx| scroll.remeasure_items(0..1, cx));
        }
        self.record = record;
        cx.notify();
    }
}

fn present(panel: Entity<SideQuestions>, window: &mut Window, cx: &mut App) {
    window.open_sheet(cx, move |sheet, window, _| {
        let close = panel.downgrade();
        sheet
            .title("旁路问答")
            .size(
                (window.rem_size() * 54.)
                    .min(window.viewport_size().width - window.rem_size() * 2.),
            )
            .overlay_closable(false)
            .child(div().size_full().child(panel.clone()))
            .on_close(move |_, _, cx| {
                let _ = close.update(cx, |panel, cx| panel.close(cx));
            })
    });
}

impl Render for QuestionDetail {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        MessageScroller::new("side-answer", self.scroll.clone(), move |_, _, cx| {
            let Some(owner) = owner.upgrade() else {
                return div().into_any_element();
            };
            let detail = owner.read(cx);
            v_flex()
                .w_full()
                .min_w_0()
                .gap_3()
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .whitespace_normal()
                        .child(detail.record.question.clone()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "{} · {}",
                            status_label(&detail.record.status),
                            detail.record.model
                        )),
                )
                .when(
                    detail.record.answer.is_empty() && detail.record.status == "running",
                    |view| {
                        view.child(
                            h_flex()
                                .gap_2()
                                .child(Spinner::new().small())
                                .child("正在生成回答…"),
                        )
                    },
                )
                .child(
                    TextView::new(&detail.text)
                        .w_full()
                        .selectable(true)
                        .style(crate::theme::document_style())
                        .plugin(crate::math::MathPlugin::inline())
                        .plugin(crate::math::MathPlugin::display()),
                )
                .when_some(detail.record.error.clone(), |view, error| {
                    view.child(div().text_sm().text_color(cx.theme().danger).child(error))
                })
                .into_any_element()
        })
        .with_row_style(StyleRefinement::default().p_3())
        .size_full()
    }
}

struct QuestionSummary {
    index: usize,
    id: SharedString,
    question: SharedString,
    status: SharedString,
}

struct QuestionList {
    owner: WeakEntity<SideQuestions>,
    items: Vec<QuestionSummary>,
    selected: Option<IndexPath>,
}

impl ListDelegate for QuestionList {
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
        let owner = self.owner.upgrade()?;
        Some(
            ListItem::new(item.id.clone())
                .h_16()
                .px_3()
                .confirmed(owner.read(cx).selected == Some(item.index))
                .accessibility_label(format!("{}，{}", item.question, status_label(&item.status)))
                .child(
                    v_flex()
                        .min_w_0()
                        .w_full()
                        .gap_1()
                        .child(div().text_sm().text_ellipsis().child(item.question.clone()))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(status_label(&item.status)),
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
        let Some(index) = selected
            .and_then(|selected| self.items.get(selected.row))
            .map(|item| item.index)
        else {
            return;
        };
        let owner = self.owner.clone();
        // Programmatic revision updates also call this delegate while the
        // parent is leased. Defer selection so keyboard navigation remains live
        // without borrowing SideQuestions again inside sync_list.
        cx.defer(move |cx| {
            let _ = owner.update(cx, |panel, cx| {
                panel.selected = Some(index);
                panel.new_question = false;
                panel.error = None;
                cx.notify();
            });
        });
    }

    fn confirm(&mut self, _: bool, _: &mut Window, cx: &mut Context<ListState<Self>>) {
        let Some(index) = self
            .selected
            .and_then(|selected| self.items.get(selected.row))
            .map(|item| item.index)
        else {
            return;
        };
        let _ = self.owner.update(cx, |panel, cx| {
            panel.selected = Some(index);
            panel.new_question = false;
            panel.error = None;
            cx.notify();
        });
    }
}

fn status_label(status: &str) -> &'static str {
    match status {
        "running" => "生成中",
        "completed" => "已完成",
        "cancelled" => "已停止",
        "failed" => "失败",
        _ => "未知状态",
    }
}
