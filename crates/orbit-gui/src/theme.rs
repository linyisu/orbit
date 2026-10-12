//! Product palette and shared geometry. Component and Shell consume the same
//! resolved tokens; feature views never introduce literal palette colors.
use gpui_kit::component::{Theme, ThemeRegistry, ThemeSet};
use gpui_kit::{App, SharedString};
use std::rc::Rc;

pub(crate) const CONTENT_WIDTH_REM: f32 = 48.;

/// Technical tables keep identifiers and short CJK values intact. Kit measures
/// the actual glyph runs and owns horizontal scrolling when the table is wider
/// than its viewport; prose outside cells retains its normal wrapping.
pub(crate) fn document_style() -> gpui_kit::component::text::TextViewStyle {
    use gpui_kit::{Overflow, StyleRefinement, Styled};
    let mut table = StyleRefinement::default();
    table.overflow.x = Some(Overflow::Scroll);
    gpui_kit::component::text::TextViewStyle::default()
        .table(table)
        .table_cell(StyleRefinement::default().whitespace_nowrap())
}

pub(crate) fn install(cx: &mut App) {
    let mut set: ThemeSet = serde_json::from_str(include_str!("../appearance/theme.json"))
        .expect("bundled Orbit theme is valid");
    let fonts = cx.text_system().all_font_names();
    let family: Option<SharedString> = [
        "Noto Sans CJK SC",
        "WenQuanYi Micro Hei",
        "Noto Sans",
        "Inter",
    ]
    .into_iter()
    .find(|name| fonts.iter().any(|font| font == name))
    .map(Into::into);
    for theme in &mut set.themes {
        theme.font_family = family.clone();
        let defaults = if theme.mode.is_dark() {
            ThemeRegistry::global(cx).default_dark_theme()
        } else {
            ThemeRegistry::global(cx).default_light_theme()
        };
        if let (Some(highlight), Some(defaults)) = (&mut theme.highlight, &defaults.highlight) {
            highlight.syntax = defaults.syntax.clone();
        }
    }
    Theme::update(cx, |theme| {
        for config in set.themes {
            if config.mode.is_dark() {
                theme.dark_theme = Rc::new(config);
            } else {
                theme.light_theme = Rc::new(config);
            }
        }
        theme.focus_ring = true;
    });
}
