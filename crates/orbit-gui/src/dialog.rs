//! Shared modal geometry follows the task search surface.
use gpui_kit::component::{
    WindowExt,
    dialog::{AlertDialog, Dialog},
};
use gpui_kit::*;

pub(crate) fn open(
    window: &mut Window,
    cx: &mut App,
    build: impl Fn(Dialog, &mut Window, &mut App) -> Dialog + 'static,
) {
    window.open_dialog(cx, move |dialog, window, cx| {
        let width = width(window);
        let top = window.viewport_size().height * 0.1;
        build(dialog, window, cx)
            .width(width)
            .margin_top(top)
            .p_4()
            .gap_4()
    });
}

pub(crate) fn alert(
    window: &mut Window,
    cx: &mut App,
    build: impl Fn(AlertDialog, &mut Window, &mut App) -> AlertDialog + 'static,
) {
    window.open_alert_dialog(cx, move |dialog, window, cx| {
        let width = width(window);
        let top = window.viewport_size().height * 0.1;
        build(dialog, window, cx)
            .width(width)
            .margin_top(top)
            .p_4()
            .gap_4()
    });
}

pub(crate) fn width(window: &Window) -> Pixels {
    (window.viewport_size().width - window.rem_size() * 4.)
        .max(px(0.))
        .min(window.rem_size() * 60.)
}

/// Reserve the title, footer and gaps at the active UI scale. Large fonts in a compact window must
/// not push the surface above its search offset.
pub(crate) fn body_height(window: &Window) -> Pixels {
    (window.viewport_size().height * 0.65)
        .min((window.viewport_size().height * 0.8 - window.rem_size() * 8.).max(px(0.)))
}
