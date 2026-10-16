//! Editable remote fields composed from Kit input, list, combobox and popup behavior.
use super::*;
use gpui_kit::component::{
    ElementExt, IndexPath, Selectable, ThemeStyled,
    list::{List, ListDelegate, ListItem, ListState},
};
use std::rc::Rc;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Choice {
    pub(super) value: String,
    pub(super) label: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ChoiceKind {
    Target,
    Directory,
}

pub(super) enum ChoiceEvent {
    Browse,
    Selected(String),
    Submit,
}

type Footer = Rc<dyn Fn(&mut Window, &mut App) -> AnyElement>;

pub(super) struct ChoiceInput {
    input: Entity<InputState>,
    label: &'static str,
    kind: ChoiceKind,
    items: Vec<Choice>,
    list: Entity<ListState<Choices>>,
    open: bool,
    bounds: Bounds<Pixels>,
    footer: Option<Footer>,
    deferred_context: Option<gpui_kit::base::DeferredPopover>,
    popup_focus: FocusHandle,
    trigger_focus: FocusHandle,
    _focus_events: Vec<Subscription>,
    _input_events: Subscription,
}

struct Choices {
    owner: WeakEntity<ChoiceInput>,
    items: Vec<Choice>,
    selected: Option<IndexPath>,
    value: String,
}

impl EventEmitter<ChoiceEvent> for ChoiceInput {}

impl ChoiceInput {
    pub(super) fn is_open(&self) -> bool {
        self.open
    }

    pub(super) fn new(
        input: Entity<InputState>,
        label: &'static str,
        kind: ChoiceKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let owner = cx.weak_entity();
        let list = cx.new(|list_cx| {
            ListState::new(
                Choices {
                    owner,
                    items: vec![],
                    selected: None,
                    value: String::new(),
                },
                window,
                list_cx,
            )
        });
        let input_events =
            cx.subscribe_in(&input, window, |view, _, event, window, cx| match event {
                InputEvent::Change => view.refresh(window, cx),
                InputEvent::Blur => {
                    cx.defer_in(window, |view, window, cx| {
                        if !view
                            .popup_focus
                            .contains_focused(window, cx)
                            && !view
                                .list
                                .focus_handle(cx)
                                .contains_focused(window, cx)
                        {
                            view.set_open(false, cx);
                        }
                    });
                }
                InputEvent::PressEnter { shift: false, .. } => {
                    if view.open {
                        let choice = view
                            .list
                            .read(cx)
                            .selected_index()
                            .and_then(|ix| {
                                view.list
                                    .read(cx)
                                    .delegate()
                                    .items
                                    .get(ix.row)
                                    .cloned()
                            });
                        if let Some(choice) = choice {
                            view.select(choice.value, window, cx);
                        } else if view.kind == ChoiceKind::Target {
                            let value = view.input.read(cx).value().to_string();
                            view.select(value, window, cx);
                        } else {
                            cx.emit(ChoiceEvent::Browse);
                        }
                    } else {
                        cx.emit(ChoiceEvent::Submit);
                    }
                }
                _ => {}
            });
        let popup_focus = cx.focus_handle().tab_stop(false);
        let trigger_focus = cx.focus_handle().tab_stop(false);
        let focus_events = [&trigger_focus, &popup_focus]
            .into_iter()
            .map(|focus| {
                cx.on_focus_out(focus, window, |_, _, window, cx| {
                    cx.defer_in(window, |view, window, cx| {
                        if !view
                            .trigger_focus
                            .contains_focused(window, cx)
                            && !view
                                .popup_focus
                                .contains_focused(window, cx)
                        {
                            view.set_open(false, cx);
                        }
                    });
                })
            })
            .collect();
        Self {
            input,
            label,
            kind,
            items: vec![],
            list,
            open: false,
            bounds: Bounds::default(),
            footer: None,
            deferred_context: None,
            _input_events: input_events,
            popup_focus,
            trigger_focus,
            _focus_events: focus_events,
        }
    }

    pub(super) fn set_items(
        &mut self,
        items: Vec<Choice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.items != items {
            self.items = items;
            self.refresh(window, cx);
        }
    }

    pub(super) fn set_footer(&mut self, footer: Footer) {
        self.footer = Some(footer);
    }

    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self.input.read(cx).value().to_string();
        let query = value.to_lowercase();
        let items = self
            .items
            .iter()
            .filter(|item| {
                self.kind == ChoiceKind::Directory
                    || item
                        .label
                        .to_lowercase()
                        .contains(&query)
            })
            .cloned()
            .collect();
        self.list.update(cx, |list, cx| {
            list.delegate_mut().items = items;
            list.delegate_mut().value = value;
            list.set_selected_index(None, window, cx);
            cx.notify();
        });
        cx.notify();
    }

    fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.open == open {
            return;
        }
        self.open = open;
        self.deferred_context =
            open.then(|| gpui_kit::base::GlobalState::register_deferred_popover(cx));
        if open {
            cx.emit(ChoiceEvent::Browse);
        }
        cx.notify();
    }

    /// Hand navigation to the list while preserving its existing highlight.
    fn focus_choices(&mut self, last: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.set_open(true, cx);
        self.list.update(cx, |list, cx| {
            let count = list.delegate().items.len();
            if list.selected_index().is_none() && count > 0 {
                list.set_selected_index(
                    Some(IndexPath::default().row(if last { count - 1 } else { 0 })),
                    window,
                    cx,
                );
            }
            list.focus(window, cx);
        });
    }

    fn select(&mut self, value: String, window: &mut Window, cx: &mut Context<Self>) {
        self.set_open(false, cx);
        self.input.update(cx, |input, cx| {
            input.set_value(value.clone(), window, cx);
            input.focus(window, cx);
        });
        cx.emit(ChoiceEvent::Selected(value));
    }
}

impl Render for ChoiceInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let list_focus = self.list.focus_handle(cx);
        let below = window.bounds().size.height - self.bounds.bottom();
        let above = self.bounds.top();
        let rows = self
            .list
            .read(cx)
            .delegate()
            .items
            .len()
            .max(1);
        let list_height = window.rem_size() * (rows as f32 * 2.).min(15.);
        let anchor = if below < list_height + window.rem_size() && above > below {
            Anchor::BottomLeft
        } else {
            Anchor::TopLeft
        };
        let mut popup = gpui_kit::base::Popup::new(
            ("remote-choice-popup", cx.entity_id()),
            div()
                .key_context("RemoteChoiceInput")
                .w_full()
                .on_prepaint({
                    let owner = cx.weak_entity();
                    move |bounds, _, cx| {
                        let _ = owner.update(cx, |view, cx| {
                            let resized = view.bounds.size.width != bounds.size.width;
                            view.bounds = bounds;
                            if resized && view.open {
                                cx.notify();
                            }
                        });
                    }
                })
                .capture_any_mouse_down(cx.listener(|view, event: &MouseDownEvent, _, cx| {
                    if event.button == MouseButton::Left {
                        view.set_open(true, cx);
                    }
                }))
                .child(
                    Input::new(&self.input)
                        .aria_label(self.label)
                        .selected(self.open)
                        .suffix(Icon::new(IconName::ChevronDown).size_4())
                        .w_full(),
                ),
        )
        .w_full()
        .anchor(anchor)
        .offset(window.rem_size() * 0.25);
        if self.open {
            popup = popup.content(
                v_flex()
                    .track_focus(&self.popup_focus)
                    .w(self.bounds.size.width)
                    .popover_style(cx)
                    .p_1()
                    .child(List::new(&self.list).max_h(rems(15.)))
                    .when_some(self.footer.as_ref(), |panel, footer| {
                        panel.child(footer(window, cx))
                    })
                    .on_mouse_down_out(cx.listener(|view, event: &MouseDownEvent, _, cx| {
                        if !view.bounds.contains(&event.position) {
                            view.set_open(false, cx);
                        }
                    })),
            );
        }
        let owner = cx.weak_entity();
        div()
            .key_context("RemoteChoices")
            .track_focus(&self.trigger_focus)
            .w_full()
            .on_action(cx.listener(|view, _: &DismissRemoteChoices, _, cx| {
                view.set_open(false, cx);
                cx.propagate();
            }))
            .on_action(cx.listener(
                |view, _: &gpui_kit::base::actions::SelectDown, window, cx| {
                    view.focus_choices(false, window, cx);
                },
            ))
            .on_action(
                cx.listener(|view, _: &gpui_kit::base::actions::SelectUp, window, cx| {
                    view.focus_choices(true, window, cx);
                }),
            )
            .child(
                gpui_kit::base::Combobox::new(("remote-choice", cx.entity_id()))
                    .open(self.open)
                    .content_focus_handle(&list_focus)
                    .on_open_change(move |open, window, cx| {
                        let _ = owner.update(cx, |view, cx| {
                            view.set_open(open, cx);
                            if !open {
                                cx.defer_in(window, |view, window, cx| {
                                    view.input
                                        .update(cx, |input, cx| input.focus(window, cx));
                                });
                            }
                        });
                    })
                    .w_full()
                    .child(popup),
            )
    }
}

impl ListDelegate for Choices {
    type Item = ListItem;

    fn items_count(&self, _: usize, _: &App) -> usize {
        self.items.len()
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> Option<ListItem> {
        let item = self.items.get(ix.row)?;
        Some(
            ListItem::new(SharedString::from(item.value.clone()))
                .h_8()
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_ellipsis()
                                .child(item.label.clone()),
                        )
                        .when(item.value == self.value, |row| {
                            row.child(Icon::new(IconName::Check).size_4())
                        }),
                ),
        )
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        div()
            .p_2()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child("没有匹配项，可直接输入")
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) {
        self.selected = ix;
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        if let Some(item) = self
            .selected
            .and_then(|ix| self.items.get(ix.row))
            .cloned()
        {
            let owner = self.owner.clone();
            cx.defer_in(window, move |_, window, cx| {
                let _ = owner.update(cx, |view, cx| view.select(item.value, window, cx));
            });
        }
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<ListState<Self>>) {
        let owner = self.owner.clone();
        cx.defer_in(window, move |_, window, cx| {
            let _ = owner.update(cx, |view, cx| {
                view.set_open(false, cx);
                view.input
                    .update(cx, |input, cx| input.focus(window, cx));
            });
        });
    }
}
