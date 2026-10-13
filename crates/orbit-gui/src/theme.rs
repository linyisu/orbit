//! Product palette and shared geometry. Component and Shell consume the same
//! resolved tokens; feature views never introduce literal palette colors.
mod syntax;
use gpui_kit::component::{Theme, ThemeSet};
use gpui_kit::{App, FontFallbacks, Global, SharedString};
use std::rc::Rc;

pub(crate) const CONTENT_WIDTH_REM: f32 = 48.;
pub(crate) const CONTENT_GUTTER_REM: f32 = 3.;
pub(crate) const NAVIGATION_WIDTH_REM: f32 = 2.;

/// Enumerated once outside rendering. The platform text system supplies the
/// family names, so the picker never offers a font it cannot load.
struct FontCatalog {
    families: Vec<SharedString>,
    ui: SharedString,
    code: SharedString,
    emoji: bool,
}
impl Global for FontCatalog {}

pub(crate) fn installed_fonts(cx: &App) -> &[SharedString] {
    &cx.global::<FontCatalog>().families
}

pub(crate) fn default_fonts(cx: &App) -> (SharedString, SharedString) {
    let fonts = cx.global::<FontCatalog>();
    (fonts.ui.clone(), fonts.code.clone())
}

pub(crate) fn has_emoji_font(cx: &App) -> bool {
    cx.global::<FontCatalog>().emoji
}

const EMOJI_FAMILIES: &[&str] = &[
    "Noto Color Emoji",
    "Apple Color Emoji",
    "Segoe UI Emoji",
    "Noto Emoji",
    "Twemoji Mozilla",
];

/// Explicit coverage supplements, rather than replaces, the platform's native
/// fallback. GPUI preserves the chosen primary font for glyphs it contains.
fn fallback_families(installed: &[String]) -> Vec<String> {
    EMOJI_FAMILIES
        .iter()
        .copied()
        .chain([
            "Noto Sans CJK SC",
            "Source Han Sans SC",
            "WenQuanYi Micro Hei",
            "Microsoft YaHei",
            "PingFang SC",
            "Noto Sans Symbols 2",
            "Noto Sans Symbols",
            "Segoe UI Symbol",
            "DejaVu Sans",
            "Noto Sans",
        ])
        .filter(|candidate| installed.iter().any(|family| family == candidate))
        .map(str::to_owned)
        .collect()
}

pub(crate) fn resolve_fonts(
    ui: Option<&str>,
    code: Option<&str>,
    cx: &App,
) -> (SharedString, SharedString) {
    let fonts = cx.global::<FontCatalog>();
    (
        resolve_family(ui, &fonts.ui, &fonts.families),
        resolve_family(code, &fonts.code, &fonts.families),
    )
}

fn resolve_family(
    preferred: Option<&str>,
    fallback: &SharedString,
    installed: &[SharedString],
) -> SharedString {
    preferred
        .and_then(|preferred| installed.iter().find(|family| family.as_ref() == preferred))
        .unwrap_or(fallback)
        .clone()
}

/// Technical tables keep identifiers and short CJK values intact. Kit measures
/// the actual glyph runs and owns horizontal scrolling when the table is wider
/// than its viewport; prose outside cells retains its normal wrapping.
pub(crate) fn document_style() -> gpui_kit::component::text::TextViewStyle {
    use gpui_kit::{Overflow, StyleRefinement, Styled, relative, rems};
    let mut table = StyleRefinement::default();
    table.overflow.x = Some(Overflow::Scroll);
    gpui_kit::component::text::TextViewStyle::default()
        .code_block(
            StyleRefinement::default()
                .text_size(rems(0.875))
                .line_height(relative(1.5)),
        )
        .table(table)
        .table_cell(StyleRefinement::default().whitespace_nowrap())
}

pub(crate) fn install(cx: &mut App) {
    let mut set: ThemeSet = serde_json::from_str(include_str!("../appearance/theme.json"))
        .expect("bundled Orbit theme is valid");
    let mut fonts = cx.text_system().all_font_names();
    fonts.sort_by_cached_key(|font| font.to_lowercase());
    fonts.dedup();
    let fallbacks = fallback_families(&fonts);
    gpui_kit::base::Root::set_font_fallbacks(Some(FontFallbacks::from_fonts(fallbacks)), cx);
    let emoji = EMOJI_FAMILIES
        .iter()
        .any(|name| fonts.iter().any(|font| font == name));
    let family: Option<SharedString> = [
        "Noto Sans CJK SC",
        "WenQuanYi Micro Hei",
        "Noto Sans",
        "Inter",
    ]
    .into_iter()
    .find(|name| fonts.iter().any(|font| font == name))
    .map(Into::into);
    cx.set_global(FontCatalog {
        families: fonts.into_iter().map(Into::into).collect(),
        ui: family
            .clone()
            .unwrap_or_else(|| Theme::global(cx).font_family.clone()),
        code: Theme::global(cx).mono_font_family.clone(),
        emoji,
    });
    for theme in &mut set.themes {
        theme.font_family = family.clone();
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
    syntax::install(cx);
}

#[cfg(test)]
mod tests {
    use super::{fallback_families, resolve_family};
    use gpui_kit::SharedString;

    #[test]
    fn removed_font_uses_default_without_losing_other_font_choice() {
        let installed = vec![SharedString::from("Sans"), SharedString::from("Code")];
        let fallback = SharedString::from("Sans");
        assert_eq!(
            resolve_family(Some("Removed"), &fallback, &installed),
            fallback
        );
        assert_eq!(resolve_family(None, &fallback, &installed), fallback);
        assert_eq!(resolve_family(Some("Code"), &fallback, &installed), "Code");
    }

    #[test]
    fn glyph_fallback_chain_uses_installed_coverage_fonts_in_order() {
        let installed = [
            "Chosen font",
            "DejaVu Sans",
            "Noto Sans CJK SC",
            "Noto Color Emoji",
        ]
        .map(str::to_owned);
        assert_eq!(
            fallback_families(&installed),
            ["Noto Color Emoji", "Noto Sans CJK SC", "DejaVu Sans"],
        );
        assert!(fallback_families(&["Chosen font".to_owned()]).is_empty());
    }
}
