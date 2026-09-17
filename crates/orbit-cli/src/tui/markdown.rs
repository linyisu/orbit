//! Render complete source lines incrementally; never reparse the conversation on scroll.
//! CommonMark inline syntax is handled by pulldown-cmark, not regex replacements.
use pulldown_cmark::{Event, Options, Parser, Tag};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use std::sync::LazyLock;
use syntect::{
    easy::HighlightLines,
    highlighting::{
        Color as SyntaxColor, HighlightState, StyleModifier, Theme, ThemeItem, ThemeSettings,
    },
    parsing::{ParseState, SyntaxReference, SyntaxSet},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

// Link grammars at build time, not when the user first resumes a conversation.
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(|| {
    syntect::dumps::from_uncompressed_data(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/syntaxes.bin"
    )))
    .expect("valid compiled syntax bundle")
});
// Follow bat's scope-based themes with a restrained, readable palette.
// Reference: https://github.com/sharkdp/bat/blob/master/assets/themes/ansi.tmTheme
// Keep semantic distinctions with light, low-saturation colors. Comments stay
// readable on translucent terminals; punctuation uses the warm base foreground.
static CODE_THEME: LazyLock<Theme> = LazyLock::new(|| {
    let color = |r, g, b| SyntaxColor { r, g, b, a: 255 };
    Theme {
        name: Some("Orbit restrained".into()),
        settings: ThemeSettings {
            foreground: Some(color(234, 232, 227)),
            ..Default::default()
        },
        scopes: [
            ("keyword - keyword.operator, storage", color(197, 218, 244)),
            ("string", color(215, 229, 192)),
            (
                "entity.name.function, support.function",
                color(198, 230, 230),
            ),
            (
                "entity.name.type, entity.name.class, support.type, support.class",
                color(224, 211, 240),
            ),
            (
                "constant.numeric, constant.language, constant.character",
                color(240, 217, 183),
            ),
            (
                "entity.name.tag, entity.other.attribute-name",
                color(197, 218, 244),
            ),
            (
                "comment, punctuation.definition.comment",
                color(190, 196, 205),
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CodeFence {
    delimiter: char,
    length: usize,
    state: (HighlightState, ParseState),
}
pub(super) type Fence = Option<CodeFence>;
const ACCENT: Color = Color::Rgb(218, 139, 109);
const SECONDARY: Color = Color::Rgb(170, 170, 170);

fn push_text_with_cjk_strong(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    style: Style,
    recover: bool,
) {
    if !recover || !text.contains("**") {
        spans.push(Span::styled(text.to_owned(), style));
        return;
    }
    let mut rest = text;
    while let Some(open) = rest.find("**") {
        let after_open = &rest[open + 2..];
        let Some(close) = after_open.find("**") else {
            break;
        };
        let content = &after_open[..close];
        if content.is_empty()
            || content.starts_with(char::is_whitespace)
            || content.ends_with(char::is_whitespace)
        {
            let split = open + 2;
            spans.push(Span::styled(rest[..split].to_owned(), style));
            rest = &rest[split..];
            continue;
        }
        if open > 0 {
            spans.push(Span::styled(rest[..open].to_owned(), style));
        }
        spans.push(Span::styled(
            content.to_owned(),
            style.add_modifier(Modifier::BOLD),
        ));
        rest = &after_open[close + 2..];
    }
    if !rest.is_empty() {
        spans.push(Span::styled(rest.to_owned(), style));
    }
}

fn recover_split_strong(spans: &mut Vec<Span<'static>>, start: usize, recover: bool) {
    if !recover {
        return;
    }
    let mut open = start;
    while open + 1 < spans.len() {
        if spans[open].content != "*" || spans[open + 1].content != "*" {
            open += 1;
            continue;
        }
        let Some(close) = (open + 2..spans.len().saturating_sub(1))
            .find(|&index| spans[index].content == "*" && spans[index + 1].content == "*")
        else {
            break;
        };
        let first = spans[open + 2..close]
            .iter()
            .find_map(|span| span.content.chars().next());
        let last = spans[open + 2..close]
            .iter()
            .rev()
            .find_map(|span| span.content.chars().next_back());
        if first.is_none_or(char::is_whitespace) || last.is_none_or(char::is_whitespace) {
            open += 2;
            continue;
        }
        spans.drain(close..close + 2);
        spans.drain(open..open + 2);
        for span in &mut spans[open..close - 2] {
            span.style = span.style.add_modifier(Modifier::BOLD);
        }
        open = close - 2;
    }
}

/// Parse only a possible table at the current source position. The caller keeps
/// an open table provisional so new rows can adjust column widths during streaming.
pub(super) fn table(source: &str, width: u16) -> Option<(usize, Vec<Line<'static>>)> {
    if !source.lines().next()?.contains('|') {
        return None;
    }
    let mut events = Parser::new_ext(source, Options::ENABLE_TABLES).into_offset_iter();
    let (Event::Start(Tag::Table(alignments)), _) = events.next()? else {
        return None;
    };
    let mut rows: Vec<Vec<Line<'static>>> = Vec::new();
    let mut end = 0;
    for (event, range) in events {
        match event {
            Event::Start(Tag::TableHead | Tag::TableRow) => rows.push(Vec::new()),
            Event::Start(Tag::TableCell) => {
                let cell_source = source[range].trim().replace(r"\|", "|");
                let mut cell = line(&cell_source, 1, &mut None);
                cell.spans.remove(0);
                super::render::sanitize_line(&mut cell);
                rows.last_mut()?.push(cell);
            }
            Event::End(pulldown_cmark::TagEnd::Table) => {
                end = range.end;
                break;
            }
            _ => {}
        }
    }
    if end == 0 || rows.is_empty() {
        return None;
    }
    let cols = alignments.len();
    let available = usize::from(width).saturating_sub(3 * cols + 3);
    // On narrow terminals use labelled cells rather than losing columns/content.
    if available < cols * 3 {
        let mut lines = Vec::new();
        for row in rows.iter().skip(1) {
            for (i, cell) in row.iter().enumerate() {
                let mut spans = vec![Span::styled(
                    format!("  {}: ", rows[0][i]),
                    Style::default().fg(ACCENT),
                )];
                spans.extend(cell.spans.clone());
                lines.push(Line::from(spans));
            }
            lines.push(Line::from(""));
        }
        if lines.is_empty() {
            lines.extend(rows.remove(0));
        }
        return Some((end, lines));
    }
    let mut widths: Vec<usize> = (0..cols)
        .map(|i| {
            rows.iter()
                .filter_map(|r| r.get(i))
                .map(Line::width)
                .max()
                .unwrap_or(1)
                .max(3)
        })
        .collect();
    while widths.iter().sum::<usize>() > available {
        let Some(i) = (0..cols).max_by_key(|&i| widths[i]) else {
            break;
        };
        widths[i] -= 1;
    }
    let border = |left: &str, join: &str, right: &str| {
        Line::styled(
            format!(
                "  {left}{}{right}",
                widths
                    .iter()
                    .map(|w| "─".repeat(w + 2))
                    .collect::<Vec<_>>()
                    .join(join)
            ),
            Style::default().fg(SECONDARY),
        )
    };
    let mut lines = vec![border("┌", "┬", "┐")];
    for (r, row) in rows.iter().enumerate() {
        let cells: Vec<_> = row
            .iter()
            .zip(&widths)
            .map(|(cell, w)| wrap_cell(cell, *w))
            .collect();
        for n in 0..cells.iter().map(Vec::len).max().unwrap_or(1) {
            let mut spans = vec![Span::styled("  │", Style::default().fg(SECONDARY))];
            for i in 0..cols {
                let cell = cells
                    .get(i)
                    .and_then(|c| c.get(n))
                    .cloned()
                    .unwrap_or_default();
                let padding = widths[i].saturating_sub(cell.width());
                let left = match alignments[i] {
                    pulldown_cmark::Alignment::Right => padding,
                    pulldown_cmark::Alignment::Center => padding / 2,
                    _ => 0,
                };
                spans.push(Span::raw(" ".repeat(left + 1)));
                spans.extend(cell.spans.into_iter().map(|mut span| {
                    if r == 0 {
                        span.style = span.style.add_modifier(Modifier::BOLD).fg(ACCENT);
                    }
                    span
                }));
                spans.push(Span::raw(" ".repeat(padding - left + 1)));
                spans.push(Span::styled("│", Style::default().fg(SECONDARY)));
            }
            lines.push(Line::from(spans));
        }
        if r + 1 < rows.len() {
            let mut separator = border("├", "┼", "┤");
            if r > 0 {
                separator.style = Style::default().fg(Color::Rgb(115, 115, 115));
            }
            lines.push(separator);
        }
    }
    lines.push(border("└", "┴", "┘"));
    Some((end, lines))
}

fn wrap_cell(cell: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::default()];
    let mut used = 0;
    for span in &cell.spans {
        for grapheme in span.content.graphemes(true) {
            let size = grapheme.width();
            if used + size > width && used > 0 {
                lines.push(Line::default());
                used = 0;
            }
            lines
                .last_mut()
                .unwrap()
                .spans
                .push(Span::styled(grapheme.to_owned(), span.style));
            used += size;
        }
    }
    lines
}

/// Resolve standard names, common Markdown aliases, attributes and filenames.
/// Ambiguous platform names (e.g. .NET) and unknown labels stay plain text.
fn syntax_for_label(info: &str) -> Option<&'static SyntaxReference> {
    let normalized = info
        .trim()
        .trim_matches(|c| c == '{' || c == '}')
        .trim_start_matches('.')
        .to_ascii_lowercase();
    let full = normalized.strip_prefix("language-").unwrap_or(&normalized);
    if let Some(syntax) = SYNTAXES.find_syntax_by_token(full) {
        return Some(syntax);
    }
    let token = match full {
        "visual basic" | "visual basic .net" | "visual basic.net" => "vb",
        "c sharp" => "cs",
        other => other.split_whitespace().next().unwrap_or(""),
    };
    let token = token.trim_start_matches('.');
    let token = match token {
        "csharp" | "c#" | "c-sharp" => "cs",
        "vb.net" | "vbnet" | "visualbasic" | "visualbasic.net" => "vb",
        "fsharp" | "f#" => "fs",
        "c++" | "cplusplus" => "cpp",
        "rs" => "rust",
        "js" | "node" | "nodejs" => "javascript",
        "py" | "python3" => "python",
        "sh" | "shell" | "console" => "bash",
        "ts" => "typescript",
        "tsx" => "TypescriptReact",
        "yml" => "yaml",
        "text" | "txt" | "plaintext" => return Some(SYNTAXES.find_syntax_plain_text()),
        other => other,
    };
    SYNTAXES.find_syntax_by_token(token).or_else(|| {
        token
            .rsplit_once('.')
            .and_then(|(_, ext)| SYNTAXES.find_syntax_by_extension(ext))
    })
}

pub(super) fn line(source: &str, _index: usize, fence: &mut Fence) -> Line<'static> {
    let trimmed = source.trim_start();
    let marker = trimmed.chars().next().unwrap_or(' ');
    let count = trimmed.chars().take_while(|c| *c == marker).count();
    if let Some(code) = fence {
        if marker == code.delimiter && count >= code.length && trimmed[count..].trim().is_empty() {
            *fence = None;
            return Line::styled("  └─", Style::default().fg(SECONDARY));
        }
        let mut highlighter =
            HighlightLines::from_state(&CODE_THEME, code.state.0.clone(), code.state.1.clone());
        let mut spans = vec![Span::styled("  │ ", Style::default().fg(SECONDARY))];
        match highlighter.highlight_line(source, &SYNTAXES) {
            Ok(tokens) => spans.extend(tokens.into_iter().map(|(style, text)| {
                let c = style.foreground;
                Span::styled(
                    text.to_owned(),
                    Style::default().fg(Color::Rgb(c.r, c.g, c.b)),
                )
            })),
            Err(_) => spans.push(Span::raw(source.to_owned())),
        }
        code.state = highlighter.state();
        return Line::from(spans);
    }
    if matches!(marker, '`' | '~')
        && count >= 3
        && !(marker == '`' && trimmed[count..].contains('`'))
    {
        let label = trimmed[count..].trim();
        let resolved = syntax_for_label(label);
        let syntax = resolved.unwrap_or_else(|| SYNTAXES.find_syntax_plain_text());
        *fence = Some(CodeFence {
            delimiter: marker,
            length: count,
            state: HighlightLines::new(syntax, &CODE_THEME).state(),
        });
        return Line::styled(
            format!(
                "  ┌─ {label}{}",
                if !label.is_empty() && resolved.is_none() {
                    " · 纯文本"
                } else {
                    ""
                }
            ),
            Style::default().fg(SECONDARY),
        );
    }
    let mut spans = vec![Span::raw("  ")];
    let indent = source.len() - trimmed.len();
    if indent > 0 {
        spans.push(Span::raw(source[..indent].to_owned()));
    }
    let beginning = spans.len();
    let mut style = Style::default();
    let mut stack = Vec::new();
    let mut ordered = None;
    let mut links: Vec<(usize, String)> = Vec::new();
    let recover_cjk_strong = !trimmed.contains(r"\**") && !trimmed.contains(r"\*\*");
    for event in Parser::new_ext(
        trimmed,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    ) {
        match event {
            Event::Start(tag) => {
                stack.push(style);
                match tag {
                    Tag::Heading { .. } => style = style.fg(ACCENT).add_modifier(Modifier::BOLD),
                    Tag::Strong => style = style.add_modifier(Modifier::BOLD),
                    Tag::Emphasis => style = style.add_modifier(Modifier::ITALIC),
                    Tag::Strikethrough => style = style.add_modifier(Modifier::CROSSED_OUT),
                    Tag::BlockQuote(_) => {
                        spans.push(Span::styled("▎ ", Style::default().fg(ACCENT)));
                    }
                    Tag::List(start) => ordered = start,
                    Tag::Item => spans.push(Span::styled(
                        ordered.map_or("• ".into(), |n| format!("{n}. ")),
                        Style::default().fg(ACCENT),
                    )),
                    Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. } => {
                        links.push((spans.len(), dest_url.into_string()));
                        style = style
                            .fg(Color::LightBlue)
                            .add_modifier(Modifier::UNDERLINED);
                    }
                    _ => {}
                }
            }
            Event::End(tag) => {
                style = stack.pop().unwrap_or_default();
                if matches!(
                    tag,
                    pulldown_cmark::TagEnd::Link | pulldown_cmark::TagEnd::Image
                ) && let Some((start, url)) = links.pop()
                {
                    let label: String = spans[start..].iter().map(|s| s.content.as_ref()).collect();
                    if !url.is_empty() && label != url {
                        spans.push(Span::styled(
                            format!(" ({url})"),
                            Style::default().fg(SECONDARY),
                        ));
                    }
                }
            }
            Event::Text(text) => {
                push_text_with_cjk_strong(&mut spans, &text, style, recover_cjk_strong);
            }
            Event::Html(text) | Event::InlineHtml(text) => {
                spans.push(Span::styled(text.into_string(), style))
            }
            Event::Code(text) => {
                spans.push(Span::styled(text.into_string(), style.fg(Color::LightCyan)))
            }
            Event::Rule => spans.push(Span::styled(
                "────────────────────────",
                Style::default().fg(SECONDARY),
            )),
            Event::TaskListMarker(done) => spans.push(Span::styled(
                if done { "☑ " } else { "☐ " },
                style.fg(ACCENT),
            )),
            Event::SoftBreak | Event::HardBreak => {}
            _ => {}
        }
    }
    recover_split_strong(&mut spans, beginning, recover_cjk_strong);
    // Reference definitions and incomplete constructs must never disappear.
    if spans.len() == beginning && !trimmed.is_empty() {
        spans.push(Span::raw(trimmed.to_owned()));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_real_model_labels_aliases_attributes_and_filename_extensions() {
        for label in [
            "csharp",
            "C#",
            "cs",
            "C Sharp",
            "{.csharp}",
            "language-csharp",
            "Program.cs",
            "csharp title=demo",
        ] {
            assert_eq!(syntax_for_label(label).unwrap().name, "C#", "{label}");
        }
        for label in [
            "vb",
            "VB.NET",
            "vbnet",
            "Visual Basic",
            "Visual Basic .NET",
            "Module.vb",
        ] {
            assert_eq!(
                syntax_for_label(label).unwrap().name,
                "Visual Basic .NET",
                "{label}"
            );
        }
        for label in ["typescript", "ts", "tsx", "toml", "Dockerfile", "python3"] {
            assert!(syntax_for_label(label).is_some(), "{label}");
        }
        assert!(syntax_for_label(".NET").is_none());
        assert!(syntax_for_label("invented-language").is_none());
        for (label, sample) in [
            (
                "csharp",
                "public static int Fact(int n) { return 42; } // note",
            ),
            ("vb", "Public Function Fact(n As Integer) As Long ' note"),
        ] {
            let mut fence = None;
            line(&format!("```{label}"), 0, &mut fence);
            let rendered = line(sample, 1, &mut fence);
            let mut colors = Vec::new();
            for span in rendered.spans.iter().skip(1) {
                if !colors.contains(&span.style.fg) {
                    colors.push(span.style.fg);
                }
            }
            assert!(colors.len() >= 3, "{label}: {rendered:?}");
            assert_eq!(rendered.to_string(), format!("  │ {sample}"));
        }
    }

    #[test]
    fn tables_align_unicode_wrap_cells_and_preserve_inline_content() {
        let source = "| 名称 | 值 |\n| :--- | ---: |\n| **中文** | `a\\|b` |\n| 长文本测试长文本 | 123 |\n\n后文";
        let (end, lines) = table(source, 28).unwrap();
        let rendered = lines
            .iter()
            .map(Line::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("中文"), "{rendered}");
        assert!(rendered.contains("a|b"), "{rendered}");
        assert!(!rendered.contains("**") && !rendered.contains('`'));
        assert!(source[end..].contains("后文"));
        assert!(lines.iter().all(|line| line.width() <= 28));
        let (_, narrow) = table(source, 10).unwrap();
        assert!(narrow.iter().any(|l| l.to_string().contains("名称:")));
        assert!(table("normal | text\nnot a separator", 80).is_none());
    }

    #[test]
    fn code_highlighting_keeps_multiline_state_and_unknown_languages_readable() {
        let mut fence = None;
        line("```rust", 0, &mut fence);
        let code = line("let answer = 42; // comment", 1, &mut fence);
        assert_eq!(code.to_string(), "  │ let answer = 42; // comment");
        assert!(
            code.spans
                .windows(2)
                .any(|s| s[0].style.fg != s[1].style.fg)
        );
        line("/* comment", 2, &mut fence);
        let middle = line("still a comment", 3, &mut fence);
        let mut fresh = None;
        line("```rust", 0, &mut fresh);
        assert_ne!(middle, line("still a comment", 1, &mut fresh));
        line("```", 4, &mut fence);
        line("```unknown_language", 5, &mut fence);
        assert_eq!(
            line("中文 **raw**", 6, &mut fence).to_string(),
            "  │ 中文 **raw**"
        );
    }
    fn render(source: &str) -> Line<'static> {
        line(source, 1, &mut None)
    }

    #[test]
    fn formats_inline_styles_without_modifying_code_or_urls() {
        let value = render("**重点**，*斜体*，~~删除~~，`**原样**` [文档](https://example.com)");
        assert_eq!(
            value.to_string(),
            "  重点，斜体，删除，**原样** 文档 (https://example.com)"
        );
        assert!(
            value
                .spans
                .iter()
                .any(|s| s.content == "重点" && s.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(
            value
                .spans
                .iter()
                .any(|s| s.content == "斜体" && s.style.add_modifier.contains(Modifier::ITALIC))
        );
        assert_eq!(
            render(r"\*literal\* and snake_case_name").to_string(),
            "  *literal* and snake_case_name"
        );
        assert!(
            render("[1]: https://example.com")
                .to_string()
                .contains("https://example.com")
        );
    }

    #[test]
    fn recovers_model_style_strong_markers_next_to_cjk_text() {
        let value =
            render(r#"4. **图一的技术质量明显"更认真"**：但这是一张**"我拍的我的鸟"**的记录照。"#);
        assert!(!value.to_string().contains("**"), "{value:?}");
        assert!(value.spans.iter().any(|span| {
            span.content == "\"我拍的我的鸟\"" && span.style.add_modifier.contains(Modifier::BOLD)
        }));
        let escaped = render(r"保留 \*\*字面量\*\*");
        assert_eq!(escaped.to_string(), "  保留 **字面量**");
        assert!(
            escaped
                .spans
                .iter()
                .all(|span| !span.style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn formats_blocks_and_preserves_indentation() {
        assert_eq!(line("普通段落", 0, &mut None).to_string(), "  普通段落");
        assert_eq!(render("## 标题").to_string(), "  标题");
        assert_eq!(render("  - **项目**").to_string(), "    • 项目");
        assert_eq!(render("3. 第三项").to_string(), "  3. 第三项");
        assert_eq!(render("> 引用").to_string(), "  ▎ 引用");
        assert!(render("- [x] 完成").to_string().contains("☑ 完成"));
        assert!(render("#hashtag").to_string().contains("#hashtag"));
    }

    #[test]
    fn code_fences_require_matching_delimiter_and_length() {
        let mut fence = None;
        assert_eq!(line("````rust", 0, &mut fence).to_string(), "  ┌─ rust");
        assert_eq!(line("```", 1, &mut fence).to_string(), "  │ ```");
        assert_eq!(line("**code**", 2, &mut fence).to_string(), "  │ **code**");
        line("````", 3, &mut fence);
        assert_eq!(fence, None);
        line("~~~python", 4, &mut fence);
        assert_eq!(
            fence.as_ref().map(|f| (f.delimiter, f.length)),
            Some(('~', 3))
        );
        line("~~~~", 5, &mut fence);
        assert_eq!(fence, None);
    }
}
