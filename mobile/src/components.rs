//! Shared mobile spacing, text and touch targets.
use base64::Engine;
use nsh_base::{Button, ButtonEvent, ButtonProps, text_view};
use nsh_core::ui::{Input, prelude::*};

#[derive(Clone, Copy)]
pub enum Icon {
    Menu,
    Folder,
    FolderOpen,
    Pin,
    Refresh,
    Settings,
    Computer,
    Add,
    Send,
    Close,
    Down,
}

pub fn icon(kind: Icon, size: f32) -> impl IntoElement {
    let path = match kind {
        Icon::Folder => "M3 6h7l2 2h9v12H3z M3 6V4h7l2 2",
        Icon::FolderOpen => "M3 9V5h6l2 2h9v3 M3 10h19l-4 10H2z",
        Icon::Pin => "M9 3h6l-1 7 4 4H6l4-4z M12 14v7",
        Icon::Refresh => "M20 7v5h-5 M4 17v-5h5 M5 8a8 8 0 0 1 14-3l1 2 M4 17l1 2a8 8 0 0 0 14-3",
        Icon::Menu => "M4 6h16M4 12h12M4 18h16",
        Icon::Settings => {
            "M9 3h6l1 3 3 1 2 5-2 5-3 1-1 3H9l-1-3-3-1-2-5 2-5 3-1z M15.5 12a3.5 3.5 0 1 1-7 0 3.5 3.5 0 0 1 7 0"
        }
        Icon::Computer => "M4 4h16a1 1 0 0 1 1 1v11H3V5a1 1 0 0 1 1-1z M8 20h8M12 16v4",
        Icon::Add => "M12 5v14M5 12h14",
        Icon::Send => "M12 19V5M5 12l7-7 7 7",
        Icon::Down => "M12 5v14M5 12l7 7 7-7",
        Icon::Close => "M6 6l12 12M6 18 18 6",
    };
    image(move || {
        let token = if matches!(kind, Icon::Send) { "primary.foreground" } else { "foreground" };
        let color = color(tokens().color(token).unwrap_or_default()).to_hex();
        let svg = format!("<svg xmlns='http://www.w3.org/2000/svg' width='24' height='24' viewBox='0 0 24 24'><path d='{path}' fill='none' stroke='{color}' stroke-width='1.7' stroke-linecap='round' stroke-linejoin='round'/></svg>");
        nsh_core::ui::ImageSrc::from(format!("data:image/svg+xml;base64,{}", base64::engine::general_purpose::STANDARD.encode(svg)))
    }).w(px(size)).h(px(size))
}

pub fn icon_button(
    label: &'static str,
    kind: Icon,
    action: impl Fn() + 'static,
) -> impl IntoElement {
    Button::element(ButtonProps {
        accessibility_label: Some(SharedString::from(label)).into(),
        ..Default::default()
    })
    .on_event(move |event: &ButtonEvent| {
        if matches!(event, ButtonEvent::Press) {
            action();
        }
    })
    .w(px(44.0))
    .h(px(44.0))
    .items_center()
    .justify_center()
    .rounded(px(22.0))
    .bg(if matches!(kind, Icon::Send) {
        tokens()
            .color("primary.background")
            .unwrap_or_default()
    } else if matches!(kind, Icon::Down) {
        tokens()
            .color("secondary.background")
            .unwrap_or_default()
    } else {
        nsh_core::ui::AdaptiveColor::TRANSPARENT
    })
    .child(icon(kind, 23.0))
}

pub fn composer_field(value: Signal<String>) -> Input {
    input()
        .label("描述任务…")
        .value(move || value.get())
        .on_change(move |text: &str| value.set(text.to_owned()))
        .placeholder("有什么想做的？")
        .auto_correct(false)
        .multiline(true)
        .w_full()
        .min_h(px(56.0))
        .max_h(px(160.0))
        .px_2()
        .py_1()
        .text_size(px(17.0))
        .text_color(
            tokens()
                .color("foreground")
                .unwrap_or_default(),
        )
        .placeholder_color(|| {
            color(
                tokens()
                    .color("muted.foreground")
                    .unwrap_or_default(),
            )
        })
}

pub fn button(label: &'static str, action: impl Fn() + 'static) -> impl IntoElement {
    Button::element(ButtonProps::default())
        .on_event(move |event: &ButtonEvent| {
            if matches!(event, ButtonEvent::Press) {
                action();
            }
        })
        .w_full()
        .flex_shrink(1.0)
        .min_h(px(48.0))
        .px_4()
        .py_3()
        .rounded(px(12.0))
        .border_1()
        .border_color(
            tokens()
                .color("border")
                .unwrap_or_default(),
        )
        .child(
            text(label)
                .text_color(
                    tokens()
                        .color("secondary.foreground")
                        .unwrap_or_default(),
                )
                .font_weight(FontWeight::MEDIUM),
        )
}

pub fn field(value: Signal<String>, placeholder: &'static str) -> Input {
    input()
        .label(placeholder)
        .value(move || value.get())
        .on_change(move |text: &str| value.set(text.to_owned()))
        .placeholder(placeholder)
        .auto_correct(false)
        .w_full()
        .h(px(44.0))
        .px_3()
        .py_1()
        .rounded(px(12.0))
        .border_1()
        .border_color(
            tokens()
                .color("border")
                .unwrap_or_default(),
        )
        .bg(tokens()
            .color("background")
            .unwrap_or_default())
        .text_color(
            tokens()
                .color("foreground")
                .unwrap_or_default(),
        )
        .text_size(px(16.0))
}

pub fn markdown(value: impl Fn() -> String + 'static) -> impl IntoElement {
    text_view()
        .markdown(move || Some(SharedString::from(value())))
        .selectable(true)
        .text_size(px(16.0))
        .line_height(px(24.0))
        .text_color(
            tokens()
                .color("foreground")
                .unwrap_or_default(),
        )
}

pub fn primary_button(label: &'static str, action: impl Fn() + 'static) -> impl IntoElement {
    Button::element(ButtonProps::default())
        .on_event(move |event: &ButtonEvent| {
            if matches!(event, ButtonEvent::Press) {
                action();
            }
        })
        .w_full()
        .min_h(px(52.0))
        .px_4()
        .py_3()
        .rounded(px(14.0))
        .bg(tokens()
            .color("primary.background")
            .unwrap_or_default())
        .child(
            text(label)
                .w_full()
                .text_center()
                .text_color(
                    tokens()
                        .color("primary.foreground")
                        .unwrap_or_default(),
                )
                .font_weight(FontWeight::MEDIUM),
        )
}

pub fn toolbar_button(label: &'static str, action: impl Fn() + 'static) -> impl IntoElement {
    Button::element(ButtonProps::default())
        .on_event(move |event: &ButtonEvent| {
            if matches!(event, ButtonEvent::Press) {
                action();
            }
        })
        .min_w(px(48.0))
        .h(px(48.0))
        .px_3()
        .items_center()
        .justify_center()
        .rounded(px(24.0))
        .bg(tokens()
            .color("secondary.background")
            .unwrap_or_default())
        .child(text(label).text_size(px(16.0)))
}

pub fn page_header(title: &'static str, close: impl Fn() + 'static) -> impl IntoElement {
    view()
        .w_full()
        .flex_row()
        .items_center()
        .gap_3()
        .p_4()
        .child(
            text(title)
                .flex_1()
                .text_size(px(20.0))
                .font_weight(FontWeight::MEDIUM),
        )
        .child(icon_button("关闭", Icon::Close, close))
}
