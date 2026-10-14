//! A virtualized conversation of user turns, each with one execution disclosure.
mod images;
mod markdown;
pub(crate) use images::ImageRequest;
mod model;
mod navigation;
mod process;
mod process_height;
use gpui_kit::component::{
    ActiveTheme, ElementExt, Icon, IconName, Sizable,
    bubble::{Bubble, BubbleContent, BubbleVariant},
    button::{Button, ButtonVariants},
    h_flex,
    message::{Message, MessageAlignment, MessageContent, MessageFooter},
    message_scroller::{MessageScroller, MessageScrollerState},
    text::{TextView, TextViewState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use model::{Kind, Transcript, Turn};
pub(crate) use navigation::bind_keys;
use orbit_client::protocol::Update;
use process::ProcessDisclosure;
use process_height::ProcessHeight;
use std::{cell::Cell, rc::Rc, sync::Arc};

pub(crate) struct Conversation {
    _subscriptions: Vec<Subscription>,
    transcript: Transcript,
    rows: Vec<Entity<TurnView>>,
    scroll: Entity<MessageScrollerState>,
    navigation: Entity<navigation::Navigation>,
    navigation_controls: Entity<navigation::Controls>,
    viewport_height: Rc<Cell<Pixels>>,
    viewport_bounds: Rc<Cell<Bounds<Pixels>>>,
}
struct TurnView {
    index: usize,
    user: Option<Entity<MessageRow>>,
    parts: Vec<Entity<MessageRow>>,
    process: Vec<usize>,
    conclusion: Option<usize>,
    reply: SharedString,
    label: SharedString,
    disclosure: ProcessDisclosure,
    scroll: Entity<MessageScrollerState>,
    process_scroll: Entity<MessageScrollerState>,
    process_height: ProcessHeight,
    process_limit: Pixels,
    viewport_height: Rc<Cell<Pixels>>,
    viewport_bounds: Rc<Cell<Bounds<Pixels>>>,
    _subscriptions: Vec<Subscription>,
}
struct LayoutChanged {
    index: usize,
}
struct Disclosure {
    index: usize,
}
struct MessageRow {
    _reply_images: Option<Entity<images::ReplyImages>>,
    _subscriptions: Vec<Subscription>,
    index: usize,
    title: SharedString,
    text: Entity<TextViewState>,
    literal: Option<SharedString>,
    viewport: Option<Rc<Cell<Bounds<Pixels>>>>,
    source_len: usize,
    deferred_text: Option<String>,
    muted: bool,
    user: bool,
    images: Vec<MessageImage>,
    expanded: bool,
}
struct MessageImage {
    path: SharedString,
    data: Result<Option<Arc<Image>>, SharedString>,
    dimensions: Option<(u32, u32)>,
}

impl EventEmitter<Disclosure> for MessageRow {}
impl EventEmitter<LayoutChanged> for MessageRow {}
impl EventEmitter<ImageRequest> for MessageRow {}
impl EventEmitter<ImageRequest> for TurnView {}
impl EventEmitter<ImageRequest> for Conversation {}
impl Conversation {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let scroll = cx.new(|cx| MessageScrollerState::new(0, cx));
        let viewport_height = Rc::new(Cell::new(px(0.)));
        let navigation =
            cx.new(|cx| navigation::Navigation::new(scroll.clone(), viewport_height.clone(), cx));
        let navigation_controls = cx.new(|cx| navigation::Controls::new(navigation.clone(), cx));
        Self {
            _subscriptions: vec![],
            navigation,
            navigation_controls,
            transcript: Transcript::default(),
            rows: vec![],
            scroll,
            viewport_height,
            viewport_bounds: Rc::new(Cell::new(Bounds::default())),
        }
    }

    pub(crate) fn navigation_controls(&self) -> Entity<navigation::Controls> {
        self.navigation_controls.clone()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub(crate) fn prompts(&self) -> Vec<String> {
        self.transcript
            .turns
            .iter()
            .filter_map(|turn| turn.prompt.clone())
            .filter(|text| !text.trim().is_empty())
            .collect()
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
            let row = cx.new(|cx| {
                TurnView::new(
                    index * 2 + 1,
                    self.scroll.clone(),
                    self.viewport_height.clone(),
                    self.viewport_bounds.clone(),
                    cx,
                )
            });
            self._subscriptions
                .push(cx.subscribe(&row, |_, _, request: &ImageRequest, cx| {
                    cx.emit(request.clone());
                }));
            self.rows.push(row);
            // Keep the prompt and its response in separate virtual rows. A
            // disclosure can then reveal its header without scrolling back
            // through a potentially very long user message.
            self.scroll
                .update(cx, |scroll, cx| scroll.append(2, cx));
            if let Some(prompt) = &self.transcript.turns[index].prompt {
                self.navigation
                    .update(cx, |navigation, cx| {
                        navigation.append(index * 2, prompt, cx)
                    });
            }
        }
        let turn = &self.transcript.turns[change.turn];
        self.rows[change.turn].update(cx, |view, cx| view.sync(turn, change.part, cx));
        self.scroll.update(cx, |scroll, cx| {
            scroll.remeasure_items(change.turn * 2 + 1..change.turn * 2 + 2, cx)
        });
        cx.notify();
    }

    pub(crate) fn image(
        &mut self,
        path: &str,
        result: Result<Vec<u8>, String>,
        cx: &mut Context<Self>,
    ) {
        let dimensions = result.as_ref().ok().and_then(|bytes| {
            image::ImageReader::new(std::io::Cursor::new(bytes))
                .with_guessed_format()
                .ok()?
                .into_dimensions()
                .ok()
        });
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
                    if image.is_err() {
                        let previous_len = row.images.len();
                        row.images
                            .retain(|attachment| attachment.path.as_ref() != path);
                        if row.images.len() != previous_len {
                            cx.notify();
                        }
                    }
                    for attachment in &mut row.images {
                        if attachment.path.as_ref() == path {
                            attachment.data = image.clone();
                            attachment.dimensions = dimensions;
                            cx.notify();
                        }
                    }
                });
                self.scroll.update(cx, |scroll, cx| {
                    scroll.remeasure_items(index * 2..index * 2 + 1, cx)
                });
            }
        }
    }
}

impl TurnView {
    fn measure_process_row(
        &mut self,
        revision: u64,
        index: usize,
        size: Size<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let before = self
            .process_height
            .height(self.process_limit);
        self.process_height
            .record(revision, index, size);
        if before
            != self
                .process_height
                .height(self.process_limit)
        {
            self.scroll.update(cx, |scroll, cx| {
                scroll.remeasure_items(self.index..self.index + 1, cx);
            });
            cx.notify();
        }
    }

    fn new(
        index: usize,
        scroll: Entity<MessageScrollerState>,
        viewport_height: Rc<Cell<Pixels>>,
        viewport_bounds: Rc<Cell<Bounds<Pixels>>>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            index,
            user: None,
            parts: vec![],
            process: vec![],
            conclusion: None,
            reply: "".into(),
            label: "".into(),
            disclosure: ProcessDisclosure::default(),
            scroll,
            process_scroll: cx.new(|cx| MessageScrollerState::new(0, cx)),
            process_height: ProcessHeight::default(),
            process_limit: px(0.),
            viewport_height,
            viewport_bounds,
            _subscriptions: vec![],
        }
    }

    fn sync(&mut self, turn: &Turn, changed: Option<usize>, cx: &mut Context<Self>) {
        self.reply = turn.reply().into();
        if self.user.is_none()
            && let Some(prompt) = &turn.prompt
        {
            self.user = Some(cx.new(|cx| {
                let mut row = MessageRow::new(self.index - 1, prompt, false, true, cx);
                row.viewport = Some(self.viewport_bounds.clone());
                row.images = turn
                    .images
                    .iter()
                    .map(|path| MessageImage {
                        path: path.clone().into(),
                        data: Ok(None),
                        dimensions: None,
                    })
                    .collect();
                row
            }));
        }
        while self.parts.len() < turn.parts.len() {
            let index = self.parts.len();
            let part = &turn.parts[index];
            let row = cx.new(|cx| {
                MessageRow::new(index, &part.content, part.kind != Kind::Text, false, cx)
            });
            self._subscriptions
                .push(cx.subscribe(&row, |_, _, request: &ImageRequest, cx| {
                    cx.emit(request.clone());
                }));
            self._subscriptions
                .push(cx.subscribe(&row, |view, _, event: &LayoutChanged, cx| {
                    view.process_height.refresh();
                    if let Some(index) = view
                        .process
                        .iter()
                        .position(|part| *part == event.index)
                    {
                        view.process_scroll
                            .update(cx, |scroll, cx| {
                                scroll.remeasure_items(index..index + 1, cx);
                            });
                    }
                    view.scroll.update(cx, |scroll, cx| {
                        scroll.remeasure_items(view.index..view.index + 1, cx);
                    });
                    cx.notify();
                }));
            self._subscriptions
                .push(cx.subscribe(&row, |view, _, event: &Disclosure, cx| {
                    view.process_height.refresh();
                    if let Some(index) = view
                        .process
                        .iter()
                        .position(|part| *part == event.index)
                    {
                        view.process_scroll
                            .update(cx, |scroll, cx| {
                                scroll.remeasure_items(index..index + 1, cx);
                                // Explicitly anchor after invalidation: a list whose
                                // collapsed rows fit has resumed tail following.
                                scroll.scroll_to_item(index, cx);
                            });
                    }
                    view.scroll.update(cx, |scroll, cx| {
                        scroll.remeasure_items(view.index..view.index + 1, cx);
                        scroll.scroll_to_item(view.index, cx);
                    });
                    cx.notify();
                }));
            self.parts.push(row);
        }
        let conclusion = turn.conclusion();
        let process: Vec<_> = turn.process_indices().collect();
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
                    row.text
                        .update(cx, |text, cx| text.set_text(&content, cx));
                }
                row.source_len = part.content.len();
                cx.notify();
            });
            if let Some(index) = process
                .iter()
                .position(|part| *part == index)
            {
                self.process_scroll
                    .update(cx, |scroll, cx| {
                        scroll.remeasure_items(index..index + 1, cx)
                    });
            }
        }
        let preserved = self
            .process
            .iter()
            .zip(&process)
            .take_while(|(a, b)| a == b)
            .count();
        let count = turn.process_count();
        if count > self.process.len() {
            self.process_scroll
                .update(cx, |scroll, cx| {
                    scroll.append(count - self.process.len(), cx)
                });
        }
        if preserved < self.process.len() {
            self.process_scroll
                .update(cx, |scroll, cx| {
                    scroll.remeasure_items(preserved..count, cx)
                });
        }
        if self.process != process {
            self.process_height.invalidate(count);
        } else if changed.is_some_and(|part| process.contains(&part)) {
            self.process_height.refresh();
        }
        self.process = process;
        self.disclosure
            .sync(turn.completed && conclusion.is_some());
        self.conclusion = conclusion;
        self.label = turn.label().into();
        cx.notify();
    }
}

impl Render for TurnView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.weak_entity();
        let reading = self.process.iter().any(|index| {
            let part = self.parts[*index].read(cx);
            part.expanded && part.source_len > 0
        });
        let available = if self.viewport_height.get() > px(0.) {
            self.viewport_height.get()
        } else {
            window.viewport_size().height * 0.6
        };
        self.process_limit = if reading {
            (available * 0.7).min(window.rem_size() * 32.)
        } else {
            available * 0.5
        };
        let process_height = self
            .process_height
            .height(self.process_limit);
        let revision = self.process_height.revision();
        v_flex()
            .w_full()
            .min_w_0()
            .items_start()
            .gap_4()
            .when(!self.process.is_empty(), |view| {
                view.child(gpui_kit::base::ScrollContainment::new(
                    "process-heading-wheel-boundary",
                    Button::new("turn-process")
                        .ghost()
                        .small()
                        .icon(if self.disclosure.expanded() {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .label(self.label.clone())
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.disclosure.toggle();
                            if view.disclosure.expanded() {
                                view.process_scroll
                                    .update(cx, |scroll, cx| scroll.scroll_to_item(0, cx));
                            }
                            view.scroll.update(cx, |scroll, cx| {
                                scroll.remeasure_items(view.index..view.index + 1, cx);
                                scroll.scroll_to_item(view.index, cx);
                            });
                            cx.notify();
                        })),
                ))
            })
            .when(
                self.disclosure.expanded() && !self.process.is_empty(),
                |view| {
                    view.child(gpui_kit::base::ScrollContainment::new(
                        "process-wheel-boundary",
                        div()
                            .id("process-viewport")
                            .occlude()
                            .w_full()
                            .min_w_0()
                            .h(process_height)
                            .border_l_1()
                            .border_color(cx.theme().border)
                            .pl_3()
                            .child(
                                MessageScroller::new(
                                    "turn-activities",
                                    self.process_scroll.clone(),
                                    move |index, _, cx| {
                                        weak.upgrade()
                                            .and_then(|view| {
                                                let view = view.read(cx);
                                                view.process.get(index).map(|part| {
                                                    (
                                                        view.parts[*part].clone(),
                                                        index + 1 == view.process.len(),
                                                    )
                                                })
                                            })
                                            .map(|(part, last)| {
                                                let measured = weak.clone();
                                                // Keep the final control clear of the clipping
                                                // edge. Row padding participates in list height
                                                // and wheel/scrollbar limits, unlike an inset
                                                // outside the scrolling content.
                                                div()
                                                    .w_full()
                                                    .min_w_0()
                                                    .child(
                                                        div()
                                                            .w_full()
                                                            .min_w_0()
                                                            .pb_1()
                                                            .when(last, |row| row.pb_3())
                                                            .child(part),
                                                    )
                                                    .on_prepaint(move |bounds, _, cx| {
                                                        let measured = measured.clone();
                                                        cx.defer(move |cx| {
                                                            let _ =
                                                                measured.update(cx, |view, cx| {
                                                                    view.measure_process_row(
                                                                        revision,
                                                                        index,
                                                                        bounds.size,
                                                                        cx,
                                                                    );
                                                                });
                                                        });
                                                    })
                                                    .into_any_element()
                                            })
                                            .unwrap_or_else(|| div().into_any_element())
                                    },
                                )
                                .jump_button(false)
                                .with_row_style(StyleRefinement::default().px_0().pb_0())
                                .with_list_style(StyleRefinement::default().py_0())
                                .size_full(),
                            ),
                    ))
                },
            )
            .when_some(
                self.conclusion
                    .and_then(|i| self.parts.get(i).cloned()),
                |view, reply| view.child(reply),
            )
            .when(!self.reply.is_empty(), |view| {
                view.child(
                    h_flex().child(
                        gpui_kit::component::clipboard::Clipboard::new("copy-answer")
                            .value(self.reply.clone())
                            .small()
                            .tooltip("复制完整 AI 回答")
                            .accessibility_label("复制完整 AI 回答"),
                    ),
                )
            })
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
        let owner = cx.weak_entity();
        let viewport_height = self.viewport_height.clone();
        let viewport_bounds = self.viewport_bounds.clone();
        let scroller =
            MessageScroller::new("conversation", self.scroll.clone(), move |index, _, cx| {
                weak.upgrade()
                    .and_then(|view| {
                        view.read(cx)
                            .rows
                            .get(index / 2)
                            .cloned()
                    })
                    .map(|turn| {
                        let row = if index % 2 == 0 {
                            div()
                                .w_full()
                                .when_some(turn.read(cx).user.clone(), |view, user| {
                                    view.py_3().child(user)
                                })
                                .into_any_element()
                        } else {
                            turn.into_any_element()
                        };
                        let row_bounds = row_bounds.clone();
                        h_flex()
                            .relative()
                            .child(
                                canvas(
                                    move |bounds, _, _| {
                                        row_bounds
                                            .borrow_mut()
                                            .push((index, bounds));
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
                                viewport_bounds.set(bounds);
                                if viewport_height.replace(bounds.size.height) != bounds.size.height
                                {
                                    let owner = owner.clone();
                                    // This is the resolved chat viewport, excluding
                                    // the composer and toolbar. Resize disclosures
                                    // after paint, outside the list's measurement
                                    // borrow, and only when that viewport changes.
                                    cx.defer(move |cx| {
                                        let _ = owner.update(cx, |view, cx| {
                                            for row in &view.rows {
                                                row.update(cx, |_, cx| cx.notify());
                                            }
                                            view.scroll
                                                .update(cx, |scroll, cx| scroll.remeasure(cx));
                                            // Resizing can hide the current dot even
                                            // when the active round has not changed.
                                            view.navigation
                                                .update(cx, |navigation, cx| {
                                                    navigation.reveal_active(cx)
                                                });
                                            cx.notify();
                                        });
                                    });
                                }
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
    fn new(index: usize, source: &str, muted: bool, user: bool, cx: &mut Context<Self>) -> Self {
        let text = cx.new(|cx| {
            if user {
                // User input remains literal, including pasted code.
                TextViewState::markdown("", cx)
            } else {
                TextViewState::markdown(&markdown::prose(if muted { "" } else { source }), cx)
            }
        });
        let mut subscriptions = vec![];
        let reply_images = (!user).then(|| {
            let images = cx.new(images::ReplyImages::new);
            let resolver = images::ReplyImages::resolver(&images, cx);
            text.update(cx, |text, cx| text.set_image_source_override(resolver, cx));
            subscriptions.push(cx.subscribe(&images, |_, _, request: &ImageRequest, cx| {
                cx.emit(request.clone());
            }));
            subscriptions.push(cx.observe(&images, |row, _, cx| {
                row.text
                    .update(cx, |text, cx| text.invalidate_inline_layout(cx));
                cx.emit(LayoutChanged { index: row.index });
                cx.notify();
            }));
            images
        });
        Self {
            index,
            _reply_images: reply_images,
            _subscriptions: subscriptions,
            title: "".into(),
            text,
            literal: user.then(|| source.to_owned().into()),
            viewport: None,
            source_len: source.len(),
            deferred_text: muted.then(|| source.to_owned()),
            muted,
            user,
            images: vec![],
            expanded: !muted,
        }
    }
}

impl Render for MessageRow {
    /// The title icon owns a column; expanded content starts at the title text, using the same
    /// inset rather than Button's internal padding.
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let disclosure_inset = rems(1.5);
        let message_group: SharedString = format!("user-message-{}", cx.entity_id()).into();
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
                        .accessibility_label(self.title.clone())
                        .px_0()
                        .max_w_full()
                        .child(
                            h_flex()
                                .min_w_0()
                                .child(
                                    div()
                                        .w(disclosure_inset)
                                        .flex_none()
                                        .child(
                                            Icon::new(if self.expanded {
                                                IconName::ChevronDown
                                            } else {
                                                IconName::ChevronRight
                                            })
                                            .small(),
                                        ),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .child(self.title.clone()),
                                ),
                        )
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.expanded = !view.expanded;
                            if view.expanded
                                && let Some(source) = view.deferred_text.take()
                            {
                                view.text.update(cx, |text, cx| {
                                    text.set_text(&markdown::prose(&source), cx)
                                });
                            }
                            cx.emit(Disclosure { index: view.index });
                            cx.notify();
                        })),
                )
            })
            .when(!self.images.is_empty(), |view| {
                view.child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .children(
                            self.images
                                .iter()
                                .enumerate()
                                .map(|(index, attachment)| {
                                    let data = attachment.data.clone();
                                    let dimensions = attachment
                                        .dimensions
                                        .unwrap_or((800, 600));
                                    Button::new(("message-image", index))
                                        .ghost()
                                        .w_32()
                                        .h_24()
                                        .p_0()
                                        .child(match &data {
                                            Ok(Some(image)) => img(image.clone())
                                                .size_full()
                                                .object_fit(ObjectFit::Contain)
                                                .into_any_element(),
                                            Ok(None) => div()
                                                .child("加载图片…")
                                                .into_any_element(),
                                            Err(_) => div()
                                                .child("图片加载失败")
                                                .into_any_element(),
                                        })
                                        .on_click(move |_, window, cx| {
                                            crate::image_preview::open(
                                                data.clone(),
                                                Some(dimensions),
                                                window,
                                                cx,
                                            );
                                        })
                                }),
                        ),
                )
            })
            .when(self.expanded, |view| {
                if let Some(source) = &self.literal {
                    return view.child(
                        div()
                            .min_w_0()
                            .whitespace_normal()
                            .child(
                                gpui_kit::base::SelectableText::new("user-source", source.clone())
                                    .map(|text| {
                                        if let Some(viewport) = &self.viewport {
                                            text.scroll_viewport(viewport.get())
                                        } else {
                                            text
                                        }
                                    }),
                            ),
                    );
                }
                let text = TextView::new(&self.text)
                    .when(!self.user, |view| view.w_full())
                    .min_w_0()
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
                                cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
                            })
                    })
                    .plugin(crate::math::MathPlugin::inline())
                    .plugin(crate::math::MathPlugin::display())
                    .selectable(true)
                    .when(self.muted, |view| {
                        view.text_color(cx.theme().muted_foreground)
                    });
                if self.muted {
                    view.child(
                        div()
                            .w_full()
                            .min_w_0()
                            .pl(disclosure_inset)
                            .child(text),
                    )
                } else {
                    view.child(text)
                }
            });
        if self.user {
            div()
                .group(message_group.clone())
                .child(
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
                        .when_some(
                            self.literal
                                .clone()
                                .filter(|text| !text.is_empty()),
                            |message, source| {
                                message.footer(
                                    MessageFooter::new()
                                        .content_inset(false)
                                        .child(
                                            div()
                                                .id("user-copy-action")
                                                .opacity(0.)
                                                .group_hover(message_group.clone(), |style| {
                                                    style.opacity(1.)
                                                })
                                                .in_focus(|style| style.opacity(1.))
                                                .child(
                                                    gpui_kit::component::clipboard::Clipboard::new(
                                                        "copy-user-question",
                                                    )
                                                    .value(source)
                                                    .small()
                                                    .tooltip("复制问题")
                                                    .accessibility_label("复制用户问题"),
                                                ),
                                        ),
                                )
                            },
                        ),
                )
                .into_any_element()
        } else {
            Message::new()
                .alignment(MessageAlignment::Start)
                .content(
                    MessageContent::new()
                        .w_full()
                        .child(body),
                )
                .into_any_element()
        }
    }
}
