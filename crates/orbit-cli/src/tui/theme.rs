//! All TUI palette values, syntax themes and color transformations live here.
//! Widgets use semantic theme roles; they must not define their own RGB literals.

use ratatui::style::Color;

/// Centralized TUI Theme palette for Orbit CLI.
/// Designed for high readability, distinct rich visual hierarchy,
/// and future theme customization / theme switching support.
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
    pub strong: Color,
    pub success: Color,
    pub tab_background: Color,
    pub tab_error: Color,
    pub tab_hover_background: Color,
    pub tab_inactive: Color,
    pub text_warm: Color,
    pub tool_detail: Color,
    pub white: Color,

    // Brand / Base Accent Colors
    pub accent: Color,
    pub secondary: Color,
    pub border: Color,

    // Markdown Heading Hierarchy
    pub md_h1: Color,
    pub md_h2: Color,
    pub md_h3: Color,
    pub md_h4_plus: Color,

    // Markdown Inline Typography
    pub md_bold: Color,
    pub md_italic: Color,
    pub md_bold_italic: Color,
    pub md_strikethrough: Color,
    pub md_mark: Color,
    pub md_inline_code: Color,

    // Markdown Blocks, Lists, Links, Rules & Tables
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

    // Thinking States
    pub thinking_header_default: Color,
    pub thinking_header_hover: Color,
    pub thinking_rail: Color,

    // Tool Call States
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
    /// Default dark theme with vibrant, high-contrast, non-overlapping color palette.
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
            on_accent: Color::Black,
            output_foreground: Color::Rgb(222, 222, 222),
            output_muted: Color::Rgb(155, 155, 155),
            overlay_background: Color::Rgb(48, 48, 52),
            overlay_foreground: Color::Rgb(222, 218, 212),
            patch_added: Color::Rgb(139, 195, 143),
            patch_location: Color::Rgb(139, 185, 195),
            placeholder: Color::Rgb(178, 184, 194),
            repaint_sentinel: Color::Rgb(1, 2, 3),
            selection: Color::Blue,
            strong: Color::Rgb(230, 230, 230),
            success: Color::Rgb(114, 205, 142),
            tab_background: Color::Rgb(40, 38, 40),
            tab_error: Color::Rgb(220, 111, 122),
            tab_hover_background: Color::Rgb(47, 43, 45),
            tab_inactive: Color::Rgb(177, 171, 171),
            text_warm: Color::Rgb(230, 225, 218),
            tool_detail: Color::Rgb(160, 160, 160),
            white: Color::White,
            accent: Color::Rgb(218, 139, 109), // Warm Amber / Terracotta
            secondary: Color::Rgb(170, 170, 170), // Muted Gray
            border: Color::Rgb(85, 85, 85),    // Border Gray

            // Markdown Headings - Distinct Warm-to-Cool Palette
            md_h1: Color::Rgb(235, 140, 115), // H1: Coral / Warm Crimson
            md_h2: Color::Rgb(230, 175, 110), // H2: Golden Amber
            md_h3: Color::Rgb(145, 205, 175), // H3: Jade Green
            md_h4_plus: Color::Rgb(140, 190, 230), // H4+: Sky Cyan

            // Markdown Inline Typography - Rich Vibrant Accents
            md_bold: Color::Rgb(255, 215, 140), // Bold: Bright Golden Yellow
            md_italic: Color::Rgb(185, 175, 225), // Italic: Soft Violet
            md_bold_italic: Color::Rgb(255, 185, 205), // Bold+Italic: Peach Pink
            md_strikethrough: Color::Rgb(160, 165, 175), // Strikethrough: Cool Gray
            md_mark: Color::Rgb(255, 195, 110), // Mark/Highlight: Gold Underline
            md_inline_code: Color::Rgb(145, 220, 225), // Inline Code: Ice Cyan

            // Markdown Blocks, Lists, Links, Rules & Tables
            md_quote_rail: Color::Rgb(120, 175, 235), // Quote Rail: Celestial Blue
            md_quote_text: Color::Rgb(190, 195, 205), // Quote Text: Soft Mist Gray
            md_unordered_bullet: Color::Rgb(125, 210, 190), // Unordered List • : Mint Cyan
            md_ordered_bullet: Color::Rgb(135, 195, 235), // Ordered List 1. : Ice Blue
            md_task_done: Color::Rgb(120, 215, 150),  // Task Done ☑ : Emerald Green
            md_link: Color::Rgb(110, 190, 255),       // Hyperlink: Electric Sky Blue
            md_rule: Color::Rgb(110, 125, 145), // Horizontal Rule / Divider: Metallic Blue-Gray
            md_table_header: Color::Rgb(240, 190, 120), // Table Header: Golden Amber
            md_table_border: Color::Rgb(110, 125, 145), // Table Border: Metallic Blue-Gray
            md_table_separator: Color::Rgb(100, 110, 125), // Table Separator

            // Thinking States
            thinking_header_default: Color::Rgb(185, 165, 225), // Soft Amethyst Violet
            thinking_header_hover: Color::Rgb(245, 190, 255),   // Neon Electric Purple-Pink
            thinking_rail: Color::Rgb(145, 145, 145),           // Thinking Vertical Rail

            // Tool Call States
            tool_finished: Color::Rgb(105, 175, 135), // Soft Mint Green
            tool_running: Color::Rgb(220, 160, 90),   // Warm Amber Yellow
            tool_failed: Color::Rgb(225, 110, 105),   // Soft Coral Red
            tool_summary: Color::Rgb(160, 165, 175),  // Muted Slate Gray
            tool_hover: Color::Rgb(140, 225, 170),    // Bright Emerald Mint (Hover)
            tool_divider: Color::Rgb(110, 125, 145),  // Tool Card Divider
            tool_border: Color::Rgb(135, 145, 160),   // Tool Card Enclosure Border
        }
    }
}

/// Global active theme instance.
pub const THEME: Theme = Theme::dark();

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
pub(super) static CODE_THEME: LazyLock<syntect::highlighting::Theme> = LazyLock::new(|| {
    let color = |r, g, b| SyntaxColor { r, g, b, a: 255 };
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
                color(120, 130, 145),
            ),
            (
                "keyword.operator, punctuation.separator, punctuation.terminator",
                color(171, 178, 191),
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

pub(super) fn tab_hover_background(elapsed: std::time::Duration) -> Color {
    let shade = 48 + (elapsed.as_millis().min(240) / 10) as u8;
    Color::Rgb(shade, shade - 8, shade - 10)
}
pub(super) fn diff_syntax_color(
    color: syntect::highlighting::Color,
    tag: similar::ChangeTag,
) -> Color {
    let tint = match tag {
        similar::ChangeTag::Delete => THEME.error,
        similar::ChangeTag::Insert => THEME.success,
        similar::ChangeTag::Equal => return Color::Rgb(color.r, color.g, color.b),
    };
    let Color::Rgb(r, g, b) = tint else {
        return tint;
    };
    Color::Rgb(
        ((color.r as u16 + r as u16) / 2) as u8,
        ((color.g as u16 + g as u16) / 2) as u8,
        ((color.b as u16 + b as u16) / 2) as u8,
    )
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
