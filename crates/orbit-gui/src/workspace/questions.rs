use super::*;
use gpui_kit::base::{SelectableText, TextSelection};
#[path = "questions/history.rs"]
mod history;
#[path = "questions/side.rs"]
mod side;
use gpui_kit::component::checkbox::Checkbox;
pub(super) use history::QuestionHistory;
use orbit_protocol::user_input::{Answer, Kind, Record, Status};
pub(super) use side::SideQuestions;

/// Retained while the request is pending: polling and task renders must not
/// replace the user's selection or their unfinished written answer.
pub(super) struct QuestionForm {
    owner: WeakEntity<Workspace>,
    session: String,
    record: Record,
    answers: Vec<Answer>,
    inputs: Vec<Entity<TextareaState>>,
    current: usize,
    hidden: bool,
    error: Option<String>,
    pending: bool,
    _subscriptions: Vec<Subscription>,
}

impl QuestionForm {
    pub(super) fn disconnected(&mut self, cx: &mut Context<Self>) {
        if self.pending {
            self.pending = false;
            self.error =
                Some("连接已中断，提交结果未知。恢复后请先确认问题仍待回答，再重试。".into());
        }
        cx.notify();
    }

    pub(super) fn hidden(&self) -> bool {
        self.hidden
    }

    pub(super) fn retry(&mut self, cx: &mut Context<Self>) {
        self.pending = false;
        cx.notify();
    }

    fn new(
        owner: WeakEntity<Workspace>,
        session: String,
        record: Record,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let inputs: Vec<Entity<TextareaState>> = record
            .form
            .questions
            .iter()
            .map(|_| {
                cx.new(|cx| {
                    TextareaState::new(window, cx)
                        .placeholder("或自行撰写回复")
                        .submit_on_enter(true)
                        .auto_grow(1, 3)
                })
            })
            .collect();
        let mut subscriptions = Vec::new();
        for (index, input) in inputs.iter().enumerate() {
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                move |view, input, event, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        if view.record.form.questions[index].kind == Kind::Single
                            && !input.read(cx).value().trim().is_empty()
                        {
                            view.answers[index].selected.clear();
                        }
                        view.error = None;
                        cx.notify();
                    } else if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                        view.advance(window, cx);
                    }
                },
            ));
            let focus = input.read(cx).focus_handle(cx);
            subscriptions.push(cx.on_focus(&focus, window, |_, _, cx| cx.notify()));
            subscriptions.push(cx.on_blur(&focus, window, |_, _, cx| cx.notify()));
        }
        let answers = record
            .form
            .questions
            .iter()
            .map(|q| Answer {
                question_id: q.id.clone(),
                ..Default::default()
            })
            .collect();
        Self {
            owner,
            session,
            record,
            answers,
            inputs,
            current: 0,
            hidden: false,
            error: None,
            pending: false,
            _subscriptions: subscriptions,
        }
    }

    fn advance(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        self.answers[self.current].text = self.inputs[self.current]
            .read(cx)
            .value()
            .to_string();
        let form = orbit_protocol::user_input::Form {
            questions: vec![self.record.form.questions[self.current].clone()],
        };
        if let Err(error) = form.validate_answers(&self.answers[self.current..=self.current]) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        if self.current + 1 < self.answers.len() {
            self.current += 1;
            self.error = None;
            cx.notify();
        } else {
            self.submit(false, cx);
        }
    }

    fn submit(&mut self, decline: bool, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let current = self
            .owner
            .upgrade()
            .is_some_and(|owner| {
                let workspace = owner.read(cx);
                workspace.connected
                    && !workspace.opening
                    && workspace
                        .info
                        .as_ref()
                        .is_some_and(|info| info.id == self.session)
                    && workspace
                        .user_inputs
                        .iter()
                        .any(|record| {
                            record.id == self.record.id && record.status == Status::Pending
                        })
            });
        if !current {
            self.error = Some("请等待会话恢复并确认问题仍待回答。输入内容已保留。".into());
            cx.notify();
            return;
        }
        for (answer, input) in self
            .answers
            .iter_mut()
            .zip(&self.inputs)
        {
            answer.text = input.read(cx).value().to_string();
        }
        if !decline
            && let Err(error) = self
                .record
                .form
                .validate_answers(&self.answers)
        {
            self.error = Some(error);
            cx.notify();
            return;
        }
        let request = Request::AnswerUserInput {
            session: self.session.clone(),
            request: self.record.id.clone(),
            answers: (!decline).then(|| self.answers.clone()),
        };
        if self
            .owner
            .update(cx, |view, cx| view.command(Command::Request(request), cx))
            .unwrap_or(false)
        {
            self.pending = true;
        }
        cx.notify();
    }
}

impl Render for QuestionForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ix = self.current;
        let mut answer = self.answers[ix].clone();
        answer.text = self.inputs[ix]
            .read(cx)
            .value()
            .to_string();
        let ready = orbit_protocol::user_input::Form {
            questions: vec![self.record.form.questions[ix].clone()],
        }
        .validate_answers(&[answer])
        .is_ok();
        let question = &self.record.form.questions[ix];
        let single = question.kind == Kind::Single;
        let custom = question.allow_custom || question.kind == Kind::Text;
        let focused = self.inputs[ix]
            .read(cx)
            .focus_handle(cx)
            .is_focused(window);
        v_flex()
            .id("agent-question-card")
            .on_action(cx.listener(|view, _: &SendMessage, window, cx| view.advance(window, cx)))
            .w_full()
            .min_w_0()
            .p_3()
            .gap_3()
            .bg(cx.theme().group_box)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius_lg)
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(Icon::new(IconName::MessageCircle).small())
                    .child(if self.answers.len() > 1 {
                        format!("问题 {}/{}", ix + 1, self.answers.len())
                    } else {
                        "问题".into()
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("hide-question")
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .accessibility_label("收起问题")
                            .tooltip("收起问题")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.hidden = true;
                                let _ = view.owner.update(cx, |_, cx| cx.notify());
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .id("question-options")
                    .max_h(window.viewport_size().height * 0.4)
                    .overflow_y_scroll()
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                div()
                                    .px_2()
                                    .pb_2()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(SelectableText::new(("question-prompt",
                                        ix),
                                        question
                                        .prompt
                                        .clone())),
                            )
                            .children(question.options.iter().enumerate().map(
                                |(index, option)| {
                                    let option_id = option.id.clone();
                                    let checked = self.answers[ix].selected.contains(&option.id);
                                    gpui_kit::base::Button::new(("question-option", index))
                                        .rounded(cx.theme().radius)
                                        .text_color(cx.theme().foreground)
                                        .hover(|style| style.bg(cx.theme().accent))
                                        .focus_visible(|style| style.bg(cx.theme().accent))
                                        .styles(|styles| styles
                                            .disabled(|style| style
                                            .opacity(0.5)))
                                        .w_full()
                                        .h_auto()
                                        .py_2()
                                        .px_3()
                                        .justify_start()
                                        .disabled(self.pending)
                                        .when(checked, |button| button.bg(cx.theme().accent))
                                        .accessibility_label(format!(
                                            "{}{}",
                                            option.label,
                                            if checked { "，已选择" } else { "" }
                                        ))
                                        .child(
                                            h_flex()
                                                .w_full()
                                                .min_w_0()
                                                .gap_3()
                                                .child(
                                                    div()
                                                        .flex_shrink_0()
                                                        .size_6()
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .rounded(cx.theme().radius)
                                                        .bg(cx.theme().muted)
                                                        .text_sm()
                                                        .child((index + 1).to_string()),
                                                )
                                                .child(
                                                    v_flex()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .gap_1()
                                                        .items_start()
                                                        .child(
                                                            div()
                                                                .whitespace_normal()
                                                                .child(SelectableText::new(("question-label",
                                                                    index),
                                                                    option
                                                                    .label
                                                                    .clone())),
                                                        )
                                                        .when(
                                                            !option.description.is_empty(),
                                                            |view| {
                                                                view.child(
                                                                    div()
                                                                        .text_sm()
                                                                        .whitespace_normal()
                                                                        .text_color(
                                                                            cx.theme()
                                                                                .muted_foreground,
                                                                        )
                                                                        .child(
                                                                            SelectableText::new(("question-description",
                                                                                index),
                                                                                option
                                                                                .description
                                                                                .clone()),
                                                                        ),
                                                                )
                                                            },
                                                        ),
                                                )
                                                .when(checked, |row| {
                                                    row.child(
                                                        Icon::new(if single {
                                                            IconName::ArrowRight
                                                        } else {
                                                            IconName::Check
                                                        })
                                                        .small(),
                                                    )
                                                }),
                                        )
                                        .on_click(cx.listener(move |view, event, window, cx| {
                                            if view
                                                .pending
                                                || view
                                                .current != ix
                                                || (matches!(event,
                                                ClickEvent::Mouse(_))
                                                && TextSelection::has_selection(window,
                                                cx)) {
                                                return;
                                            }
                                            if single {
                                                view.answers[ix].selected.clear();
                                                view.inputs[ix].update(cx, |input, cx| {
                                                    input.set_value("", window, cx)
                                                });
                                            }
                                            if !checked || single {
                                                view.answers[ix].selected.push(option_id.clone());
                                            } else {
                                                view.answers[ix]
                                                    .selected
                                                    .retain(|id| id != &option_id);
                                            }
                                            view.error = None;
                                            if single {
                                                view.advance(window, cx);
                                            } else {
                                                cx.notify();
                                            }
                                        }))
                                },
                            )),
                    ),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .px_2()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(orbit_client::directories::display_diagnostic(error.as_ref()).into_owned()),
                )
            })
            .child(
                h_flex()
                    .items_end()
                    .gap_2()
                    .p_2()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(if focused {
                        cx.theme().ring
                    } else {
                        transparent_black()
                    })
                    .when(custom, |row| {
                        row.child(
                            div().flex_1().min_w_0().child(
                                Textarea::new(&self.inputs[ix])
                                    .appearance(false)
                                    .disabled(self.pending)
                                    .aria_label("自行撰写回复"),
                            ),
                        )
                    })
                    .when(!custom, |row| row.child(div().flex_1()))
                    .when(ix > 0, |row| {
                        row.child(
                            Button::new("previous-question")
                                .ghost()
                                .small()
                                .label("上一题")
                                .disabled(self.pending)
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.current -= 1;
                                    view.error = None;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        Button::new("skip-question")
                            .ghost()
                            .small()
                            .label(if self.answers.len() > 1 {
                                "跳过全部"
                            } else {
                                "跳过"
                            })
                            .disabled(self.pending)
                            .on_click(cx.listener(|view, _, _, cx| view.submit(true, cx))),
                    )
                    .child(
                        Button::new("send-answer")
                            .primary()
                            .small()
                            .label(if self.pending {
                                "发送中…"
                            } else if ix + 1 < self.answers.len() {
                                "下一题"
                            } else {
                                "发送"
                            })
                            .disabled(self.pending || !ready)
                            .on_click(cx.listener(|view, _, window, cx| view.advance(window, cx))),
                    ),
            )
    }
}

impl Workspace {
    pub(super) fn sync_questions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(info) = &self.info else {
            self.question_form = None;
            return;
        };
        let Some(record) = self
            .user_inputs
            .iter()
            .find(|record| record.status == Status::Pending)
        else {
            self.question_form = None;
            return;
        };
        if self
            .question_form
            .as_ref()
            .is_some_and(|form| {
                let form = form.read(cx);
                form.record.id == record.id && form.session == info.id
            })
        {
            return;
        }
        let owner = cx.weak_entity();
        let session = info.id.clone();
        let record = record.clone();
        self.question_form =
            Some(cx.new(|cx| QuestionForm::new(owner, session, record, window, cx)));
    }

    pub(super) fn questions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_questions(window, cx);
        if let Some(form) = &self.question_form {
            form.update(cx, |form, cx| {
                form.hidden = false;
                cx.notify();
            });
        }
        cx.notify();
    }

    pub(super) fn show_plugins(
        &self,
        catalog: orbit_protocol::plugins::Catalog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = &self.info else { return };
        let session = info.id.clone();
        let workspace = info.workspace.clone();
        let owner = cx.weak_entity();
        crate::dialog::open(window, cx, move |dialog, _, cx| {
            let owner2 = owner.clone();
            let session2 = session.clone();
            let workspace2 = workspace.clone();
            dialog
                .title("项目插件")
                .child(
                    v_flex().gap_3().children(
                        catalog
                            .marketplaces
                            .iter()
                            .flat_map(|market| {
                                let source = market.source.clone();
                                market
                                    .plugins
                                    .iter()
                                    .map(|plugin| {
                                        let name = plugin.name.clone();
                                        let source = source.clone();
                                        let owner = owner.clone();
                                        let session = session.clone();
                                        let workspace = workspace.clone();
                                        let enabled = catalog.applied.iter().any(|entry| {
                                            entry.plugin_name == name
                                                && entry.marketplace_source == source
                                        });
                                        let inspected = catalog
                                            .inspections
                                            .iter()
                                            .find(|entry| {
                                                entry.source == source && entry.name == name
                                            })
                                            .and_then(|entry| entry.package.as_ref().ok());
                                        let package = orbit_protocol::plugins::Selection {
                                            marketplace_source: source.clone(),
                                            plugin_name: name.clone(),
                                            package_source: plugin
                                                .source
                                                .clone()
                                                .unwrap_or_default(),
                                            version: inspected
                                                .and_then(|package| package.version.clone()),
                                            marketplace_revision: market.revision.clone(),
                                        };
                                        v_flex()
                                            .gap_1()
                                            .child(
                                                Checkbox::new(SharedString::from(format!(
                                                    "{source}:{name}"
                                                )))
                                                .label(name.clone())
                                                .checked(enabled)
                                                .disabled(
                                                    !enabled
                                                        && (inspected.is_none()
                                                            || plugin.source.is_none()),
                                                )
                                                .on_change(move |checked, window, cx| {
                                                    let action = if *checked {
                                                        orbit_protocol::plugins::Action::Enable {
                                                            plugin: package.clone(),
                                                        }
                                                    } else {
                                                        orbit_protocol::plugins::Action::Disable {
                                                            marketplace_source: source.clone(),
                                                            plugin_name: name.clone(),
                                                        }
                                                    };
                                                    let _ = owner.update(cx, |view, cx| {
                                                        view.command(
                                                            Command::Request(Request::Plugins {
                                                                session: session.clone(),
                                                                expected_workspace: Some(
                                                                    workspace.clone(),
                                                                ),
                                                                action,
                                                            }),
                                                            cx,
                                                        )
                                                    });
                                                    window.close_dialog(cx);
                                                }),
                                            )
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(plugin.description.clone()),
                                            )
                                    })
                                    .collect::<Vec<_>>()
                            }),
                    ),
                )
                .footer(
                    Button::new("add-marketplace")
                        .label("添加来源…")
                        .on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            let session = session2.clone();
                            let workspace = workspace2.clone();
                            let _ = owner2.update(cx, |view, cx| {
                                view.prompt(
                                    ("添加插件来源", "Git 地址或本地目录"),
                                    "",
                                    "添加",
                                    move |source| {
                                        Ok(Request::Plugins {
                                            session: session.clone(),
                                            expected_workspace: Some(workspace.clone()),
                                            action: orbit_protocol::plugins::Action::Add { source },
                                        })
                                    },
                                    window,
                                    cx,
                                )
                            });
                        }),
                )
        });
    }
}
