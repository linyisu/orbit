//! Render complete source lines incrementally; never reparse the conversation on scroll.
//! CommonMark inline syntax is handled by pulldown-cmark, not regex replacements.
use pulldown_cmark::{Event, Options, Parser, Tag};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

pub(super) type Fence = Option<(char, usize)>;
const ACCENT: Color = Color::Rgb(218, 139, 109);
const SECONDARY: Color = Color::Rgb(170, 170, 170);

pub(super) fn line(source: &str, index: usize, fence: &mut Fence) -> Line<'static> {
    let trimmed = source.trim_start();
    let marker = trimmed.chars().next().unwrap_or(' ');
    let count = trimmed.chars().take_while(|c| *c == marker).count();
    if let Some((delimiter, length)) = *fence {
        if marker == delimiter && count >= length && trimmed[count..].trim().is_empty() {
            *fence = None;
            return Line::styled("  └─", Style::default().fg(SECONDARY));
        }
        return Line::from(vec![
            Span::styled("  │ ", Style::default().fg(SECONDARY)),
            Span::raw(source.to_owned()),
        ]);
    }
    if matches!(marker, '`' | '~')
        && count >= 3
        && !(marker == '`' && trimmed[count..].contains('`'))
    {
        *fence = Some((marker, count));
        return Line::styled(
            format!("  ┌─ {}", trimmed[count..].trim()),
            Style::default().fg(SECONDARY),
        );
    }
    let mut spans = vec![Span::styled(
        if index == 0 { "● " } else { "  " },
        Style::default().fg(ACCENT),
    )];
    let indent = source.len() - trimmed.len();
    if indent > 0 {
        spans.push(Span::raw(source[..indent].to_owned()));
    }
    let beginning = spans.len();
    let mut style = Style::default();
    let mut stack = Vec::new();
    let mut ordered = None;
    let mut links: Vec<(usize, String)> = Vec::new();
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
                        spans.push(Span::styled("│ ", Style::default().fg(ACCENT)))
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
            Event::Text(text) | Event::Html(text) | Event::InlineHtml(text) => {
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
    // Reference definitions and incomplete constructs must never disappear.
    if spans.len() == beginning && !trimmed.is_empty() {
        spans.push(Span::raw(trimmed.to_owned()));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn formats_blocks_and_preserves_indentation() {
        assert_eq!(render("## 标题").to_string(), "  标题");
        assert_eq!(render("  - **项目**").to_string(), "    • 项目");
        assert_eq!(render("3. 第三项").to_string(), "  3. 第三项");
        assert_eq!(render("> 引用").to_string(), "  │ 引用");
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
        assert_eq!(fence, Some(('~', 3)));
        line("~~~~", 5, &mut fence);
        assert_eq!(fence, None);
    }
}
