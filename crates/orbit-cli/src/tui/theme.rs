//! All TUI palette values, syntax themes and color transformations live here.
//! Widgets use semantic roles; syntax palettes and contrast adjustments may use RGB.

use ratatui::style::{Color, Modifier, Style};

/// Semantic colors shared by TUI widgets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub approval_detail: Color,
    pub approval_foreground: Color,
    pub approval_link: Color,
    pub approval_muted: Color,
    pub border_muted: Color,
    pub caret: Color,
    pub context_danger: Color,
    pub danger: Color,
    pub danger_background: Color,
    pub danger_foreground: Color,
    pub description: Color,
    pub detail: Color,
    pub dialog_accent: Color,
    pub diff_added: Color,
    pub diff_added_bg: Color,
    pub diff_context: Color,
    pub diff_removed: Color,
    pub diff_removed_bg: Color,
    pub error: Color,
    pub faint: Color,
    pub foreground: Color,
    pub label_muted: Color,
    pub line_number: Color,
    pub logo_foreground: Color,
    pub logo_shadow: Color,
    pub muted: Color,
    pub muted_text: Color,
    pub navigation_muted: Color,
    pub navigation_palette: [Color; 6],
    pub on_accent: Color,
    pub output_foreground: Color,
    pub output_muted: Color,
    pub overlay_background: Color,
    pub overlay_foreground: Color,
    pub patch_added: Color,
    pub patch_location: Color,
    pub placeholder: Color,
    pub repaint_sentinel: Color,
    pub selection: Color,
    pub selection_foreground: Color,
    pub selection_light_foreground: Color,
    pub strong: Color,
    pub success: Color,
    pub tab_background: Color,
    pub tab_error: Color,
    pub tab_hover_background: Color,
    pub tab_inactive: Color,
    pub text_warm: Color,
    pub tool_detail: Color,
    pub white: Color,
    pub accent: Color,
    pub secondary: Color,
    pub border: Color,
    pub md_h1: Color,
    pub md_h2: Color,
    pub md_h3: Color,
    pub md_h4_plus: Color,
    pub md_bold: Color,
    pub md_italic: Color,
    pub md_bold_italic: Color,
    pub md_strikethrough: Color,
    pub md_mark: Color,
    pub md_inline_code: Color,
    pub md_quote_rail: Color,
    pub md_quote_text: Color,
    pub md_unordered_bullet: Color,
    pub md_ordered_bullet: Color,
    pub md_task_done: Color,
    pub md_link: Color,
    pub md_rule: Color,
    pub md_table_header: Color,
    pub md_table_border: Color,
    pub md_table_separator: Color,
    pub thinking_header_default: Color,
    pub thinking_header_hover: Color,
    pub thinking_rail: Color,
    pub tool_finished: Color,
    pub tool_running: Color,
    pub tool_failed: Color,
    pub tool_summary: Color,
    pub tool_hover: Color,
    pub tool_divider: Color,
    pub tool_border: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    pub const fn dark() -> Self {
        Self {
            approval_detail: Color::Rgb(200, 205, 215),
            approval_foreground: Color::Rgb(210, 215, 225),
            approval_link: Color::Rgb(120, 200, 240),
            approval_muted: Color::Rgb(110, 115, 125),
            border_muted: Color::Rgb(85, 85, 90),
            caret: Color::Gray,
            context_danger: Color::Rgb(220, 100, 88),
            danger: Color::Rgb(235, 135, 135),
            danger_background: Color::Rgb(180, 50, 50),
            danger_foreground: Color::Rgb(255, 160, 160),
            description: Color::Rgb(180, 185, 195),
            detail: Color::Rgb(190, 190, 190),
            dialog_accent: Color::Rgb(220, 140, 100),
            diff_added: Color::Rgb(145, 195, 155),
            diff_added_bg: Color::Rgb(22, 48, 28),
            diff_context: Color::Rgb(100, 100, 100),
            diff_removed: Color::Rgb(218, 139, 139),
            diff_removed_bg: Color::Rgb(55, 24, 28),
            error: Color::Rgb(220, 110, 104),
            faint: Color::Rgb(120, 120, 125),
            foreground: Color::Rgb(220, 222, 226),
            label_muted: Color::Rgb(150, 150, 150),
            line_number: Color::Rgb(90, 95, 105),
            logo_foreground: Color::Rgb(230, 222, 210),
            logo_shadow: Color::Rgb(112, 80, 69),
            muted: Color::Rgb(140, 140, 140),
            muted_text: Color::Rgb(160, 160, 165),
            navigation_muted: Color::Rgb(130, 135, 145),
            navigation_palette: [
                Color::Rgb(130, 135, 145),
                Color::Rgb(147, 151, 159),
                Color::Rgb(163, 165, 169),
                Color::Rgb(170, 165, 157),
                Color::Rgb(158, 151, 145),
                Color::Rgb(142, 139, 142),
            ],
            on_accent: Color::Black,
            output_foreground: Color::Rgb(222, 222, 222),
            output_muted: Color::Rgb(155, 155, 155),
            overlay_background: Color::Rgb(48, 48, 52),
            overlay_foreground: Color::Rgb(222, 218, 212),
            patch_added: Color::Rgb(139, 195, 143),
            patch_location: Color::Rgb(139, 185, 195),
            placeholder: Color::Rgb(178, 184, 194),
            repaint_sentinel: Color::Rgb(1, 2, 3),
            selection: Color::Rgb(78, 72, 103),
            selection_foreground: Color::Rgb(222, 222, 222),
            selection_light_foreground: Color::Rgb(255, 255, 255),
            strong: Color::Rgb(230, 230, 230),
            success: Color::Rgb(114, 205, 142),
            tab_background: Color::Rgb(40, 38, 40),
            tab_error: Color::Rgb(220, 111, 122),
            tab_hover_background: Color::Rgb(47, 43, 45),
            tab_inactive: Color::Rgb(177, 171, 171),
            text_warm: Color::Rgb(230, 225, 218),
            tool_detail: Color::Rgb(160, 160, 160),
            white: Color::White,
            accent: Color::Rgb(218, 139, 109),
            secondary: Color::Rgb(170, 170, 170),
            border: Color::Rgb(85, 85, 85),
            md_h1: Color::Rgb(235, 140, 115),
            md_h2: Color::Rgb(230, 175, 110),
            md_h3: Color::Rgb(145, 205, 175),
            md_h4_plus: Color::Rgb(140, 190, 230),
            md_bold: Color::Rgb(255, 215, 140),
            md_italic: Color::Rgb(185, 175, 225),
            md_bold_italic: Color::Rgb(255, 185, 205),
            md_strikethrough: Color::Rgb(160, 165, 175),
            md_mark: Color::Rgb(255, 195, 110),
            md_inline_code: Color::Rgb(145, 220, 225),
            md_quote_rail: Color::Rgb(120, 175, 235),
            md_quote_text: Color::Rgb(190, 195, 205),
            md_unordered_bullet: Color::Rgb(125, 210, 190),
            md_ordered_bullet: Color::Rgb(135, 195, 235),
            md_task_done: Color::Rgb(120, 215, 150),
            md_link: Color::Rgb(110, 190, 255),
            md_rule: Color::Rgb(110, 125, 145),
            md_table_header: Color::Rgb(240, 190, 120),
            md_table_border: Color::Rgb(110, 125, 145),
            md_table_separator: Color::Rgb(100, 110, 125),
            thinking_header_default: Color::Rgb(185, 165, 225),
            thinking_header_hover: Color::Rgb(245, 190, 255),
            thinking_rail: Color::Rgb(145, 145, 145),
            tool_finished: Color::Rgb(105, 175, 135),
            tool_running: Color::Rgb(220, 160, 90),
            tool_failed: Color::Rgb(225, 110, 105),
            tool_summary: Color::Rgb(160, 165, 175),
            tool_hover: Color::Rgb(140, 225, 170),
            tool_divider: Color::Rgb(110, 125, 145),
            tool_border: Color::Rgb(135, 145, 160),
        }
    }
}

pub const THEME: Theme = Theme::dark();

/// Use one subdued background while retaining token colors and readable contrast.
pub(super) fn selection_style(style: Style) -> Style {
    let (source_foreground, source_background) = if style.add_modifier.contains(Modifier::REVERSED)
    {
        (style.bg, style.fg)
    } else {
        (style.fg, style.bg)
    };
    let background = color_rgb(THEME.selection).unwrap();
    let mut foreground = color_rgb(source_foreground.unwrap_or(Color::Reset))
        .unwrap_or_else(|| color_rgb(THEME.selection_foreground).unwrap());
    if contrast(foreground, background) < 4.5
        && let Some(badge) = source_background
            .and_then(color_rgb)
            .filter(|color| contrast(*color, background) > contrast(foreground, background))
    {
        foreground = badge;
    }
    let original = foreground;
    let light = color_rgb(THEME.selection_light_foreground).unwrap();
    for step in 1..=16 {
        if contrast(foreground, background) >= 4.5 {
            break;
        }
        let blend =
            |from: u8, to: u8| ((u16::from(from) * (16 - step) + u16::from(to) * step) / 16) as u8;
        foreground = (
            blend(original.0, light.0),
            blend(original.1, light.1),
            blend(original.2, light.2),
        );
    }

    let (red, green, blue) = foreground;
    let (bg_red, bg_green, bg_blue) = background;
    style
        .fg(Color::Rgb(red, green, blue))
        .bg(Color::Rgb(bg_red, bg_green, bg_blue))
        .remove_modifier(Modifier::REVERSED | Modifier::DIM)
}

pub(super) fn color_rgb(color: Color) -> Option<(u8, u8, u8)> {
    let index = match color {
        Color::Reset => return None,
        Color::Rgb(red, green, blue) => return Some((red, green, blue)),
        Color::Indexed(index) => index,
        Color::Black => 0,
        Color::Red => 1,
        Color::Green => 2,
        Color::Yellow => 3,
        Color::Blue => 4,
        Color::Magenta => 5,
        Color::Cyan => 6,
        Color::Gray => 7,
        Color::DarkGray => 8,
        Color::LightRed => 9,
        Color::LightGreen => 10,
        Color::LightYellow => 11,
        Color::LightBlue => 12,
        Color::LightMagenta => 13,
        Color::LightCyan => 14,
        Color::White => 15,
    };
    let rgb = match index {
        0..=15 => [
            (0, 0, 0),
            (128, 0, 0),
            (0, 128, 0),
            (128, 128, 0),
            (0, 0, 128),
            (128, 0, 128),
            (0, 128, 128),
            (192, 192, 192),
            (128, 128, 128),
            (255, 0, 0),
            (0, 255, 0),
            (255, 255, 0),
            (0, 0, 255),
            (255, 0, 255),
            (0, 255, 255),
            (255, 255, 255),
        ][usize::from(index)],
        16..=231 => {
            let value = index - 16;
            let channel = |level| if level == 0 { 0 } else { 55 + level * 40 };
            (
                channel(value / 36),
                channel(value / 6 % 6),
                channel(value % 6),
            )
        }
        _ => {
            let shade = 8 + (index - 232) * 10;
            (shade, shade, shade)
        }
    };
    Some(rgb)
}

fn luminance((red, green, blue): (u8, u8, u8)) -> f64 {
    let linear = |channel| {
        let channel = f64::from(channel) / 255.0;
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
}

fn contrast(foreground: (u8, u8, u8), background: (u8, u8, u8)) -> f64 {
    let foreground = luminance(foreground);
    let background = luminance(background);
    (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
}

/// Stable language hues with a bounded brightness, including unknown languages.
pub(super) fn code_label_color(key: &str) -> Color {
    let hash = key.bytes().fold(0xcbf29ce484222325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    let hue = ((hash ^ (hash >> 32)) % 1536) as u16;
    let low = 146 + ((hash >> 40) % 16) as u8;
    let high = 208 + ((hash >> 48) % 16) as u8;
    let offset = ((hue % 256) * u16::from(high - low) / 255) as u8;
    let rising = low + offset;
    let falling = high - offset;
    let (red, green, blue) = match hue / 256 {
        0 => (high, rising, low),
        1 => (falling, high, low),
        2 => (low, high, rising),
        3 => (low, falling, high),
        4 => (rising, low, high),
        _ => (high, low, falling),
    };
    Color::Rgb(red, green, blue)
}

pub(super) fn desaturate_ansi_color(color: Color) -> Color {
    let (r, g, b) = match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Red => (210, 80, 75),
        Color::LightRed => (235, 110, 105),
        Color::Green => (90, 175, 110),
        Color::LightGreen => (120, 205, 140),
        Color::Yellow => (210, 175, 80),
        Color::LightYellow => (235, 200, 105),
        Color::Blue => (95, 150, 215),
        Color::LightBlue => (125, 180, 235),
        Color::Magenta => (180, 115, 195),
        Color::LightMagenta => (205, 140, 220),
        Color::Cyan => (100, 180, 185),
        Color::LightCyan => (130, 205, 210),
        Color::White => (205, 205, 205),
        Color::Gray | Color::DarkGray => (130, 130, 130),
        Color::Black => (40, 40, 40),
        _ => return color,
    };
    let gray = 140.0;
    Color::Rgb(
        (r as f32 * 0.75 + gray * 0.25) as u8,
        (g as f32 * 0.75 + gray * 0.25) as u8,
        (b as f32 * 0.75 + gray * 0.25) as u8,
    )
}

pub(super) fn desaturate_for_thinking(color: Option<Color>) -> Color {
    let (red, green, blue) = match color.unwrap_or(Color::Rgb(218, 218, 212)) {
        Color::Rgb(red, green, blue) => (red, green, blue),
        Color::Black => (0, 0, 0),
        Color::Red | Color::LightRed => (220, 110, 104),
        Color::Green | Color::LightGreen => (114, 205, 142),
        Color::Yellow | Color::LightYellow => (224, 194, 110),
        Color::Blue | Color::LightBlue => (128, 180, 225),
        Color::Magenta | Color::LightMagenta => (205, 144, 215),
        Color::Cyan | Color::LightCyan => (126, 205, 208),
        Color::Gray | Color::DarkGray => (155, 155, 155),
        Color::White => (232, 232, 228),
        Color::Indexed(_) | Color::Reset => (190, 190, 188),
    };

    let blend = |channel| ((u16::from(channel) * 7 + 175 * 3) / 10) as u8;

    Color::Rgb(blend(red), blend(green), blend(blue))
}

use std::sync::LazyLock;
use syntect::highlighting::{Color as SyntaxColor, StyleModifier, ThemeItem, ThemeSettings};
use syntect::parsing::Scope;

/// Terminal effects follow syntax scopes in every source view, including
/// nested scopes and embedded languages; no source text is removed or parsed again.
pub(super) fn syntax_modifiers(scopes: &[Scope]) -> Modifier {
    static RULES: LazyLock<[(Scope, Modifier); 5]> = LazyLock::new(|| {
        [
            ("markup.heading", Modifier::BOLD),
            ("markup.bold", Modifier::BOLD),
            ("markup.italic", Modifier::ITALIC),
            ("markup.strikethrough", Modifier::CROSSED_OUT),
            ("markup.highlight", Modifier::UNDERLINED),
        ]
        .map(|(scope, modifier)| (scope.parse().expect("valid syntax effect scope"), modifier))
    });
    RULES
        .iter()
        .fold(Modifier::empty(), |effects, (scope, modifier)| {
            if scopes.iter().any(|token| scope.is_prefix_of(*token)) {
                effects | *modifier
            } else {
                effects
            }
        })
}

pub(super) static CODE_THEME: LazyLock<syntect::highlighting::Theme> = LazyLock::new(|| {
    let color = |r, g, b| SyntaxColor { r, g, b, a: 255 };
    let semantic = |value| {
        let (red, green, blue) = color_rgb(value).expect("explicit syntax palette color");
        color(red, green, blue)
    };
    syntect::highlighting::Theme {
        name: Some("Orbit Vibrant".into()),
        settings: ThemeSettings {
            foreground: Some(color(225, 230, 240)),
            ..Default::default()
        },
        scopes: [
            (
                "keyword - keyword.operator, storage, storage.type",
                color(198, 120, 221),
            ),
            ("string", color(152, 195, 121)),
            (
                "entity.name.function, support.function",
                color(97, 175, 239),
            ),
            (
                "entity.name.type, entity.name.class, support.type, support.class",
                color(229, 192, 123),
            ),
            (
                "constant.numeric, constant.language, constant.character",
                color(209, 154, 102),
            ),
            (
                "entity.name.tag, entity.other.attribute-name",
                color(224, 108, 117),
            ),
            (
                "comment, punctuation.definition.comment",
                color(163, 173, 190),
            ),
            (
                "keyword.operator, punctuation.separator, punctuation.terminator",
                color(171, 178, 191),
            ),
            ("markup.heading, entity.name.section", color(229, 192, 123)),
            ("markup.bold", color(229, 192, 123)),
            ("markup.italic", color(198, 120, 221)),
            (
                "markup.bold markup.italic, markup.italic markup.bold",
                semantic(THEME.md_bold_italic),
            ),
            ("markup.strikethrough", semantic(THEME.md_strikethrough)),
            ("markup.highlight", semantic(THEME.md_mark)),
            ("markup.raw.inline", color(126, 205, 208)),
            ("markup.underline.link", color(97, 175, 239)),
            ("markup.math", color(126, 205, 208)),
            ("markup.inserted", semantic(THEME.patch_added)),
            ("markup.deleted", semantic(THEME.diff_removed)),
            ("meta.diff.header", semantic(THEME.accent)),
            ("meta.diff.range", semantic(THEME.patch_location)),
            (
                "punctuation.definition.heading, punctuation.definition.thematic-break, \
                 punctuation.definition.list_item, punctuation.definition.blockquote, \
                 punctuation.definition.raw, punctuation.definition.bold, \
                 punctuation.definition.italic, punctuation.definition.strikethrough, \
                 punctuation.definition.highlight, \
                 punctuation.definition.math, punctuation.definition.link, \
                 punctuation.definition.image, constant.other.language-name",
                color(198, 120, 221),
            ),
        ]
        .into_iter()
        .map(|(scope, foreground)| ThemeItem {
            scope: scope.parse().expect("valid built-in syntax scope"),
            style: StyleModifier {
                foreground: Some(foreground),
                ..Default::default()
            },
        })
        .collect(),
        ..Default::default()
    }
});

pub(super) fn control_style(hover: Option<std::time::Duration>) -> Style {
    let base = Style::default()
        .fg(THEME.accent)
        .remove_modifier(Modifier::BOLD);
    hover.map_or(base, |elapsed| base.bg(control_hover_background(elapsed)))
}

fn control_hover_background(elapsed: std::time::Duration) -> Color {
    let shade = 48 + (elapsed.as_millis().min(240) / 10) as u8;
    Color::Rgb(shade, shade - 8, shade - 10)
}
pub(super) fn ansi_color(code: u16) -> Option<Color> {
    match code {
        30 => Some(Color::Black),
        31 => Some(Color::Red),
        32 => Some(Color::Green),
        33 => Some(Color::Yellow),
        34 => Some(Color::Blue),
        35 => Some(Color::Magenta),
        36 => Some(Color::Cyan),
        37 => Some(Color::White),
        90 => Some(Color::DarkGray),
        91 => Some(Color::LightRed),
        92 => Some(Color::LightGreen),
        93 => Some(Color::LightYellow),
        94 => Some(Color::LightBlue),
        95 => Some(Color::LightMagenta),
        96 => Some(Color::LightCyan),
        97 => Some(Color::White),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_keeps_token_colors_badges_and_text_modifiers() {
        let modifiers = Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED;
        let token = Color::Rgb(97, 175, 239);
        let selected = selection_style(
            Style::default()
                .fg(token)
                .add_modifier(modifiers | Modifier::DIM),
        );
        assert_eq!(selected.bg, Some(THEME.selection));
        let foreground = color_rgb(selected.fg.unwrap()).unwrap();
        assert!(foreground.2 > foreground.1 && foreground.1 > foreground.0);
        assert!(contrast(foreground, color_rgb(THEME.selection).unwrap()) >= 4.5);
        assert!(selected.add_modifier.contains(modifiers));
        assert!(!selected.add_modifier.contains(Modifier::DIM));

        for language in ["python", "rust", "orbit-script"] {
            let badge = code_label_color(language);
            let selected = selection_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(badge)
                    .add_modifier(Modifier::BOLD),
            );
            assert_eq!(selected.bg, Some(THEME.selection));
            let (red, green, blue) = color_rgb(badge).unwrap();
            let (selected_red, selected_green, selected_blue) =
                color_rgb(selected.fg.unwrap()).unwrap();
            assert_eq!(red.cmp(&green), selected_red.cmp(&selected_green));
            assert_eq!(red.cmp(&blue), selected_red.cmp(&selected_blue));
            assert_eq!(green.cmp(&blue), selected_green.cmp(&selected_blue));
            assert!(selected.add_modifier.contains(Modifier::BOLD));
        }
    }

    #[test]
    fn selection_handles_default_dark_light_and_already_reversed_styles() {
        for (source, foreground) in [
            (Style::default(), THEME.selection_foreground),
            (
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(240, 240, 240)),
                Color::Rgb(255, 255, 255),
            ),
            (
                Style::default()
                    .fg(Color::Rgb(222, 222, 222))
                    .bg(Color::Rgb(30, 30, 30)),
                Color::Rgb(222, 222, 222),
            ),
            (
                Style::default()
                    .fg(Color::Rgb(80, 80, 80))
                    .bg(Color::Rgb(240, 240, 240))
                    .add_modifier(Modifier::REVERSED),
                Color::Rgb(240, 240, 240),
            ),
        ] {
            let selected = selection_style(source);
            assert_eq!(selected.fg, Some(foreground));
            assert_eq!(selected.bg, Some(THEME.selection));
            assert!(!selected.add_modifier.contains(Modifier::REVERSED));
        }
        let source = Color::Rgb(16, 16, 16);
        let selected = selection_style(Style::default().fg(source));
        assert_ne!(selected.fg, Some(source));
        assert_eq!(selected.bg, Some(THEME.selection));
        assert!(
            contrast(
                color_rgb(selected.fg.unwrap()).unwrap(),
                color_rgb(THEME.selection).unwrap()
            ) >= 4.5
        );
    }

    #[test]
    fn selection_contrast_remains_readable_across_indexed_colors_and_rgb_hues() {
        assert_eq!(contrast((0, 0, 0), (255, 255, 255)), 21.0);
        let check = |color| {
            for background in [
                Color::Reset,
                Color::Rgb(24, 24, 24),
                Color::Rgb(240, 240, 240),
            ] {
                let selected = selection_style(Style::default().fg(color).bg(background));
                let foreground = color_rgb(selected.fg.unwrap()).unwrap();
                let background = color_rgb(selected.bg.unwrap()).unwrap();
                assert!(
                    contrast(foreground, background) >= 4.5,
                    "unreadable selection for {color:?}: {selected:?}"
                );
            }
        };
        for index in 0..=255 {
            check(Color::Indexed(index));
        }
        for red in [0, 24, 85, 120, 200, 255] {
            for green in [0, 24, 85, 120, 200, 255] {
                for blue in [0, 24, 85, 120, 200, 255] {
                    check(Color::Rgb(red, green, blue));
                }
            }
        }
    }
}
