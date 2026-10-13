//! Prevent capture-based nested scrollers from handing wheel input to ancestors.
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior,
    InspectorElementId, IntoElement, LayoutId, Pixels, ScrollWheelEvent, Window,
};

/// Contains unused wheel input after a child's capture-phase scroll handlers.
///
/// Pair with a scroller using [`crate::ScrollableMask`]. Its inner masks handle
/// movement first; this boundary consumes the remainder, including input at
/// either edge or when the content fits. Ancestor virtual lists never receive
/// that remainder. Layout, pointer clicks, keyboard input, and appearance stay
/// with the child. Bubble-only child scrollers need their own ScrollableMask.
pub struct ScrollContainment {
    id: ElementId,
    child: AnyElement,
}

impl ScrollContainment {
    pub fn new(id: impl Into<ElementId>, child: impl IntoElement) -> Self {
        Self {
            id: id.into(),
            child: child.into_any_element(),
        }
    }
}

impl IntoElement for ScrollContainment {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for ScrollContainment {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        self.child.prepaint(window, cx);
        // The child may occlude its background; register this normal hitbox
        // afterwards so containment remains active without occluding the child.
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
        let hitbox = hitbox.id;
        // Capture handlers run in registration order. Register after the child
        // so its masks can move first, but before any ancestor's bubble handler.
        window.on_mouse_event(move |_: &ScrollWheelEvent, phase, window, cx| {
            if phase.capture() && hitbox.should_handle_scroll(window) {
                cx.stop_propagation();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ScrollableMask;
    use gpui::{
        Axis, Context, InteractiveElement, IntoElement, ListAlignment, ListOffset, ListState,
        ParentElement, Render, ScrollDelta, ScrollHandle, ScrollWheelEvent,
        StatefulInteractiveElement, Styled, TestAppContext, VisualTestContext, Window, div, list,
        point, px,
    };

    struct NestedScroll {
        outer: ScrollHandle,
        inner: ListState,
    }

    impl Render for NestedScroll {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("outer")
                .w_full()
                .h(px(200.))
                .overflow_y_scroll()
                .track_scroll(&self.outer)
                .child(ScrollContainment::new(
                    "inner-boundary",
                    div()
                        .occlude()
                        .relative()
                        .w_full()
                        .h(px(60.))
                        .child(
                            list(self.inner.clone(), |_, _, _| {
                                div().w_full().h(px(50.)).into_any_element()
                            })
                            .size_full(),
                        )
                        .child(ScrollableMask::new(Axis::Vertical, &self.inner)),
                ))
                .child(div().w_full().h(px(400.)))
        }
    }

    fn setup<'a>(
        cx: &'a mut TestAppContext,
        outer: &ScrollHandle,
        inner: &ListState,
    ) -> &'a mut VisualTestContext {
        let (_, cx) = cx.add_window_view({
            let outer = outer.clone();
            let inner = inner.clone();
            move |_, _| NestedScroll { outer, inner }
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        cx
    }

    fn wheel(cx: &mut VisualTestContext, y: f32, delta: f32) {
        cx.simulate_event(ScrollWheelEvent {
            position: point(px(10.), px(y)),
            delta: ScrollDelta::Pixels(point(px(0.), px(delta))),
            ..Default::default()
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
    }

    #[gpui::test]
    fn inner_scrolls_but_both_edges_never_move_the_outer_view(cx: &mut TestAppContext) {
        let outer = ScrollHandle::new();
        let inner = ListState::new(8, ListAlignment::Top, px(400.));
        let cx = setup(cx, &outer, &inner);
        wheel(cx, 10., -40.);
        assert!(inner.scroll_px_offset_for_scrollbar().y < px(0.));
        assert_eq!(outer.offset().y, px(0.));

        inner.scroll_to_end();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        let bottom = inner.scroll_px_offset_for_scrollbar().y;
        wheel(cx, 10., -40.);
        assert_eq!(inner.scroll_px_offset_for_scrollbar().y, bottom);
        assert_eq!(outer.offset().y, px(0.));

        // Outside the boundary the outer conversation still scrolls normally.
        wheel(cx, 150., -40.);
        assert_eq!(outer.offset().y, px(-40.));
        inner.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.),
        });
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        wheel(cx, 10., 40.);
        assert_eq!(inner.scroll_px_offset_for_scrollbar().y, px(0.));
        assert_eq!(outer.offset().y, px(-40.));
    }

    #[gpui::test]
    fn empty_or_fitting_content_also_contains_wheel_input(cx: &mut TestAppContext) {
        for count in [0, 1] {
            let outer = ScrollHandle::new();
            let inner = ListState::new(count, ListAlignment::Top, px(400.));
            let cx = setup(cx, &outer, &inner);
            wheel(cx, 10., -40.);
            assert_eq!(outer.offset().y, px(0.));
            wheel(cx, 150., -40.);
            assert_eq!(outer.offset().y, px(-40.));
        }
    }
}
