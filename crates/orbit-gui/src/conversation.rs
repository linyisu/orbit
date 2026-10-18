//! Chronological conversation segments with independently owned disclosures.
mod images;
mod markdown;
mod model;
mod navigation;
mod process;
use crate::file_preview::FileRequest;
use gpui_kit::component::{
    ActiveTheme, Icon, IconName, Sizable,
    bubble::{Bubble, BubbleContent, BubbleVariant},
    button::{Button, ButtonVariants},
    clipboard::Clipboard,
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
use orbit_client::conversation::layout::Segment;
use orbit_client::protocol::Update;
use process::ProcessDisclosure;
use std::{cell::Cell, collections::BTreeMap, rc::Rc, sync::Arc};

pub(crate) struct Conversation {
    _subscriptions: Vec<Subscription>,
    transcript: Transcript,
    activity: orbit_client::activity::Activity,
    rows: Vec<Entity<TurnView>>,
    order: Vec<RowKey>,
    scroll: Entity<MessageScrollerState>,
    scrolled_up: bool,
    navigation: Entity<navigation::Navigation>,
    navigation_controls: Entity<navigation::Controls>,
    tail_control: Entity<navigation::TailControl>,
    viewport_height: Rc<Cell<Pixels>>,
    viewport_bounds: Rc<Cell<Bounds<Pixels>>>,
}
#[derive(Clone)]
pub(crate) struct ToolRequest(pub orbit_client::execution::ToolRun);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKey {
    Prompt(usize),
    Part(usize, usize),
    Tools(usize, usize),
    Footer(usize),
}

struct TurnView {
    user: Option<Entity<MessageRow>>,
    parts: Vec<Entity<MessageRow>>,
    groups: BTreeMap<usize, Entity<ToolGroup>>,
    reply: SharedString,
    completed: bool,
    viewport_bounds: Rc<Cell<Bounds<Pixels>>>,
    _subscriptions: Vec<Subscription>,
}

struct ToolGroup {
    start: usize,
    rows: Vec<Entity<MessageRow>>,
    label: SharedString,
    disclosure: ProcessDisclosure,
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
    copy_ready: bool,
    images: Vec<MessageImage>,
    files: Vec<orbit_protocol::MessageImage>,
    expanded: bool,
    manual: bool,
    tool: Option<orbit_client::execution::ToolRun>,
}
struct MessageImage {
    path: SharedString,
    data: Result<Option<Arc<Image>>, SharedString>,
    dimensions: Option<(u32, u32)>,
}

impl EventEmitter<Disclosure> for MessageRow {}
impl EventEmitter<Disclosure> for ToolGroup {}
impl EventEmitter<Disclosure> for TurnView {}
impl EventEmitter<LayoutChanged> for TurnView {}
impl EventEmitter<LayoutChanged> for MessageRow {}
impl EventEmitter<FileRequest> for MessageRow {}
impl EventEmitter<FileRequest> for TurnView {}
impl EventEmitter<FileRequest> for Conversation {}
impl EventEmitter<ToolRequest> for MessageRow {}
impl EventEmitter<ToolRequest> for TurnView {}
impl EventEmitter<ToolRequest> for Conversation {}
impl Conversation {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let scroll = cx.new(|cx| MessageScrollerState::new(0, cx));
        let viewport_height = Rc::new(Cell::new(px(0.)));
        let navigation =
            cx.new(|cx| navigation::Navigation::new(scroll.clone(), viewport_height.clone(), cx));
        let navigation_controls = cx.new(|cx| navigation::Controls::new(navigation.clone(), cx));
        let tail_control = cx.new(|cx| navigation::TailControl::new(navigation.clone(), cx));

        Self {
            _subscriptions: vec![],
            navigation,
            navigation_controls,
            tail_control,
            transcript: Transcript::default(),
            activity: Default::default(),
            rows: vec![],
            order: vec![],
            scroll,
            scrolled_up: false,
            viewport_height,
            viewport_bounds: Rc::new(Cell::new(Bounds::default())),
        }
    }

    pub(crate) fn navigation_controls(&self) -> Entity<navigation::Controls> {
        self.navigation_controls.clone()
    }

    pub(crate) fn tail_control(&self) -> Entity<navigation::TailControl> {
        self.tail_control.clone()
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
        let running = match &update {
            Update::Started { .. } => Some(true),
            Update::Finished { .. } => Some(false),
            _ => None,
        };
        if let Some(running) = running {
            self.tail_control
                .update(cx, |control, cx| {
                    control.set_running(running, cx);
                });
        }
        if let Update::Agent(event) = &update {
            self.activity.apply(event);
        }
        let Some(change) = self.transcript.apply(update) else {
            cx.notify();
            return;
        };
        while self.rows.len() < self.transcript.turns.len() {
            let index = self.rows.len();
            let row = cx.new(|_| TurnView {
                user: None,
                parts: Vec::new(),
                groups: BTreeMap::new(),
                reply: "".into(),
                completed: false,
                viewport_bounds: self.viewport_bounds.clone(),
                _subscriptions: Vec::new(),
            });
            self._subscriptions
                .push(cx.subscribe(&row, |_, _, request: &FileRequest, cx| {
                    cx.emit(request.clone());
                }));
            self._subscriptions
                .push(cx.subscribe(&row, |_, _, request: &ToolRequest, cx| {
                    cx.emit(request.clone());
                }));
            self._subscriptions
                .push(cx.subscribe(&row, move |view, _, event: &Disclosure, cx| {
                    view.scroll
                        .update(cx, |scroll, cx| scroll.pause_following(cx));
                    view.remeasure_part(index, event.index, cx);
                }));
            self._subscriptions.push(cx.subscribe(
                &row,
                move |view, _, event: &LayoutChanged, cx| {
                    view.remeasure_part(index, event.index, cx);
                },
            ));
            self.rows.push(row);
            if let Some(prompt) = &self.transcript.turns[index].prompt {
                self.navigation
                    .update(cx, |navigation, cx| {
                        navigation.append(self.order.len(), prompt, cx)
                    });
            }
        }
        let turn = &self.transcript.turns[change.turn];
        let structure_changed = self.rows[change.turn]
            .read(cx)
            .parts
            .len()
            != turn.parts.len()
            || self.order.last() != Some(&RowKey::Footer(self.rows.len() - 1));
        self.rows[change.turn].update(cx, |view, cx| view.sync(turn, change.part, cx));
        if structure_changed {
            self.sync_order(cx);
        }
        if let Some(part) = change.part {
            self.remeasure_part(change.turn, part, cx);
        } else {
            let start = self
                .order
                .iter()
                .position(|key| *key == RowKey::Prompt(change.turn))
                .unwrap();
            let end = self
                .order
                .iter()
                .position(|key| *key == RowKey::Footer(change.turn))
                .unwrap();
            self.scroll.update(cx, |scroll, cx| {
                scroll.remeasure_items(start..end + 1, cx);
            });
        }
        cx.notify();
    }

    pub(crate) fn activity_label(&self) -> String {
        let progress = self.activity.progress();
        if progress.is_empty() {
            self.activity.label().into()
        } else {
            format!("{} · {progress}", self.activity.label())
        }
    }

    fn remeasure_part(&mut self, turn: usize, part: usize, cx: &mut Context<Self>) {
        let key = self.transcript.turns[turn]
            .segments()
            .into_iter()
            .find_map(|segment| match segment {
                Segment::Part(index) if index == part => Some(RowKey::Part(turn, part)),
                Segment::Tools(range) if range.contains(&part) => {
                    Some(RowKey::Tools(turn, range.start))
                }
                _ => None,
            });
        if let Some(index) = key.and_then(|key| {
            self.order
                .iter()
                .position(|row| *row == key)
        }) {
            self.scroll.update(cx, |scroll, cx| {
                scroll.remeasure_items(index..index + 1, cx);
            });
        }
    }

    fn sync_order(&mut self, cx: &mut Context<Self>) {
        let mut order = Vec::new();
        let mut prompts = Vec::new();
        for (turn, source) in self.transcript.turns.iter().enumerate() {
            if source.prompt.is_some() {
                prompts.push(order.len());
            }
            order.push(RowKey::Prompt(turn));
            order.extend(
                source
                    .segments()
                    .into_iter()
                    .map(|segment| match segment {
                        Segment::Part(part) => RowKey::Part(turn, part),
                        Segment::Tools(range) => RowKey::Tools(turn, range.start),
                    }),
            );
            order.push(RowKey::Footer(turn));
        }
        let prefix = self
            .order
            .iter()
            .zip(&order)
            .take_while(|(a, b)| a == b)
            .count();
        let suffix = self.order[prefix..]
            .iter()
            .rev()
            .zip(order[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        if prefix < self.order.len() || prefix < order.len() {
            self.scroll.update(cx, |scroll, cx| {
                scroll.splice(
                    prefix..self.order.len() - suffix,
                    order.len() - prefix - suffix,
                    cx,
                );
            });
        }
        self.order = order;
        self.navigation
            .update(cx, |navigation, cx| navigation.update_rows(&prompts, cx));
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
                    if let Some(position) = self
                        .order
                        .iter()
                        .position(|key| *key == RowKey::Prompt(index))
                    {
                        scroll.remeasure_items(position..position + 1, cx);
                    }
                });
            }
        }
    }
}

impl TurnView {
    fn sync(&mut self, turn: &Turn, changed: Option<usize>, cx: &mut Context<Self>) {
        self.reply = turn.reply().into();
        self.completed = turn.completed;
        if self.user.is_none()
            && let Some(prompt) = &turn.prompt
        {
            let row = cx.new(|cx| {
                let mut row = MessageRow::new(0, prompt, false, true, cx);
                row.viewport = Some(self.viewport_bounds.clone());
                row.files = turn.files.clone();
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
            });
            self._subscriptions
                .push(cx.subscribe(&row, |_, _, request: &FileRequest, cx| {
                    cx.emit(request.clone());
                }));
            self.user = Some(row);
        }
        while self.parts.len() < turn.parts.len() {
            let index = self.parts.len();
            let part = &turn.parts[index];
            let row = cx.new(|cx| {
                MessageRow::new(index, &part.content, part.kind != Kind::Text, false, cx)
            });
            self._subscriptions
                .push(cx.subscribe(&row, |_, _, request: &ToolRequest, cx| {
                    cx.emit(request.clone());
                }));
            self._subscriptions
                .push(cx.subscribe(&row, |_, _, request: &FileRequest, cx| {
                    cx.emit(request.clone());
                }));
            self._subscriptions
                .push(cx.subscribe(&row, |_, _, event: &LayoutChanged, cx| {
                    cx.emit(LayoutChanged { index: event.index });
                }));
            self._subscriptions
                .push(cx.subscribe(&row, |_, _, event: &Disclosure, cx| {
                    cx.emit(Disclosure { index: event.index });
                }));
            self.parts.push(row);
        }
        let indices: Vec<_> = changed
            .map(|i| vec![i])
            .unwrap_or_else(|| (0..self.parts.len()).collect());
        for index in indices {
            let part = &turn.parts[index];
            self.parts[index].update(cx, |row, cx| {
                row.title = part.title.clone().into();
                row.tool = part.tool.clone();
                row.copy_ready = turn.completed;
                if part.kind == Kind::Notice {
                    row.expanded = true;
                }
                if part.kind == Kind::Thought && !row.manual {
                    row.expanded = part.running;
                    if row.expanded
                        && let Some(source) = row.deferred_text.take()
                    {
                        row.text
                            .update(cx, |text, cx| text.set_text(&markdown::prose(&source), cx));
                    }
                }
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
        }
        for segment in turn.segments() {
            let Segment::Tools(range) = segment else {
                continue;
            };
            let start = range.start;
            let group = self
                .groups
                .entry(start)
                .or_insert_with(|| {
                    let group = cx.new(|_| ToolGroup {
                        start,
                        rows: Vec::new(),
                        label: "".into(),
                        disclosure: ProcessDisclosure::default(),
                    });
                    self._subscriptions.push(cx.subscribe(
                        &group,
                        |_, _, event: &Disclosure, cx| {
                            cx.emit(Disclosure { index: event.index });
                        },
                    ));
                    group
                });
            group.update(cx, |group, cx| {
                group.rows = self.parts[range.clone()].to_vec();
                group.label = orbit_client::execution::batch_label(
                    turn.parts[range.clone()]
                        .iter()
                        .filter_map(|part| {
                            part.tool
                                .as_ref()
                                .map(|tool| tool.status)
                        }),
                )
                .into();
                group.disclosure.sync(
                    !turn.parts[range]
                        .iter()
                        .any(|part| part.running),
                );
                cx.notify();
            });
        }
        cx.notify();
    }
}

/// Sibling disclosures share the arrow column and text baseline.
fn disclosure_button(id: &'static str, title: SharedString, expanded: bool) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .px_0()
        .max_w_full()
        .accessibility_label(title.clone())
        .child(
            h_flex()
                .min_w_0()
                .child(
                    div().w(rems(1.5)).flex_none().child(
                        Icon::new(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .small(),
                    ),
                )
                .child(div().min_w_0().truncate().child(title)),
        )
}

impl Render for ToolGroup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .min_w_0()
            .items_start()
            .gap_2()
            .child(
                disclosure_button("tool-group", self.label.clone(), self.disclosure.expanded())
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.disclosure.toggle();
                        cx.emit(Disclosure { index: view.start });
                        cx.notify();
                    })),
            )
            .when(self.disclosure.expanded(), |view| {
                view.child(
                    v_flex()
                        .w_full()
                        .min_w_0()
                        .pl(rems(1.5))
                        .children(self.rows.clone()),
                )
            })
    }
}

impl Render for TurnView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().when(self.completed && !self.reply.is_empty(), |view| {
            view.child(
                gpui_kit::component::clipboard::Clipboard::new("copy-answer")
                    .value(self.reply.clone())
                    .small()
                    .tooltip("复制完整 AI 回答")
                    .accessibility_label("复制完整 AI 回答"),
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
                        let view = view.read(cx);
                        let key = *view.order.get(index)?;
                        Some(match key {
                            RowKey::Prompt(turn) => div()
                                .w_full()
                                .when_some(view.rows[turn].read(cx).user.clone(), |row, user| {
                                    row.py_3().child(user)
                                })
                                .into_any_element(),
                            RowKey::Part(turn, part) => view.rows[turn].read(cx).parts[part]
                                .clone()
                                .into_any_element(),
                            RowKey::Tools(turn, start) => view.rows[turn].read(cx).groups[&start]
                                .clone()
                                .into_any_element(),
                            RowKey::Footer(turn) => view.rows[turn]
                                .clone()
                                .into_any_element(),
                        })
                    })
                    .map(|row| {
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
                    .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, window, cx| {
                        if event
                            .delta
                            .pixel_delta(window.line_height())
                            .y
                            < px(0.)
                        {
                            let scroll = view.scroll.clone();
                            // Short transcripts may never have been scrolled up. A downward
                            // gesture at their end also resumes following after a disclosure.
                            cx.defer(move |cx| {
                                scroll.update(cx, |scroll, cx| {
                                    if !scroll.is_following_tail() && !scroll.is_scrolled_up() {
                                        scroll.scroll_to_end(cx);
                                    }
                                });
                            });
                        }
                    }))
                    .child(scroller)
                    .child(
                        canvas(
                            move |_, _, _| {},
                            move |bounds, _, _, cx| {
                                viewport_bounds.set(bounds);
                                if viewport_height.replace(bounds.size.height) != bounds.size.height
                                {
                                    let owner = owner.clone();
                                    // Invalidate row measurements after paint, outside
                                    // the list's measurement borrow, only when the
                                    // viewport excluding the composer changes.
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
                                // Scrollbar drags refresh the window without notifying Kit's
                                // message-scroller entity. Observe the resolved position here.
                                if let Some(view) = owner.upgrade() {
                                    let view = view.read(cx);
                                    let state = view.scroll.read(cx);
                                    let scrolled_up = state.is_scrolled_up();
                                    if scrolled_up != view.scrolled_up {
                                        let owner = owner.clone();
                                        cx.defer(move |cx| {
                                            let _ = owner.update(cx, |view, cx| {
                                                let state = view.scroll.read(cx);
                                                let scrolled_up = state.is_scrolled_up();
                                                let resume = view.scrolled_up
                                                    && !scrolled_up
                                                    && !state.is_following_tail();
                                                view.scrolled_up = scrolled_up;
                                                if resume {
                                                    view.scroll.update(cx, |state, cx| {
                                                        state.scroll_to_end(cx);
                                                    });
                                                }
                                            });
                                        });
                                    }
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
            subscriptions.push(cx.subscribe(&images, |_, _, request: &FileRequest, cx| {
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
            copy_ready: user,
            images: vec![],
            files: vec![],
            expanded: !muted,
            manual: false,
            tool: None,
        }
    }
}

impl Render for MessageRow {
    /// The title icon owns a column; expanded content starts at the title text, using the same
    /// inset rather than Button's internal padding.
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(tool) = &self.tool {
            let tool = tool.clone();
            return Button::new("open-tool-details")
                .ghost()
                .small()
                .w_full()
                .justify_start()
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .child(
                            div()
                                .w_4()
                                .flex_shrink_0()
                                .when(
                                    matches!(
                                        tool.status,
                                        orbit_client::execution::ToolStatus::Running
                                            | orbit_client::execution::ToolStatus::Background
                                    ),
                                    |slot| {
                                        slot.child(
                                            Icon::new(IconName::LoaderCircle)
                                                .small()
                                                .with_animation(
                                                    "tool-running",
                                                    Animation::new(std::time::Duration::from_secs(
                                                        2,
                                                    ))
                                                    .repeat(),
                                                    |icon, delta| {
                                                        icon.transform(Transformation::rotate(
                                                            percentage(delta),
                                                        ))
                                                    },
                                                ),
                                        )
                                    },
                                )
                                .when(
                                    tool.status == orbit_client::execution::ToolStatus::Failed,
                                    |slot| {
                                        slot.child(
                                            Icon::new(IconName::CircleAlert)
                                                .small()
                                                .text_color(cx.theme().danger),
                                        )
                                    },
                                )
                                .when(
                                    tool.status == orbit_client::execution::ToolStatus::Stopped,
                                    |slot| slot.child(Icon::new(IconName::Square).small()),
                                ),
                        )
                        .child(
                            div()
                                .max_w(rems(16.))
                                .text_ellipsis()
                                .child(tool.name.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_ellipsis()
                                .text_color(cx.theme().muted_foreground)
                                .child(tool.summary()),
                        ),
                )
                .accessibility_label(format!("{} · {}", tool.status.label(), tool.summary()))
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(ToolRequest(tool.clone()))))
                .into_any_element();
        }
        let disclosure_inset = rems(1.5);
        let message_group: SharedString = format!("user-message-{}", cx.entity_id()).into();
        let body = v_flex()
            .when(!self.user, |view| view.w_full())
            .items_start()
            .min_w_0()
            .gap_2()
            .when(self.muted, |view| {
                view.child(
                    disclosure_button("disclose", self.title.clone(), self.expanded).on_click(
                        cx.listener(|view, _, _, cx| {
                            view.expanded = !view.expanded;
                            view.manual = true;
                            if view.expanded
                                && let Some(source) = view.deferred_text.take()
                            {
                                view.text.update(cx, |text, cx| {
                                    text.set_text(&markdown::prose(&source), cx)
                                });
                            }
                            cx.emit(Disclosure { index: view.index });
                            cx.notify();
                        }),
                    ),
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
            .when(!self.files.is_empty(), |view| {
                view.child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .children(self.files.iter().map(|file| {
                            let file = file.clone();
                            gpui_kit::component::attachment::Attachment::new()
                                .id(SharedString::from(file.path.clone()))
                                .small()
                                .tooltip(format!("查看 {}", file.name))
                                .media(
                                    gpui_kit::component::attachment::AttachmentMedia::new()
                                        .child(Icon::new(IconName::File)),
                                )
                                .content(
                                    gpui_kit::component::attachment::AttachmentContent::new()
                                        .title(
                                            gpui_kit::component::attachment::AttachmentTitle::new(
                                                file.name.clone(),
                                            ),
                                        ),
                                )
                                .on_click(cx.listener(move |_, _, window, cx| {
                                    let request =
                                        crate::file_preview::open(file.clone(), window, cx);
                                    cx.emit(request);
                                }))
                        })),
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
                    .style(crate::theme::document_style(cx))
                    .when(self.copy_ready, |view| {
                        view.code_block_actions(|block, _, _| {
                            let code = block.code();
                            Clipboard::new("copy-code")
                                .xsmall()
                                .value(code)
                                .tooltip("复制代码")
                                .accessibility_label("复制代码")
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
                            .border_l_1()
                            .border_dashed()
                            .border_color(cx.theme().muted_foreground)
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
