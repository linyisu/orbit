//! Thinking headers and the outer rail around shared Markdown content.
#[cfg(test)]
use crate::tui::markdown::Fence;
use ratatui::{
    style::Style,
    text::{Line, Span},
};

use super::THINKING_MUTED;
use crate::tui::{
    app::ThinkingRow,
    markdown::{self, LinkedLine},
    theme::{THEME, desaturate_for_thinking},
};

#[cfg(test)]
pub(in crate::tui) fn assistant_line(line: &str, index: usize, code: &mut Fence) -> Line<'static> {
    markdown::line(line, index, code)
}

pub(in crate::tui) fn thinking_card_at_width(
    row: &ThinkingRow,
    expanded: bool,
    width: u16,
) -> Vec<LinkedLine> {
    let mut lines = Vec::new();
    if row.finished {
        let label = row.elapsed.map_or_else(
            || "思考".to_owned(),
            |elapsed| {
                if elapsed.as_secs() == 0 {
                    format!("思考 {}ms", elapsed.as_millis())
                } else {
                    format!("思考 {:.1}s", elapsed.as_secs_f64())
                }
            },
        );

        lines.push(LinkedLine::plain(Line::from(vec![
            Span::raw(if expanded { "  ⌄ " } else { "  › " }),
            Span::styled(label, Style::default().fg(THINKING_MUTED)),
        ])));
        if !expanded {
            lines.push(LinkedLine::plain(Line::from("")));
            return lines;
        }
    }

    if !row.finished {
        let mut header = thinking_header();
        header.line.spans[0] = Span::raw(if expanded { "  ⌄ " } else { "  › " });
        lines.push(header);
        if !expanded {
            return lines;
        }
    }

    let text = row.content.trim_end();
    lines.extend(
        markdown::document(text, width.saturating_sub(2))
            .into_iter()
            .map(thinking_linked),
    );
    if !text.is_empty() {
        lines.push(LinkedLine::plain(Line::from("")));
    }
    lines
}

pub(in crate::tui) fn thinking_header() -> LinkedLine {
    LinkedLine {
        chrome: true,
        ..LinkedLine::plain(Line::from(vec![
            Span::raw("  ⌄ "),
            Span::styled("思考", Style::default().fg(THINKING_MUTED)),
        ]))
    }
}

#[cfg(test)]
pub(in crate::tui) fn thinking_line(line: &str, index: usize, code: &mut Fence) -> Line<'static> {
    thinking_linked_line(line, index, code).line
}

#[cfg(test)]
pub(in crate::tui) fn thinking_linked_line(
    line: &str,
    index: usize,
    code: &mut Fence,
) -> LinkedLine {
    thinking_linked(markdown::linked_line(line, index, code))
}

pub(in crate::tui) fn thinking_linked(mut linked: LinkedLine) -> LinkedLine {
    if let Some(table) = &mut linked.table {
        table.thinking = true;
    }
    let rendered = &mut linked.line;
    let style = rendered.style;
    rendered.style = Style::default();
    for span in &mut rendered.spans {
        span.style = style.patch(span.style);
        span.style = span
            .style
            .fg(desaturate_for_thinking(span.style.fg));
        if let Some(background) = span.style.bg {
            span.style = span
                .style
                .bg(desaturate_for_thinking(Some(background)));
        }
    }

    // Reserve the outer rail separately so code rails keep their own color.
    linked.strip_body_margin();
    linked.framed(
        vec![Span::styled(
            "  ┊ ",
            Style::default().fg(THEME.thinking_rail),
        )],
        Vec::new(),
    )
}
