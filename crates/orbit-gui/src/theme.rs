//! Product palette and shared geometry. Component and Shell consume the same
//! resolved tokens; feature views never introduce literal palette colors.
use gpui_kit::component::{Theme, ThemeSet};
use gpui_kit::{App, SharedString};
use std::rc::Rc;

pub(crate) const CONTENT_WIDTH_REM: f32 = 48.;
pub(crate) const CONTENT_GUTTER_REM: f32 = 3.;
pub(crate) const NAVIGATION_WIDTH_REM: f32 = 2.;

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

#[cfg(test)]
mod tests {
    use gpui_kit::component::ThemeSet;
    use gpui_kit::component::highlighter::{HighlightTheme, SyntaxHighlighter};

    #[test]
    fn shipped_grammars_distinguish_code_from_comments_in_both_themes() {
        let themes: ThemeSet =
            serde_json::from_str(include_str!("../appearance/theme.json")).unwrap();
        for (language, source, token) in [
            ("bash", "cargo run -- --check # 只读检查", "cargo"),
            ("sh", "cargo run -- --check # 只读检查", "cargo"),
            ("rust", "fn main() {} // 中文注释", "fn"),
            ("python", "return 42 # 中文注释", "return"),
        ] {
            let mut highlighter = SyntaxHighlighter::new(language);
            highlighter.update(None, &source.into(), None);
            for config in &themes.themes {
                let theme = HighlightTheme {
                    name: config.name.to_string(),
                    appearance: config.mode,
                    style: config.highlight.clone().unwrap(),
                };
                let styles = highlighter.styles(&(0..source.len()), &theme);
                let color_at = |offset| {
                    styles.iter().rev().find_map(|(range, style)| {
                        range.contains(&offset).then_some(style.color).flatten()
                    })
                };
                let code = color_at(source.find(token).unwrap());
                let comment =
                    color_at(source.find("中文").or_else(|| source.find("只读")).unwrap());
                assert!(code.is_some(), "{language} code must be highlighted");
                assert!(comment.is_some(), "{language} comments must be highlighted");
                assert_ne!(code, comment, "{language} comments must be distinct");
            }
        }
    }
}
