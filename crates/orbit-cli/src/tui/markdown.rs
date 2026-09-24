//! Render model prose from CommonMark events while keeping code blocks literal.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

const ACCENT: Color = Color::Rgb(218, 139, 109);
const DIM: Color = Color::Rgb(155, 153, 157);
const CODE: Color = Color::Rgb(174, 211, 217);

struct Builder {
    lines: Vec<Line<'static>>,
    current: Vec<Span<'static>>,
    style: Style,
    styles: Vec<Style>,
    lists: Vec<Option<u64>>,
    quote_depth: usize,
    item_marker: Option<usize>,
    table_cell: usize,
    code_language: Option<String>,
    code_text: String,
    accent: Color,
    code_color: Color,
}

impl Builder {
    fn new(thinking: bool) -> Self {
        Self {
            lines: Vec::new(),
            current: Vec::new(),
            style: if thinking {
                Style::default().fg(DIM)
            } else {
                Style::default()
            },
            styles: Vec::new(),
            lists: Vec::new(),
            quote_depth: 0,
            item_marker: None,
            table_cell: 0,
            code_language: None,
            code_text: String::new(),
            accent: if thinking { DIM } else { ACCENT },
            code_color: if thinking { DIM } else { CODE },
        }
    }

    fn push(&mut self, text: impl Into<String>, style: Style) {
        self.current.push(Span::styled(text.into(), style));
    }

    fn text(&mut self, text: &str) {
        let mut parts = text.split('\n').peekable();
        while let Some(part) = parts.next() {
            if self.current.is_empty() && self.quote_depth > 0 {
                self.push("│ ".repeat(self.quote_depth), Style::default().fg(DIM));
            }
            if !part.is_empty() {
                self.marked_text(part);
            }
            if parts.peek().is_some() {
                self.flush();
            }
        }
    }

    fn marked_text(&mut self, text: &str) {
        let mut rest = text;
        while let Some(open) = rest.find("==") {
            let after_open = &rest[open + 2..];
            let Some(close) = after_open.find("==") else {
                break;
            };
            if open > 0 {
                self.push(&rest[..open], self.style);
            }
            self.push(
                &after_open[..close],
                self.style
                    .fg(self.accent)
                    .add_modifier(Modifier::UNDERLINED),
            );
            rest = &after_open[close + 2..];
        }
        if !rest.is_empty() {
            self.push(rest, self.style);
        }
    }

    fn flush(&mut self) {
        if !self.current.is_empty() {
            self.lines
                .push(Line::from(std::mem::take(&mut self.current)));
        }
        self.item_marker = None;
    }

    fn space(&mut self) {
        self.flush();
        if !self.lines.is_empty() && !self.lines.last().is_some_and(|line| line.spans.is_empty()) {
            self.lines.push(Line::default());
        }
    }

    fn nested_style(&mut self, style: Style) {
        self.styles.push(self.style);
        self.style = style;
    }

    fn restore_style(&mut self) {
        if let Some(style) = self.styles.pop() {
            self.style = style;
        }
    }

    fn start_item(&mut self) {
        self.flush();
        if self.quote_depth > 0 {
            self.push("│ ".repeat(self.quote_depth), Style::default().fg(DIM));
        }
        self.push(
            "  ".repeat(self.lists.len().saturating_sub(1)),
            Style::default(),
        );
        let marker = match self.lists.last_mut() {
            Some(Some(next)) => {
                let marker = format!("{next}. ");
                *next += 1;
                marker
            }
            _ => "• ".to_owned(),
        };
        self.item_marker = Some(self.current.len());
        self.push(marker, Style::default().fg(self.accent));
    }

    fn finish_code(&mut self) {
        let code = std::mem::take(&mut self.code_text);
        for source in code.split_terminator('\n') {
            self.push("│ ", Style::default().fg(DIM));
            self.push(
                source.trim_end_matches('\r').replace('\t', "    "),
                Style::default().fg(self.code_color),
            );
            self.flush();
        }
        self.push("└─", Style::default().fg(DIM));
        self.flush();
        self.code_language = None;
        self.space();
    }
}

pub fn render(text: &str, width: usize, thinking: bool) -> Vec<Line<'static>> {
    let options =
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS | Options::ENABLE_TABLES;
    let mut builder = Builder::new(thinking);
    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                builder.flush();
                builder.nested_style(
                    builder
                        .style
                        .fg(builder.accent)
                        .add_modifier(Modifier::BOLD),
                );
                let level = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    HeadingLevel::H3 => 3,
                    HeadingLevel::H4 => 4,
                    HeadingLevel::H5 => 5,
                    HeadingLevel::H6 => 6,
                };
                builder.push(format!("{} ", "#".repeat(level)), Style::default().fg(DIM));
            }
            Event::End(TagEnd::Heading(_)) => {
                builder.flush();
                builder.restore_style();
                builder.space();
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                builder.flush();
                let language = match kind {
                    CodeBlockKind::Fenced(label) => {
                        label.split_whitespace().next().unwrap_or("").to_owned()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                builder.push(format!("┌─ {language}"), Style::default().fg(DIM));
                builder.flush();
                builder.code_language = Some(language);
            }
            Event::End(TagEnd::CodeBlock) => builder.finish_code(),
            Event::Start(Tag::List(start)) => {
                builder.flush();
                builder.lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                builder.flush();
                builder.lists.pop();
                if builder.lists.is_empty() {
                    builder.space();
                }
            }
            Event::Start(Tag::Item) => builder.start_item(),
            Event::End(TagEnd::Item | TagEnd::Paragraph | TagEnd::TableRow | TagEnd::TableHead) => {
                builder.flush();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                builder.flush();
                builder.quote_depth += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                builder.flush();
                builder.quote_depth = builder.quote_depth.saturating_sub(1);
            }
            Event::Start(Tag::TableRow | Tag::TableHead) => {
                builder.flush();
                builder.table_cell = 0;
            }
            Event::Start(Tag::TableCell) => {
                if builder.table_cell > 0 {
                    builder.push(" │ ", Style::default().fg(DIM));
                }
                builder.table_cell += 1;
            }
            Event::Start(Tag::Strong) => {
                builder.nested_style(builder.style.add_modifier(Modifier::BOLD));
            }
            Event::Start(Tag::Emphasis) => {
                builder.nested_style(builder.style.add_modifier(Modifier::ITALIC));
            }
            Event::Start(Tag::Strikethrough) => {
                builder.nested_style(builder.style.add_modifier(Modifier::CROSSED_OUT));
            }
            Event::Start(Tag::Link { .. }) => {
                builder.nested_style(
                    builder
                        .style
                        .fg(builder.code_color)
                        .add_modifier(Modifier::UNDERLINED),
                );
            }
            Event::End(
                TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link,
            ) => {
                builder.restore_style();
            }
            Event::Text(value) if builder.code_language.is_some() => {
                builder.code_text.push_str(&value);
            }
            Event::Text(value)
            | Event::Html(value)
            | Event::InlineHtml(value)
            | Event::InlineMath(value)
            | Event::DisplayMath(value)
            | Event::FootnoteReference(value) => builder.text(&value),
            Event::Code(value) => {
                builder.push(value.to_string(), builder.style.fg(builder.code_color))
            }
            Event::TaskListMarker(checked) => {
                if let Some(index) = builder.item_marker
                    && let Some(marker) = builder.current.get_mut(index)
                {
                    marker.content = if checked { "[x] " } else { "[ ] " }.into();
                }
            }
            Event::SoftBreak => builder.text(" "),
            Event::HardBreak => builder.flush(),
            Event::Rule => {
                builder.flush();
                builder.push("────", Style::default().fg(DIM));
                builder.flush();
            }
            _ => {}
        }
    }
    builder.flush();
    while builder
        .lines
        .last()
        .is_some_and(|line| line.spans.is_empty())
    {
        builder.lines.pop();
    }
    if builder.lines.is_empty() {
        builder.lines.push(Line::default());
    }
    builder
        .lines
        .into_iter()
        .flat_map(|line| wrap_line(line, width.max(1)))
        .collect()
}

fn wrap_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    if line.spans.is_empty() {
        return vec![line];
    }
    let gutter = line.spans.first().and_then(|span| {
        let text = span.content.as_ref();
        if text.contains('│') {
            Some(span.clone())
        } else if line
            .spans
            .iter()
            .take(3)
            .any(|span| matches!(span.content.as_ref(), "• " | "[x] " | "[ ] "))
        {
            let width = line
                .spans
                .iter()
                .take_while(|span| {
                    span.content.chars().all(char::is_whitespace)
                        || matches!(span.content.as_ref(), "• " | "[x] " | "[ ] ")
                })
                .map(Span::width)
                .sum();
            Some(Span::raw(" ".repeat(width)))
        } else if text.chars().all(char::is_whitespace) {
            Some(Span::raw(" ".repeat(span.width())))
        } else {
            None
        }
    });
    let mut rows = Vec::new();
    let mut current = Vec::<Span<'static>>::new();
    let mut used = 0;
    for span in line.spans {
        for grapheme in span.content.graphemes(true) {
            let size = grapheme.width().max(1);
            if used + size > width && used > 0 {
                rows.push(Line::from(std::mem::take(&mut current)));
                used = 0;
                if let Some(prefix) = &gutter
                    && prefix.width() + size <= width
                {
                    current.push(prefix.clone());
                    used = prefix.width();
                }
            }
            if let Some(last) = current.last_mut()
                && last.style == span.style
            {
                last.content.to_mut().push_str(grapheme);
            } else {
                current.push(Span::styled(grapheme.to_owned(), span.style));
            }
            used += size;
        }
    }
    rows.push(Line::from(current));
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_tasks_inline_code_and_tables_without_markdown_delimiters() {
        let lines = render(
            "# Tasks\n- [x] **done** with `code` and ==mark==\n\n| A | B |\n|---|---|\n| 1 | 2 |",
            40,
            false,
        );
        let text = plain(&lines);

        assert!(text.contains("[x] done with code and mark"), "{text}");
        assert!(text.contains("A │ B"), "{text}");
        assert!(lines.iter().all(|line| line.width() <= 40));
        assert!(lines.iter().flat_map(|line| &line.spans).any(|span| {
            span.content == "mark" && span.style.add_modifier.contains(Modifier::UNDERLINED)
        }));
    }

    #[test]
    fn outer_code_fence_keeps_nested_markdown_literal_and_wraps_to_width() {
        let source = "````markdown\n# literal heading\n```rust\n==raw==\nfn main() {}\n```\n````";
        let lines = render(source, 24, true);
        let text = plain(&lines);

        assert!(text.contains("┌─ markdown"), "{text}");
        assert!(text.contains("# literal heading"), "{text}");
        assert!(text.contains("```rust"), "{text}");
        assert!(text.contains("==raw=="), "{text}");
        assert!(lines.iter().all(|line| line.width() <= 24));
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.content.contains("==raw==") && span.style.fg == Some(DIM))
        );
    }
}
