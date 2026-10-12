//! A virtualized conversation of user turns, each with one execution disclosure.
mod markdown;
mod model;
mod navigation;
use gpui_kit::component::{
    ActiveTheme, IconName, Sizable, WindowExt,
    bubble::{Bubble, BubbleContent, BubbleVariant},
    button::{Button, ButtonVariants},
    h_flex,
    message::{Message, MessageAlignment, MessageContent},
    message_scroller::{MessageScroller, MessageScrollerState},
    text::{TextView, TextViewState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use model::{Kind, Transcript, Turn};
pub(crate) use navigation::bind_keys;
use orbit_client::protocol::Update;
use std::sync::Arc;

pub(crate) struct Conversation {
    transcript: Transcript,
    rows: Vec<Entity<TurnView>>,
    scroll: Entity<MessageScrollerState>,
    navigation: Entity<navigation::Navigation>,
}
struct TurnView {
    index: usize,
    user: Option<Entity<MessageRow>>,
    parts: Vec<Entity<MessageRow>>,
    process_count: usize,
    conclusion: Option<usize>,
    label: SharedString,
    expanded: bool,
    scroll: Entity<MessageScrollerState>,
    process_scroll: Entity<MessageScrollerState>,
}
struct MessageRow {
    index: usize,
    title: SharedString,
    text: Entity<TextViewState>,
    source_len: usize,
    deferred_text: Option<String>,
    muted: bool,
    user: bool,
    images: Vec<MessageImage>,
    expanded: bool,
    scroll: Entity<MessageScrollerState>,
}
struct MessageImage {
    path: SharedString,
    data: Result<Option<Arc<Image>>, SharedString>,
}
impl Conversation {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let scroll = cx.new(|cx| MessageScrollerState::new(0, cx));
        let navigation = cx.new(|cx| navigation::Navigation::new(scroll.clone(), cx));
        Self {
            navigation,
            transcript: Transcript::default(),
            rows: vec![],
            scroll,
        }
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub(crate) fn first_prompt(&self) -> Option<&str> {
        self.transcript
            .turns
            .iter()
            .filter_map(|turn| turn.prompt.as_deref())
            .find(|prompt| !prompt.trim().is_empty())
    }
    pub(crate) fn apply(&mut self, update: Update, cx: &mut Context<Self>) {
        let Some(change) = self.transcript.apply(update) else {
            return;
        };
        while self.rows.len() < self.transcript.turns.len() {
            let index = self.rows.len();
            self.rows
                .push(cx.new(|cx| TurnView::new(index, self.scroll.clone(), cx)));
            self.scroll.update(cx, |scroll, cx| scroll.append(1, cx));
            if let Some(prompt) = &self.transcript.turns[index].prompt {
                self.navigation
                    .update(cx, |navigation, cx| navigation.append(index, prompt, cx));
            }
        }
        let turn = &self.transcript.turns[change.turn];
        self.rows[change.turn].update(cx, |view, cx| view.sync(turn, change.part, cx));
        self.scroll.update(cx, |scroll, cx| {
            scroll.remeasure_items(change.turn..change.turn + 1, cx)
        });
        cx.notify();
    }
    pub(crate) fn image(
        &mut self,
        path: &str,
        result: Result<Vec<u8>, String>,
        cx: &mut Context<Self>,
    ) {
        let image = result
            .map(|bytes| {
                let format = if bytes.starts_with(b"\x89PNG") {
                    ImageFormat::Png
                } else if bytes.starts_with(b"GIF8") {
                    ImageFormat::Gif
                } else if bytes.starts_with(b"RIFF") {
                    ImageFormat::Webp
                } else {
                    ImageFormat::Jpeg
                };
                Some(Arc::new(Image::from_bytes(format, bytes)))
            })
            .map_err(SharedString::from);
        for (index, turn) in self.rows.iter().enumerate() {
            if let Some(user) = &turn.read(cx).user.clone() {
                user.update(cx, |row, cx| {
                    for attachment in &mut row.images {
                        if attachment.path.as_ref() == path {
                            attachment.data = image.clone();
                            cx.notify();
                        }
                    }
                });
                self.scroll.update(cx, |scroll, cx| {
                    scroll.remeasure_items(index..index + 1, cx)
                });
            }
        }
    }
}
impl TurnView {
    fn new(index: usize, scroll: Entity<MessageScrollerState>, cx: &mut Context<Self>) -> Self {
        Self {
            index,
            user: None,
            parts: vec![],
            process_count: 0,
            conclusion: None,
            label: "".into(),
            expanded: false,
            scroll,
            process_scroll: cx.new(|cx| MessageScrollerState::new(0, cx)),
        }
    }
    fn sync(&mut self, turn: &Turn, changed: Option<usize>, cx: &mut Context<Self>) {
        if self.user.is_none()
            && let Some(prompt) = &turn.prompt
        {
            self.user = Some(cx.new(|cx| {
                let mut row =
                    MessageRow::new(self.index, self.scroll.clone(), prompt, false, true, cx);
                row.images = turn
                    .images
                    .iter()
                    .map(|path| MessageImage {
                        path: path.clone().into(),
                        data: Ok(None),
                    })
                    .collect();
                row
            }));
        }
        while self.parts.len() < turn.parts.len() {
            let index = self.parts.len();
            let part = &turn.parts[index];
            self.parts.push(cx.new(|cx| {
                MessageRow::new(
                    index,
                    self.process_scroll.clone(),
                    &part.content,
                    part.kind != Kind::Text,
                    false,
                    cx,
                )
            }));
        }
        let conclusion = turn.conclusion();
        // A prose segment becomes process context when the next tool starts.
        for index in [self.conclusion, conclusion].into_iter().flatten() {
            self.parts[index].update(cx, |row, _| {
                row.index = if Some(index) == conclusion {
                    self.index
                } else {
                    index
                };
                row.scroll = if Some(index) == conclusion {
                    self.scroll.clone()
                } else {
                    self.process_scroll.clone()
                };
            });
        }
        let indices: Vec<_> = changed
            .map(|i| vec![i])
            .unwrap_or_else(|| (0..self.parts.len()).collect());
        for index in indices {
            let part = &turn.parts[index];
            self.parts[index].update(cx, |row, cx| {
                row.title = part.title.clone().into();
                let delta = &part.content[row.source_len..];
                if let Some(source) = &mut row.deferred_text {
                    source.push_str(delta);
                } else if !delta.is_empty() {
                    let content = markdown::prose(&part.content);
                    row.text.update(cx, |text, cx| text.set_text(&content, cx));
                }
                row.source_len = part.content.len();
                cx.notify();
            });
            self.process_scroll.update(cx, |scroll, cx| {
                scroll.remeasure_items(index..index + 1, cx)
            });
        }
        let count = turn.process_count();
        if count > self.process_count {
            self.process_scroll.update(cx, |scroll, cx| {
                scroll.append(count - self.process_count, cx)
            });
        }
        self.process_count = count;
        self.conclusion = conclusion;
        self.label = turn.label().into();
        cx.notify();
    }
}
impl Render for TurnView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.weak_entity();
        v_flex()
            .w_full()
            .min_w_0()
            .items_start()
            .gap_4()
            .when_some(self.user.clone(), |view, user| {
                view.child(div().w_full().py_3().child(user))
            })
            .when(self.process_count > 0, |view| {
                view.child(
                    Button::new("turn-process")
                        .ghost()
                        .small()
                        .icon(if self.expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .label(self.label.clone())
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.expanded = !view.expanded;
                            if view.expanded {
                                view.process_scroll
                                    .update(cx, |scroll, cx| scroll.scroll_to_item(0, cx));
                            }
                            view.scroll.update(cx, |scroll, cx| {
                                scroll.scroll_to_item(view.index, cx);
                                scroll.remeasure_items(view.index..view.index + 1, cx)
                            });
                            cx.notify();
                        })),
                )
            })
            .when(self.expanded && self.process_count > 0, |view| {
                view.child(
                    div()
                        .w_full()
                        .h(rems((self.process_count as f32 * 1.75).clamp(4., 18.)))
                        .border_l_1()
                        .border_color(cx.theme().border)
                        .pl_3()
                        .child(
                            MessageScroller::new(
                                "turn-activities",
                                self.process_scroll.clone(),
                                move |index, _, cx| {
                                    weak.upgrade()
                                        .and_then(|view| view.read(cx).parts.get(index).cloned())
                                        .map(|part| part.into_any_element())
                                        .unwrap_or_else(|| div().into_any_element())
                                },
                            )
                            .jump_button(false)
                            .with_row_style(StyleRefinement::default().px_0().pb_1())
                            .with_list_style(StyleRefinement::default().py_0())
                            .size_full(),
                        ),
                )
            })
            .when_some(
                self.conclusion.and_then(|i| self.parts.get(i).cloned()),
                |view, reply| view.child(reply),
            )
    }
}
impl Render for Conversation {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.rows.is_empty() {
            return v_flex()
                .size_full()
                .justify_center()
                .items_center()
                .gap_2()
                .child(div().text_xl().child("开始一个任务"))
                .child(
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .child("描述要做的事，或从左侧继续已有会话"),
                )
                .into_any_element();
        }
        let weak = cx.weak_entity();
        // Kit owns virtual-list offsets. Measure only rendered rows, then update
        // the rail after paint without borrowing or replacing its scroll state.
        let measured = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let row_bounds = measured.clone();
        let navigation = self.navigation.downgrade();
        let scroller =
            MessageScroller::new("conversation", self.scroll.clone(), move |index, _, cx| {
                weak.upgrade()
                    .and_then(|view| view.read(cx).rows.get(index).cloned())
                    .map(|row| {
                        let row_bounds = row_bounds.clone();
                        h_flex()
                            .relative()
                            .child(
                                canvas(
                                    move |bounds, _, _| {
                                        row_bounds.borrow_mut().push((index, bounds));
                                    },
                                    |_, _, _, _| {},
                                )
                                .absolute()
                                .top_0()
                                .left_0()
                                .size_full(),
                            )
                            .w_full()
                            .justify_center()
                            .child(
                                div()
                                    .w_full()
                                    .when(
                                        crate::preferences::Preferences::comfortable_width(cx),
                                        |view| view.max_w(rems(crate::theme::CONTENT_WIDTH_REM)),
                                    )
                                    .child(row),
                            )
                            .into_any_element()
                    })
                    .unwrap_or_else(|| div().into_any_element())
            })
            .jump_button(false)
            .with_list_style(StyleRefinement::default().py_6())
            .with_row_style(
                StyleRefinement::default()
                    .pl(rems(crate::theme::CONTENT_GUTTER_REM))
                    .pr(rems(
                        crate::theme::CONTENT_GUTTER_REM - crate::theme::NAVIGATION_WIDTH_REM,
                    ))
                    .pb_3(),
            )
            .size_full();
        h_flex()
            .size_full()
            .items_stretch()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(scroller)
                    .child(
                        canvas(
                            move |_, _, _| {},
                            move |bounds, _, _, cx| {
                                let row = navigation::current_row(&measured.borrow(), bounds.top());
                                measured.borrow_mut().clear();
                                if let Some(row) = row {
                                    let navigation = navigation.clone();
                                    cx.defer(move |cx| {
                                        let _ = navigation.update(cx, |navigation, cx| {
                                            navigation.visible_row(row, cx)
                                        });
                                    });
                                }
                            },
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    ),
            )
            .child(self.navigation.clone())
            .into_any_element()
    }
}

impl MessageRow {
    fn new(
        index: usize,
        scroll: Entity<MessageScrollerState>,
        source: &str,
        muted: bool,
        user: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            index,
            title: "".into(),
            text: cx.new(|cx| {
                if user {
                    // Display user input literally. Escape Markdown punctuation
                    // and use hard breaks so deliberate newlines stay visible.
                    let mut text = String::with_capacity(source.len());
                    for ch in source.chars() {
                        if ch == '\n' {
                            text.push_str("  \n");
                        } else {
                            if ch.is_ascii_punctuation() {
                                text.push('\\');
                            }
                            text.push(ch);
                        }
                    }
                    TextViewState::markdown(&text, cx)
                } else {
                    TextViewState::markdown(&markdown::prose(if muted { "" } else { source }), cx)
                }
            }),
            source_len: source.len(),
            deferred_text: muted.then(|| source.to_owned()),
            muted,
            user,
            images: vec![],
            expanded: !muted,
            scroll,
        }
    }
}
impl Render for MessageRow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = v_flex()
            .when(!self.user, |view| view.w_full())
            .items_start()
            .min_w_0()
            .gap_2()
            .when(self.muted, |view| {
                view.child(
                    Button::new("disclose")
                        .ghost()
                        .small()
                        .icon(if self.expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .label(self.title.clone())
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.expanded = !view.expanded;
                            if view.expanded
                                && let Some(source) = view.deferred_text.take()
                            {
                                view.text.update(cx, |text, cx| {
                                    text.set_text(&markdown::prose(&source), cx)
                                });
                            }
                            view.scroll.update(cx, |scroll, cx| {
                                scroll.remeasure_items(view.index..view.index + 1, cx)
                            });
                            cx.notify();
                        })),
                )
            })
            .when(!self.images.is_empty(), |view| {
                view.child(h_flex().gap_2().flex_wrap().children(
                    self.images.iter().enumerate().map(|(index, attachment)| {
                        let data = attachment.data.clone();
                        let path = attachment.path.clone();
                        Button::new(("message-image", index))
                            .ghost()
                            .child(match &data {
                                Ok(Some(image)) => img(image.clone())
                                    .h_24()
                                    .max_w_64()
                                    .object_fit(ObjectFit::Contain)
                                    .into_any_element(),
                                Ok(None) => div().child("加载图片…").into_any_element(),
                                Err(_) => div().child("图片加载失败").into_any_element(),
                            })
                            .on_click(move |_, window, cx| {
                                let data = data.clone();
                                let path = path.clone();
                                window.open_dialog(cx, move |dialog, _, _| {
                                    dialog.title(path.clone()).child(match &data {
                                        Ok(Some(image)) => img(image.clone())
                                            .max_w_full()
                                            .max_h(rems(32.))
                                            .object_fit(ObjectFit::Contain)
                                            .into_any_element(),
                                        Err(error) => div().child(error.clone()).into_any_element(),
                                        _ => div().child("加载图片…").into_any_element(),
                                    })
                                });
                            })
                    }),
                ))
            })
            .when(self.expanded, |view| {
                view.child(
                    TextView::new(&self.text)
                        .when(!self.user, |view| view.w_full())
                        .style(crate::theme::document_style())
                        .code_block_actions(|block, _, _| {
                            let code = block.code();
                            Button::new("copy-code")
                                .ghost()
                                .xsmall()
                                .icon(IconName::Copy)
                                .label("复制")
                                .tooltip("复制代码")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        code.to_string(),
                                    ));
                                })
                        })
                        .plugin(crate::math::MathPlugin::inline())
                        .plugin(crate::math::MathPlugin::display())
                        .selectable(true)
                        .when(self.muted, |view| {
                            view.text_color(cx.theme().muted_foreground)
                        }),
                )
            });
        if self.user {
            Message::new()
                .alignment(MessageAlignment::End)
                .with_stack_style(StyleRefinement::default().max_w(relative(0.8)))
                .content(
                    MessageContent::new().bubble(
                        Bubble::new()
                            .with_variant(BubbleVariant::Muted)
                            .content(BubbleContent::new().child(body)),
                    ),
                )
                .into_any_element()
        } else {
            Message::new()
                .alignment(MessageAlignment::Start)
                .content(MessageContent::new().w_full().child(body))
                .into_any_element()
        }
    }
}
