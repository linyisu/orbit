//! Fenced code uses the same language bundle as TUI. Kit retains selection,
//! copy, layout and per-block caching; this adapter only supplies token styles.
use std::ops::Range;

use gpui_kit::base::{TextViewDefaults, text::CodeBlock};
use gpui_kit::component::{Theme, highlighter::HighlightTheme};
use gpui_kit::{App, FontStyle, FontWeight, HighlightStyle, Hsla, Rgba};
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color, FontStyle as SyntaxFontStyle, StyleModifier, ThemeItem, ThemeSettings,
};

pub(super) fn install(cx: &mut App) {
    // Component replaces Base defaults whenever it synchronizes its theme.
    // Reinstall after that transaction, never during render: the callback's
    // identity stays stable across frames so Base can retain parsed highlights.
    cx.observe_global::<Theme>(install_defaults).detach();
    // Startup loads the saved mode and fonts in the same app transaction,
    // before global observers become active. Use that final palette too.
    cx.defer(install_defaults);
}

fn install_defaults(cx: &mut App) {
    let theme = Theme::global(cx);
    let palette = syntax_theme(&theme.highlight_theme, theme.foreground);
    TextViewDefaults::global(cx)
        .with_code_block_highlighter(move |block| highlight(block, &palette))
        .install(cx);
}

/// Translate syntax roles from the active palette, including user theme colors.
/// The scope groups mirror TUI; neither frontend depends on the other's UI.
fn syntax_theme(source: &HighlightTheme, foreground: Hsla) -> syntect::highlighting::Theme {
    syntect::highlighting::Theme {
        name: Some(source.name.clone()),
        settings: ThemeSettings {
            foreground: Some(to_syntax_color(
                source.style.editor_foreground.unwrap_or(foreground),
            )),
            ..Default::default()
        },
        scopes: [
            (
                "keyword - keyword.operator, storage, storage.type",
                "keyword",
            ),
            ("string", "string"),
            ("entity.name.function, support.function", "function"),
            (
                "entity.name.type, entity.name.class, support.type, support.class",
                "type",
            ),
            (
                "constant.numeric, constant.language, constant.character",
                "number",
            ),
            ("entity.name.tag, entity.other.attribute-name", "tag"),
            ("comment, punctuation.definition.comment", "comment"),
            (
                "keyword.operator, punctuation.separator, punctuation.terminator",
                "operator",
            ),
            ("markup.heading, entity.name.section, markup.bold", "type"),
            (
                "markup.italic, markup.bold markup.italic, markup.italic markup.bold",
                "keyword",
            ),
            ("markup.strikethrough", "comment"),
            ("markup.highlight", "number"),
            ("markup.raw.inline, markup.math", "string.special"),
            ("markup.underline.link", "function"),
            ("markup.inserted", "string"),
            ("markup.deleted", "tag"),
            ("meta.diff.header", "function"),
            ("meta.diff.range", "type"),
            (
                "punctuation.definition.heading, punctuation.definition.thematic-break, \
              punctuation.definition.list_item, punctuation.definition.blockquote, \
              punctuation.definition.raw, punctuation.definition.bold, \
              punctuation.definition.italic, punctuation.definition.strikethrough, \
              punctuation.definition.highlight, punctuation.definition.math, \
              punctuation.definition.link, punctuation.definition.image, \
              constant.other.language-name",
                "keyword",
            ),
        ]
        .into_iter()
        .filter_map(|(scope, role)| {
            let style = source.style.syntax.style(role)?;
            let mut font_style = SyntaxFontStyle::empty();
            if style
                .font_weight
                .is_some_and(|weight| weight >= FontWeight::BOLD)
            {
                font_style |= SyntaxFontStyle::BOLD;
            }
            if style.font_style == Some(FontStyle::Italic) {
                font_style |= SyntaxFontStyle::ITALIC;
            }
            Some(ThemeItem {
                scope: scope.parse().expect("valid built-in syntax scope"),
                style: StyleModifier {
                    foreground: style.color.map(to_syntax_color),
                    font_style: Some(font_style),
                    ..Default::default()
                },
            })
        })
        .collect(),
        ..Default::default()
    }
}

fn to_syntax_color(color: Hsla) -> Color {
    let color = color.to_rgb();
    let byte = |value: f32| (value * 255.).round().clamp(0., 255.) as u8;
    Color {
        r: byte(color.r),
        g: byte(color.g),
        b: byte(color.b),
        a: byte(color.a),
    }
}

fn highlight(
    block: &CodeBlock,
    theme: &syntect::highlighting::Theme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some(syntax) = block
        .lang()
        .and_then(|label| orbit_syntax::syntax_for_label(&label))
    else {
        return Vec::new();
    };
    let code = block.code();
    let mut parser = HighlightLines::new(syntax, theme);
    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut offset = 0;
    for line in code.split_inclusive('\n') {
        // The shared grammar bundle is built with no_newlines. Keep parser
        // state across lines, but count the original CR/LF and UTF-8 bytes.
        let text = line.trim_end_matches(['\r', '\n']);
        let Ok(tokens) = parser.highlight_line(text, &orbit_syntax::SYNTAXES) else {
            return Vec::new();
        };
        let mut start = offset;
        for (style, token) in tokens {
            let end = start + token.len();
            let color = style.foreground;
            let style = HighlightStyle {
                color: Some(
                    Rgba {
                        r: color.r as f32 / 255.,
                        g: color.g as f32 / 255.,
                        b: color.b as f32 / 255.,
                        a: color.a as f32 / 255.,
                    }
                    .into(),
                ),
                font_weight: style
                    .font_style
                    .contains(SyntaxFontStyle::BOLD)
                    .then_some(FontWeight::BOLD),
                font_style: style
                    .font_style
                    .contains(SyntaxFontStyle::ITALIC)
                    .then_some(FontStyle::Italic),
                ..Default::default()
            };
            if let Some((previous, previous_style)) = highlights.last_mut()
                && previous.end == start
                && *previous_style == style
            {
                previous.end = end;
            } else if start < end {
                highlights.push((start..end, style));
            }
            start = end;
        }
        offset += line.len();
    }
    highlights
}

#[cfg(test)]
mod tests;
