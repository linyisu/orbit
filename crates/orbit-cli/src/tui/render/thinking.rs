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
                if elapsed.as_secs_f64() < 0.1 {
                    "思考 <0.1s".into()
                } else {
                    format!("思考 {:.1}s", elapsed.as_secs_f64())
                }
            },
        );
        lines.push(LinkedLine::plain(Line::from(vec![
            Span::raw("  "),
            Span::styled(label, Style::default().fg(THINKING_MUTED)),
        ])));
        if !expanded {
            lines.push(LinkedLine::plain(Line::from("")));
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
    let rendered = &mut linked.line;
    let style = rendered.style;
    rendered.style = Style::default();
    for span in &mut rendered.spans {
        span.style = style.patch(span.style);
        span.style = span.style.fg(desaturate_for_thinking(span.style.fg));
        if let Some(background) = span.style.bg {
            span.style = span.style.bg(desaturate_for_thinking(Some(background)));
        }
    }

    // Reserve the outer rail separately so code rails keep their own color.
    if let Some(first) = rendered.spans.first_mut()
        && let Some(rest) = first.content.strip_prefix("  ")
    {
        if rest.is_empty() {
            rendered.spans.remove(0);
        } else {
            first.content = rest.to_owned().into();
        }
        linked.shift_offsets(-2);
    }
    linked.line.spans.insert(
        0,
        Span::styled("  ┊ ", Style::default().fg(THEME.thinking_rail)),
    );
    linked.shift_offsets("  ┊ ".len() as isize);
    linked.decorate(&[0])
}
