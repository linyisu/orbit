//! Cache Unicode-aware physical-line layout, and hand only visible lines to
//! Ratatui. Scrolling never asks a Paragraph to wrap and skip the entire
//! conversation from its beginning.
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use std::{collections::HashSet, ops::Range};
use unicode_linebreak::linebreaks;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{app::TranscriptItem, render};

#[derive(Debug)]
struct Row {
    line: Line<'static>,
    wrapped: Vec<Line<'static>>,
    ranges: Vec<Range<usize>>,
    links: Vec<super::markdown::LinkTarget>,
    gutter: Option<Gutter>,
    start: usize,
    height: usize,
}

#[derive(Debug)]
struct Gutter {
    first: Line<'static>,
    continuation: Line<'static>,
    width: u16,
}

fn list_marker(text: &str) -> bool {
    matches!(text, "• " | "☑ " | "☐ ")
        || text
            .strip_suffix(". ")
            .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

fn is_rail_gutter(content: &str) -> bool {
    if let Some(prefix) = content
        .strip_suffix("┊ │ ")
        .or_else(|| content.strip_suffix("│ "))
        .or_else(|| content.strip_suffix("┊ "))
    {
        prefix.chars().all(|c| c == ' ')
    } else {
        false
    }
}

fn take_gutter(line: &mut Line<'static>) -> Option<Gutter> {
    if line
        .spans
        .first()
        .is_some_and(|span| is_rail_gutter(span.content.as_ref()))
    {
        let span = line.spans.remove(0);
        let width = span.width() as u16;
        let gutter = Line::from(span);
        return Some(Gutter {
            first: gutter.clone(),
            continuation: gutter,
            width,
        });
    }

    let mut quote_end = None;

    for (index, span) in line.spans.iter().enumerate() {
        if span.content == "▎ " {
            quote_end = Some(index);
        } else if !span.content.chars().all(char::is_whitespace) {
            break;
        }
    }

    if let Some(end) = quote_end {
        let prefix = line.spans.drain(..=end).collect::<Vec<_>>();
        let width = prefix.iter().map(Span::width).sum::<usize>() as u16;
        let gutter = Line::from(prefix);
        return Some(Gutter {
            first: gutter.clone(),
            continuation: gutter,
            width,
        });
    }

    let marker = line
        .spans
        .iter()
        .enumerate()
        .find(|(index, span)| {
            list_marker(&span.content)
                && line.spans[..*index]
                    .iter()
                    .all(|prefix| prefix.content.chars().all(char::is_whitespace))
        })
        .map(|(index, _)| index);
    let end = marker.or_else(|| {
        line.spans
            .iter()
            .take_while(|span| {
                !span.content.is_empty() && span.content.chars().all(char::is_whitespace)
            })
            .count()
            .checked_sub(1)
    })?;
    let prefix = line.spans.drain(..=end).collect::<Vec<_>>();
    let width = prefix.iter().map(Span::width).sum::<usize>() as u16;

    Some(Gutter {
        first: Line::from(prefix),
        continuation: Line::from(" ".repeat(width as usize)),
        width,
    })
}

fn slice_line(line: &Line<'static>, range: Range<usize>) -> Line<'static> {
    let mut spans = Vec::new();
    let mut offset = 0;

    for span in &line.spans {
        let start = offset;
        let end = start + span.content.len();
        offset = end;
        let from = range.start.max(start);
        let to = range.end.min(end);

        if from < to {
            spans.push(Span::styled(
                span.content[from - start..to - start].to_owned(),
                span.style,
            ));
        }
    }

    Line {
        style: line.style,
        alignment: line.alignment,
        spans,
    }
}

fn wrap_ranges(text: &str, width: u16) -> Vec<Range<usize>> {
    if text.is_empty() || width == 0 {
        return std::iter::once_with(|| 0..0).collect();
    }

    let opportunities = linebreaks(text)
        .map(|(index, _)| index)
        .collect::<HashSet<_>>();
    let mut ranges = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let mut used = 0;
        let mut hard_end = start;
        let mut allowed = None;
        let mut overflowed = false;

        for (relative, grapheme) in text[start..].grapheme_indices(true) {
            let end = start + relative + grapheme.len();
            let size = grapheme.width();

            if used + size > usize::from(width) && hard_end > start {
                overflowed = true;
                break;
            }

            used += size;
            hard_end = end;

            if opportunities.contains(&end) {
                allowed = Some(end);
            }
        }

        if !overflowed || hard_end == text.len() {
            ranges.push(start..text.len());
            break;
        }

        let end = allowed.filter(|end| *end > start).unwrap_or(hard_end);
        ranges.push(start..end);
        start = end;
    }

    ranges
}

pub(super) fn wrap_line(
    line: &Line<'static>,
    width: u16,
) -> (Vec<Line<'static>>, Vec<Range<usize>>) {
    let text = line.to_string();
    let ranges = wrap_ranges(&text, width);
    let lines = ranges
        .iter()
        .cloned()
        .map(|range| slice_line(line, range))
        .collect();

    (lines, ranges)
}

fn byte_at_column(text: &str, range: &Range<usize>, column: u16) -> Option<usize> {
    let mut used = 0;

    for (relative, grapheme) in text.get(range.clone())?.grapheme_indices(true) {
        let width = grapheme.width() as u16;

        if column < used + width {
            return Some(range.start + relative);
        }

        used += width;
    }

    None
}

#[derive(Debug, Default)]
struct Entry {
    rows: Vec<Row>,
    disclosures: Vec<usize>,
    height: usize,
    end: usize,
    // Completed source lines never change during streaming. Only reformat the
    // unfinished final line and newly appended lines, including split fences.
    stable_rows: usize,
    parsed_bytes: usize,
    logical_index: usize,
    code: super::markdown::Fence,
    observed_bytes: Option<usize>,
}

impl Entry {
    fn push(&mut self, mut line: Line<'static>, width: u16) {
        render::sanitize_line(&mut line);
        let links = super::markdown::plain_links(&line.to_string());
        self.push_with_links(line, width, links);
    }

    fn push_with_links(
        &mut self,
        mut line: Line<'static>,
        width: u16,
        mut links: Vec<super::markdown::LinkTarget>,
    ) {
        // Markdown rules occupy the entire transcript row, including the usual gutter.
        let mut visible = line
            .spans
            .iter()
            .filter(|span| !span.content.trim().is_empty());
        if let Some(rule) = visible.next()
            && visible.next().is_none()
            && rule.style.fg == Some(super::theme::THEME.md_rule)
            && rule.content.chars().all(|ch| ch == '─')
        {
            line = Line::styled("─".repeat(usize::from(width)), rule.style);
        }
        render::sanitize_line(&mut line);
        let original = line.to_string();
        let gutter = take_gutter(&mut line);
        let removed = original.len().saturating_sub(line.to_string().len());
        links.retain_mut(|link| {
            if link.end <= removed {
                return false;
            }

            link.start = link.start.saturating_sub(removed);
            link.end = link.end.saturating_sub(removed);

            true
        });
        let content_width = width.saturating_sub(gutter.as_ref().map_or(0, |gutter| gutter.width));
        let (wrapped, ranges) = wrap_line(&line, content_width);
        let height = wrapped.len();
        self.rows.push(Row {
            gutter,
            line,
            wrapped,
            ranges,
            links,
            start: self.height,
            height,
        });
        self.height += height;
    }

    fn stream(&mut self, text: &str, thinking: bool, width: u16) {
        if self.observed_bytes == Some(text.len()) {
            return;
        }

        if self.observed_bytes.is_some_and(|len| text.len() < len) {
            *self = Self::default();
        }

        self.rows.truncate(self.stable_rows);
        self.height = self.rows.last().map_or(0, |row| row.start + row.height);
        let mut code = self.code.clone();
        let mut index = self.logical_index;
        let mut consumed = self.parsed_bytes;

        while consumed < text.len() {
            if !thinking
                && code.is_none()
                && let Some((bytes, lines)) = super::markdown::table(&text[consumed..], width)
            {
                for line in lines {
                    self.push_with_links(line.line, width, line.links);
                }

                index += text[consumed..consumed + bytes].lines().count();
                consumed += bytes;

                // A partial next row (even just "| ") can temporarily end the parser's
                // table. Do not freeze it until a complete terminating line arrives.
                if text[consumed..]
                    .split_inclusive('\n')
                    .next()
                    .is_some_and(|line| line.ends_with('\n') && !line.trim_start().starts_with('|'))
                {
                    self.stable_rows = self.rows.len();
                    self.parsed_bytes = consumed;
                    self.logical_index = index;
                    self.code = code.clone();
                }

                continue;
            }

            let part = text[consumed..].split_inclusive('\n').next().unwrap();
            let complete = part.ends_with('\n');
            let line = if complete {
                part.trim_end_matches('\n')
                    .strip_suffix('\r')
                    .unwrap_or(part.trim_end_matches('\n'))
            } else {
                part
            };

            let was_code = code.is_some();
            let mut rendered = if thinking {
                render::thinking_line(line, index, &mut code)
            } else {
                render::assistant_line(line, index, &mut code)
            };
            render::sanitize_line(&mut rendered);

            let links = if !was_code {
                super::markdown::links(line, &rendered)
            } else {
                Default::default()
            };

            self.push_with_links(rendered, width, links);
            index += 1;
            consumed += part.len();

            if complete && (thinking || code.is_some() || !line.contains('|')) {
                self.stable_rows = self.rows.len();
                self.parsed_bytes = consumed;
                self.logical_index = index;
                self.code = code.clone();
            }
        }

        // Every completed-looking streamed section owns exactly one trailing
        // blank row. This keeps reasoning distinct from the following tool or
        // final answer without relying on the next transcript entry to know
        // what came before it.
        if !text.is_empty() {
            self.push(Line::from(""), width);
        }

        self.observed_bytes = Some(text.len());
    }

    fn user(&mut self, text: &str, width: u16) {
        self.framed_input(text, width, "❯");
    }

    fn framed_input(&mut self, text: &str, width: u16, icon: &str) {
        // A complete frame lets user turns remain visually distinct after their
        // contents wrap. Keep a plain fallback for terminals too narrow to
        // draw the two side rails without stealing all useful text width.
        if width < 9 {
            for (index, line) in text.lines().enumerate() {
                self.push(
                    Line::from(vec![
                        Span::styled(
                            if index == 0 {
                                format!("{icon} ")
                            } else {
                                "  ".into()
                            },
                            ratatui::style::Style::default().fg(crate::tui::theme::THEME.accent),
                        ),
                        Span::styled(
                            line.to_owned(),
                            ratatui::style::Style::default()
                                .add_modifier(ratatui::style::Modifier::BOLD),
                        ),
                    ]),
                    width,
                );
            }

            self.push(Line::from(""), width);
            return;
        }

        let border = ratatui::style::Style::default().fg(crate::tui::theme::THEME.accent);
        let top_label = format!("╭─{icon}─");
        let trailing = "─".repeat(usize::from(width) - top_label.width() - 1);
        self.push(
            Line::from(vec![
                Span::styled("╭─", border),
                Span::styled(
                    icon.to_owned(),
                    border.add_modifier(ratatui::style::Modifier::BOLD),
                ),
                Span::styled(format!("─{trailing}╮"), border),
            ]),
            width,
        );
        let inner_width = width - 4;

        for source in text.split('\n') {
            let content = super::markdown::LinkedLine::plain(Line::from(Span::styled(
                source.to_owned(),
                ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::BOLD),
            )));

            for line in content.wrap(inner_width) {
                let padding = usize::from(inner_width).saturating_sub(line.line.width());
                let mut spans = vec![Span::styled("│ ", border)];
                let offset = spans[0].content.len();
                let links = line
                    .links
                    .into_iter()
                    .map(|mut link| {
                        link.start += offset;
                        link.end += offset;
                        link
                    })
                    .collect();
                spans.extend(line.line.spans);
                spans.push(Span::raw(" ".repeat(padding)));
                spans.push(Span::styled(" │", border));
                self.push_with_links(Line::from(spans), width, links);
            }
        }

        self.push(
            Line::from(Span::styled(
                format!("╰{}╯", "─".repeat(usize::from(width) - 2)),
                border,
            )),
            width,
        );
        self.push(Line::from(""), width);
    }
}

#[derive(Debug, Default)]
pub(super) struct TranscriptLayout {
    entries: Vec<Entry>,
    width: u16,
    expanded: std::collections::BTreeSet<usize>,
    dirty_from: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct TranscriptAnchor {
    entry: usize,
    row: usize,
    byte: usize,
}

impl TranscriptLayout {
    pub(super) fn width(&self) -> u16 {
        self.width
    }

    pub(super) fn anchor_at(&self, offset: usize) -> Option<TranscriptAnchor> {
        let entry_index = self.entries.partition_point(|entry| entry.end <= offset);
        let entry = self.entries.get(entry_index)?;
        let entry_start = entry.end.saturating_sub(entry.height);
        let relative = offset.saturating_sub(entry_start);
        let row_index = entry
            .rows
            .partition_point(|row| row.start + row.height <= relative)
            .min(entry.rows.len().saturating_sub(1));
        let row = entry.rows.get(row_index)?;
        let wrapped = relative
            .saturating_sub(row.start)
            .min(row.ranges.len().saturating_sub(1));

        Some(TranscriptAnchor {
            entry: entry_index,
            row: row_index,
            byte: row.ranges.get(wrapped)?.start,
        })
    }

    pub(super) fn offset_for_anchor(&self, anchor: TranscriptAnchor) -> Option<usize> {
        let entry = self.entries.get(anchor.entry)?;
        let row = entry.rows.get(anchor.row)?;
        let wrapped = row
            .ranges
            .partition_point(|range| range.start <= anchor.byte)
            .saturating_sub(1)
            .min(row.ranges.len().saturating_sub(1));

        Some(entry.end.saturating_sub(entry.height) + row.start + wrapped)
    }

    pub fn invalidate_from(&mut self, index: usize) {
        self.dirty_from = self.dirty_from.min(index);
    }

    #[cfg(test)]
    pub fn update(&mut self, items: &[TranscriptItem], expanded: Option<usize>, width: u16) {
        self.update_expanded(items, &expanded.into_iter().collect(), width);
    }

    pub fn update_expanded(
        &mut self,
        items: &[TranscriptItem],
        expanded: &std::collections::BTreeSet<usize>,
        width: u16,
    ) {
        if self.width != width || items.len() < self.entries.len() {
            self.entries.clear();
            self.dirty_from = 0;
            self.width = width;
        }

        if &self.expanded != expanded {
            if let Some(index) = self.expanded.symmetric_difference(expanded).next().copied() {
                self.invalidate_from(index);
            }
            self.expanded = expanded.clone();
        }

        if width == 0 {
            return;
        }

        let first = self.dirty_from.min(self.entries.len());
        self.entries.resize_with(items.len(), Entry::default);
        let mut end = first.checked_sub(1).map_or(0, |i| self.entries[i].end);

        for (relative, (entry, item)) in self.entries[first..]
            .iter_mut()
            .zip(&items[first..])
            .enumerate()
        {
            match item {
                TranscriptItem::Assistant(text) => entry.stream(text, false, width),
                TranscriptItem::Thinking(row) if !row.finished => {
                    entry.stream(row.content.trim_end(), true, width)
                }
                TranscriptItem::User(text) => {
                    *entry = Entry::default();
                    entry.user(text, width);
                }
                TranscriptItem::Tool(row) => {
                    *entry = Entry::default();
                    let item_index = first + relative;
                    let is_expanded = expanded.contains(&item_index);
                    let next_is_tool =
                        matches!(items.get(item_index + 1), Some(TranscriptItem::Tool(_)));

                    for line in render::tool_card_with_links(row, is_expanded, width) {
                        if !line.line.to_string().is_empty() {
                            entry.disclosures.push(entry.rows.len());
                        }
                        entry.push_with_links(line.line, width, line.links);
                    }

                    if !next_is_tool {
                        entry.push(Line::from(""), width);
                    }
                }
                _ => {
                    *entry = Entry::default();
                    let collapsible = matches!(item, TranscriptItem::Thinking(_));
                    let toggle = collapsible.then(|| {
                        super::tool_display::toggle_line(
                            expanded.contains(&(first + relative)),
                            width,
                        )
                    });

                    for line in render::transcript_lines_at_width(
                        std::slice::from_ref(item),
                        expanded.contains(&(first + relative)),
                        width,
                    ) {
                        if (collapsible && entry.rows.is_empty()) || toggle.as_ref() == Some(&line)
                        {
                            entry.disclosures.push(entry.rows.len());
                        }

                        entry.push(line, width);
                    }
                }
            }

            end += entry.height;
            entry.end = end;
        }

        self.dirty_from = items.len();
    }

    pub fn visible_collapsible(
        &self,
        items: &[TranscriptItem],
        offset: usize,
        height: usize,
    ) -> Option<usize> {
        self.entries
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, entry)| {
                (entry.end > offset
                    && entry.end - entry.height < offset + height
                    && (matches!(items.get(index), Some(TranscriptItem::Tool(_)))
                        || matches!(items.get(index), Some(TranscriptItem::Thinking(row)) if row.finished)))
                .then_some(index)
            })
    }

    pub(super) fn item_offset(&self, index: usize) -> Option<usize> {
        self.entries
            .get(index)
            .map(|entry| entry.end - entry.height)
    }

    pub(super) fn disclosure_at(
        &self,
        items: &[TranscriptItem],
        area: Rect,
        offset: usize,
        point: (u16, u16),
    ) -> Option<usize> {
        if !area.contains(point.into()) {
            return None;
        }

        let absolute = offset + usize::from(point.1 - area.y);
        let index = self.entries.partition_point(|entry| entry.end <= absolute);
        let entry = self.entries.get(index)?;

        if !matches!(
            items.get(index),
            Some(TranscriptItem::Tool(_))
                | Some(TranscriptItem::Thinking(super::app::ThinkingRow {
                    finished: true,
                    ..
                }))
        ) {
            return None;
        }

        let relative = absolute - (entry.end - entry.height);

        let row = entry
            .disclosures
            .iter()
            .filter_map(|i| entry.rows.get(*i))
            .find(|row| row.start <= relative && relative < row.start + row.height)?;
        let column = usize::from(point.0 - area.x);
        let gutter_width = row
            .gutter
            .as_ref()
            .map_or(0, |gutter| gutter.width as usize);

        (column < gutter_width + row.line.width()).then_some(index)
    }

    pub fn height(&self) -> usize {
        self.entries.last().map_or(0, |entry| entry.end)
    }

    pub(super) fn link_at(&self, area: Rect, offset: usize, point: (u16, u16)) -> Option<&str> {
        if !area.contains(point.into()) {
            return None;
        }

        let absolute_row = offset + (point.1 - area.y) as usize;
        let entry = self.entries.iter().find(|entry| entry.end > absolute_row)?;
        let entry_start = entry.end - entry.height;
        let row = entry.rows.iter().find(|row| {
            let start = entry_start + row.start;
            absolute_row >= start && absolute_row < start + row.height
        })?;
        let gutter_width = row.gutter.as_ref().map_or(0, |gutter| gutter.width);
        let column = point.0.checked_sub(area.x + gutter_width)?;
        let wrapped = absolute_row - entry_start - row.start;
        let range = row.ranges.get(wrapped)?;
        let byte = byte_at_column(&row.line.to_string(), range, column)?;

        row.links
            .iter()
            .find(|link| link.start <= byte && byte < link.end)
            .map(|link| link.url.as_str())
    }

    pub fn draw(&self, frame: &mut Frame<'_>, area: Rect, offset: usize) {
        self.draw_buffer(frame.buffer_mut(), area, offset);
    }

    pub(super) fn draw_buffer(
        &self,
        buffer: &mut ratatui::buffer::Buffer,
        area: Rect,
        offset: usize,
    ) {
        let mut y = area.y;
        let first = self.entries.partition_point(|entry| entry.end <= offset);

        for entry in &self.entries[first..] {
            let start = entry.end - entry.height;
            let relative = offset.saturating_sub(start);
            let first_row = entry
                .rows
                .partition_point(|row| row.start + row.height <= relative);

            for row in &entry.rows[first_row..] {
                if y >= area.bottom() {
                    return;
                }

                let skip = relative.saturating_sub(row.start);
                let height = (row.height - skip).min((area.bottom() - y) as usize) as u16;
                let gutter_width = row.gutter.as_ref().map_or(0, |gutter| gutter.width);

                if let Some(gutter) = &row.gutter {
                    for dy in 0..height {
                        let first = skip == 0 && dy == 0;
                        Paragraph::new(
                            if first {
                                gutter.first.clone()
                            } else {
                                gutter.continuation.clone()
                            }
                            .style(row.line.style),
                        )
                        .render(Rect::new(area.x, y + dy, gutter_width, 1), buffer);
                    }
                }

                for (dy, line) in row
                    .wrapped
                    .iter()
                    .skip(skip)
                    .take(height as usize)
                    .enumerate()
                {
                    Paragraph::new(line.clone()).render(
                        Rect::new(
                            area.x + gutter_width,
                            y + dy as u16,
                            area.width.saturating_sub(gutter_width),
                            1,
                        ),
                        buffer,
                    );
                }

                y += height;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn markdown_rule_occupies_one_full_row_after_resize() {
        let items = vec![super::TranscriptItem::Assistant(
            "before\n---\nafter\n".into(),
        )];
        let mut layout = super::TranscriptLayout::default();
        for width in [80, 180, 40] {
            layout.update(&items, None, width);
            let entry = &layout.entries[0];
            let rule = &entry.rows[1];
            assert_eq!(rule.line.to_string(), "─".repeat(usize::from(width)));
            assert_eq!(rule.height, 1);
            assert!(rule.gutter.is_none());
            assert_eq!(rule.wrapped[0].width(), usize::from(width));
        }
    }

    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn nested_thinking_code_gutter_is_recognized() {
        assert!(is_rail_gutter("  ┊ │ "));
        assert!(is_rail_gutter("    │ "));
        assert!(is_rail_gutter("  ┊ "));
        assert!(!is_rail_gutter("text │ "));
    }

    #[test]
    fn hover_disclosure_only_active_within_text_width() {
        let mut row = super::super::app::ThinkingRow::from("some thinking content".to_owned());
        row.finished = true;
        row.elapsed = Some(std::time::Duration::from_secs(1));
        let thinking = TranscriptItem::Thinking(row);
        let tool = TranscriptItem::Tool(super::super::app::ToolRow {
            id: "tool_1".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({ "path": "src/main.rs" }).to_string(),
            status: super::super::app::ToolStatus::Finished,
            output: "done".into(),
        });
        let items = vec![thinking, tool];
        let mut layout = TranscriptLayout::default();
        let area = Rect::new(0, 0, 80, 40);

        layout.update(&items, None, 80);

        // Thinking line: (2, 0) is over text -> Some(0); (50, 0) is in trailing blank space -> None.
        assert_eq!(layout.disclosure_at(&items, area, 0, (2, 0)), Some(0));
        assert_eq!(layout.disclosure_at(&items, area, 0, (50, 0)), None);

        // Tool line: (2, 2) is over text -> Some(1); (50, 2) is in trailing blank space -> None.
        let tool_y = layout.entries[0].height as u16;
        assert_eq!(layout.disclosure_at(&items, area, 0, (2, tool_y)), Some(1));
        assert_eq!(layout.disclosure_at(&items, area, 0, (50, tool_y)), None);
    }

    #[test]
    fn tool_title_and_output_are_clickable() {
        let items = vec![TranscriptItem::Tool(super::super::app::ToolRow {
            id: "web".into(),
            name: "web_search".into(),
            arguments: serde_json::json!({ "query": "papers" }).to_string(),
            status: super::super::app::ToolStatus::Failed,
            output: "first\nsecond\nthird\nfourth".into(),
        })];
        let mut layout = TranscriptLayout::default();
        let area = Rect::new(0, 0, 80, 40);

        layout.update(&items, None, 80);
        assert_eq!(layout.disclosure_at(&items, area, 0, (0, 0)), Some(0));
        for y in 1..layout.height() - 1 {
            assert_eq!(
                layout.disclosure_at(&items, area, 0, (0, y as u16)),
                Some(0)
            );
        }
        let collapsed = layout.height();
        layout.update(&items, Some(0), 80);

        assert!(layout.height() > collapsed);
        assert_eq!(layout.disclosure_at(&items, area, 0, (80, 2)), None);
    }

    #[test]
    fn wrapped_thinking_and_code_keep_gutters_even_when_scrolled_mid_line() {
        let text = "中文 words 👋 words ".repeat(80);

        for (item, marker) in [
            (TranscriptItem::Thinking(text.clone().into()), "┊"),
            (
                TranscriptItem::Assistant(format!("```rust\n// {text}\n```")),
                "│",
            ),
        ] {
            let mut layout = TranscriptLayout::default();
            layout.update(&[item], None, 24);
            let mut terminal = Terminal::new(TestBackend::new(24, 6)).unwrap();
            terminal
                .draw(|frame| layout.draw(frame, frame.area(), 3))
                .unwrap();
            let buffer = terminal.backend().buffer();

            for y in 0..6 {
                let column = 2;
                assert_eq!(buffer[(column, y)].symbol(), marker);
                assert_eq!(buffer[(column + 1, y)].symbol(), " ");
            }
        }
    }

    #[test]
    fn paragraph_continuations_keep_their_left_margin() {
        let text = "### 标题\n\n这是一段中文说明，换行以后仍然需要与段落开头对齐。\n  缩进段落也要保持原有的额外缩进，而不是向左跳。";
        let mut layout = TranscriptLayout::default();
        layout.update(&[TranscriptItem::Assistant(text.into())], None, 24);

        for row in layout.entries[0].rows.iter().filter(|row| row.height > 1) {
            let gutter = row.gutter.as_ref().expect("wrapped prose retains a margin");
            assert!(gutter.width >= 2);
            assert_eq!(gutter.first.width(), gutter.continuation.width());
        }
        assert!(layout.entries[0].rows.iter().any(|row| row.height > 1));
    }

    #[test]
    fn wrapped_lists_keep_markers_with_content_and_use_hanging_indents() {
        let answer = "1. **第一项很长很长**：后面的说明继续延伸到下一行。\n2. **第二项也很长**：后面的说明同样继续延伸。\n- 无序项也应保留悬挂缩进。";
        let mut layout = TranscriptLayout::default();
        layout.update(&[TranscriptItem::Assistant(answer.into())], None, 24);
        let rows = &layout.entries[0].rows;

        for (index, marker) in ["1. ", "2. ", "• "].into_iter().enumerate() {
            let gutter = rows[index].gutter.as_ref().unwrap();

            assert!(gutter.first.to_string().contains(marker));
            assert_eq!(gutter.first.width(), gutter.continuation.width());
            assert!(!rows[index].line.to_string().is_empty());
        }

        let mut terminal = Terminal::new(TestBackend::new(24, 8)).unwrap();
        terminal
            .draw(|frame| layout.draw(frame, frame.area(), 0))
            .unwrap();
        let buffer = terminal.backend().buffer();

        assert_eq!(buffer[(2, 0)].symbol(), "1");
        assert_ne!(buffer[(5, 0)].symbol(), " ");
        assert_eq!(buffer[(2, 1)].symbol(), " ");
        assert_eq!(buffer[(3, 1)].symbol(), " ");
        assert_eq!(buffer[(4, 1)].symbol(), " ");
    }

    #[test]
    fn task_list_checkbox_keeps_a_hanging_indent() {
        let mut layout = TranscriptLayout::default();
        layout.update(
            &[TranscriptItem::Assistant(
                "- [ ] 这是一个会换行的待完成任务，用于验证复选框不会再和项目符号重复显示。".into(),
            )],
            None,
            20,
        );
        let row = &layout.entries[0].rows[0];

        assert_eq!(row.gutter.as_ref().unwrap().first.to_string(), "  ☐ ");
        assert_eq!(
            row.gutter.as_ref().unwrap().first.width(),
            row.gutter.as_ref().unwrap().continuation.width()
        );
    }

    #[test]
    fn ctrl_click_targets_link_labels_but_not_code_source() {
        let mut layout = TranscriptLayout::default();
        let area = Rect::new(0, 0, 18, 8);
        layout.update(
            &[TranscriptItem::Assistant(
                "阅读 [Orbit 文档](https://example.com/orbit) 获取详情。".into(),
            )],
            None,
            area.width,
        );
        let target = (area.y..area.bottom()).find_map(|y| {
            (area.x..area.right()).find_map(|x| {
                layout
                    .link_at(area, 0, (x, y))
                    .map(|url| (x, y, url.to_owned()))
            })
        });

        assert_eq!(
            target.as_ref().map(|(_, _, url)| url.as_str()),
            Some("https://example.com/orbit")
        );

        let mut code_layout = TranscriptLayout::default();
        code_layout.update(
            &[TranscriptItem::Assistant(
                "```markdown\n[不是链接](https://example.com/no)\n```".into(),
            )],
            None,
            area.width,
        );

        assert!((area.y..area.bottom()).all(|y| {
            (area.x..area.right()).all(|x| code_layout.link_at(area, 0, (x, y)).is_none())
        }));
    }

    #[test]
    fn plain_output_and_wrapped_table_cells_keep_their_own_link_targets() {
        use super::super::app::{ToolRow, ToolStatus};
        let items = [
            TranscriptItem::User("中文 https://example.com/user".into()),
            TranscriptItem::CommandResult("结果 https://example.com/command".into()),
            TranscriptItem::Tool(ToolRow {
                id: "tool".into(), name: "run_command".into(), arguments: "{}".into(),
                status: ToolStatus::Finished, output: "地址 https://example.com/tool".into(),
            }),
            TranscriptItem::Assistant("| 名称 | 地址 |\n| --- | --- |\n| [中文链接](https://example.com/one) | [中文链接](https://example.com/two) |\n".into()),
        ];
        for width in [10, 26, 80] {
            let mut layout = TranscriptLayout::default();
            layout.update(&items, Some(2), width);
            let area = Rect::new(3, 2, width, layout.height() as u16);
            let mut targets = HashSet::new();
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    if let Some(url) = layout.link_at(area, 0, (x, y)) {
                        targets.insert(url);
                    }
                }
            }
            for suffix in ["user", "command", "tool", "one", "two"] {
                assert!(
                    targets.contains(format!("https://example.com/{suffix}").as_str()),
                    "width {width}, missing {suffix}: {targets:?}"
                );
            }
            for row in &layout.entries.last().unwrap().rows {
                for link in &row.links {
                    let text = row.line.to_string();
                    assert!(text.get(link.start..link.end).is_some());
                    assert!(link.url.ends_with("/one") || link.url.ends_with("/two"));
                }
            }
        }
    }

    #[test]
    fn command_results_remain_plain_beneath_the_command() {
        let mut layout = TranscriptLayout::default();
        layout.update(
            &[TranscriptItem::CommandResult("状态\n第二行细节".into())],
            None,
            32,
        );
        let rows = &layout.entries[0].rows;

        assert_eq!(rows[0].line.to_string(), "  状态");
        assert_eq!(rows[1].line.to_string(), "  第二行细节");
    }

    #[test]
    fn user_turn_marker_is_joined_to_the_top_border() {
        let mut layout = TranscriptLayout::default();
        layout.update(&[TranscriptItem::User("问题".into())], None, 32);

        assert!(
            layout.entries[0].rows[0]
                .line
                .to_string()
                .starts_with("╭─❯─")
        );
    }

    #[test]
    fn thinking_keeps_one_row_before_the_following_answer() {
        let mut layout = TranscriptLayout::default();
        layout.update(
            &[
                TranscriptItem::Thinking("inspect the request".to_owned().into()),
                TranscriptItem::Assistant("final answer".into()),
            ],
            None,
            40,
        );
        let thinking = &layout.entries[0];

        assert!(thinking.rows.last().unwrap().line.to_string().is_empty());
        assert_eq!(layout.entries[1].rows[0].line.to_string(), "final answer");
        assert_eq!(layout.entries[1].rows[0].gutter.as_ref().unwrap().width, 2);
    }

    #[test]
    fn thinking_code_keeps_both_rails_when_wrapped() {
        let mut layout = TranscriptLayout::default();
        layout.update(
            &[TranscriptItem::Thinking(
                format!("```markdown\n{}\n```", "long Markdown source ".repeat(4)).into(),
            )],
            None,
            22,
        );
        let row = &layout.entries[0].rows[1];
        let gutter = row.gutter.as_ref().unwrap();

        assert_eq!(gutter.first.to_string(), "  ┊ │ ");
        assert_eq!(gutter.continuation.to_string(), "  ┊ │ ");
        assert!(row.height > 1);

        let mut terminal = Terminal::new(TestBackend::new(22, 5)).unwrap();
        terminal
            .draw(|frame| layout.draw(frame, frame.area(), 0))
            .unwrap();

        for y in 1..5 {
            assert_eq!(terminal.backend().buffer()[(2, y)].symbol(), "┊");
            assert_eq!(terminal.backend().buffer()[(4, y)].symbol(), "│");
        }
    }

    #[test]
    fn unicode_line_breaks_fill_cjk_rows_and_quotes_keep_a_continuous_rail() {
        let source = "前缀 + 杂乱客厅背景 后文";
        let ranges = wrap_ranges(source, 10);

        assert!(source[ranges[0].clone()].width() >= 9);
        assert!(ranges.len() >= 2);

        let mut layout = TranscriptLayout::default();
        layout.update(
            &[TranscriptItem::Assistant(format!(
                "> {}",
                "这是一段需要换行的引用内容".repeat(4)
            ))],
            None,
            20,
        );
        let row = &layout.entries[0].rows[0];
        let gutter = row.gutter.as_ref().unwrap();

        assert_eq!(gutter.first.to_string(), "  ▎ ");
        assert_eq!(gutter.continuation.to_string(), "  ▎ ");
        assert!(row.height > 1);

        let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();
        terminal
            .draw(|frame| layout.draw(frame, frame.area(), 0))
            .unwrap();

        for y in 0..5 {
            assert_eq!(terminal.backend().buffer()[(2, y)].symbol(), "▎");
        }
    }

    #[tokio::test]
    #[ignore = "requires a disposable database copy and ORBIT_PROFILE_SESSION"]
    async fn profile_saved_session_layout() {
        let path = std::env::var("ORBIT_PROFILE_DATABASE").unwrap();
        let id = std::env::var("ORBIT_PROFILE_SESSION").unwrap();
        let start = std::time::Instant::now();
        let session = crate::session::SessionState::open(path.into(), Some(id), "profile".into())
            .await
            .unwrap();
        eprintln!("load: {:?}", start.elapsed());
        let start = std::time::Instant::now();
        let mut state = super::super::app::UiState::new("profile");
        session.restore_ui(&mut state);
        eprintln!(
            "replay: {:?}, items: {}",
            start.elapsed(),
            state.transcript.len()
        );
        let mut layout = TranscriptLayout::default();
        let start = std::time::Instant::now();
        layout.update(&state.transcript, None, 100);
        eprintln!("layout: {:?}, rows: {}", start.elapsed(), layout.height());
    }

    #[test]
    fn table_streaming_one_character_at_a_time_matches_complete_render() {
        let source = "| # | 语言 | 特性 |\n|---|---|---|\n| 1 | **Python** | `complex` |\n| 2 | C | restrict |\n| 3 | C++ | 模板 |\n\n后文\n";
        let mut text = String::new();
        let mut layout = TranscriptLayout::default();

        for ch in source.chars() {
            text.push(ch);
            let items = [TranscriptItem::Assistant(text.clone())];
            layout.invalidate_from(0);
            layout.update(&items, None, 80);
            let mut fresh = TranscriptLayout::default();
            fresh.update(&items, None, 80);
            let lines = |l: &TranscriptLayout| {
                l.entries[0]
                    .rows
                    .iter()
                    .map(|r| r.line.clone())
                    .collect::<Vec<_>>()
            };

            assert_eq!(lines(&layout), lines(&fresh), "input: {text:?}");
        }
    }

    #[test]
    fn streamed_tables_match_fresh_layout_through_split_delimiters_and_resize() {
        let mut text = String::new();
        let mut layout = TranscriptLayout::default();

        for chunk in [
            "intro\n| 名称 | 数量 |\n",
            "| ---",
            " | ---: |\n",
            "| 中文 | 12",
            "3 |\n",
            "| **test** | 4 |\n",
            "\n```rust\n/* hi\n",
            "comment */\nlet x = 1;\n```\nafter\n",
        ] {
            text.push_str(chunk);
            let items = vec![TranscriptItem::Assistant(text.clone())];
            layout.invalidate_from(0);
            layout.update(&items, None, 35);
            let mut fresh = TranscriptLayout::default();
            fresh.update(&items, None, 35);

            assert_eq!(layout.height(), fresh.height());

            let actual: Vec<_> = layout.entries[0].rows.iter().map(|r| &r.line).collect();
            let expected: Vec<_> = fresh.entries[0].rows.iter().map(|r| &r.line).collect();

            assert_eq!(actual, expected, "source: {text}");
        }

        for width in [12, 60] {
            layout.update(&[TranscriptItem::Assistant(text.clone())], None, width);

            assert!(layout.height() > 0);
        }
    }

    #[test]
    fn incremental_layout_matches_fresh_layout_for_unicode_wrapping_and_split_fences() {
        let mut layout = TranscriptLayout::default();
        let mut items = vec![TranscriptItem::Assistant(String::new())];

        for chunk in [
            "你好 e\u{301} words words",
            " **加",
            "粗** 与 `代码`",
            "\n- 项目 [链接](https://example.com)\n> 引用",
            "\n# Heading\n`",
            "``rust\n",
            "let x = 1;\n```",
            "\nlast 👨‍👩‍👧‍👦 line\n",
        ] {
            let TranscriptItem::Assistant(text) = &mut items[0] else {
                unreachable!()
            };

            text.push_str(chunk);
            layout.invalidate_from(0);

            for width in [17] {
                layout.update(&items, None, width);
                let mut reference = TranscriptLayout::default();
                reference.update(&items, None, width);

                assert_eq!(layout.height(), reference.height());

                for offset in 0..layout.height() {
                    let mut actual = Terminal::new(TestBackend::new(width, 5)).unwrap();
                    actual.draw(|f| layout.draw(f, f.area(), offset)).unwrap();
                    let mut expected = Terminal::new(TestBackend::new(width, 5)).unwrap();
                    expected
                        .draw(|f| reference.draw(f, f.area(), offset))
                        .unwrap();

                    assert_eq!(
                        actual.backend().buffer(),
                        expected.backend().buffer(),
                        "width {width} offset {offset}"
                    );
                }
            }
        }
    }

    #[test]
    fn width_and_expansion_invalidate_cached_heights() {
        let items = vec![
            TranscriptItem::Thinking("thinking words ".repeat(80).into()),
            TranscriptItem::Assistant("answer words ".repeat(80)),
        ];
        let mut layout = TranscriptLayout::default();

        for (width, expanded) in [
            (80, None),
            (20, None),
            (20, Some(1)),
            (80, Some(1)),
            (80, None),
        ] {
            layout.update(&items, expanded, width);
            let mut expected = TranscriptLayout::default();
            expected.update(&items, expanded, width);

            assert_eq!(layout.height(), expected.height());

            let offset = layout.height().saturating_sub(6);
            let mut actual_terminal = Terminal::new(TestBackend::new(width, 6)).unwrap();
            actual_terminal
                .draw(|f| layout.draw(f, f.area(), offset))
                .unwrap();
            let mut expected_terminal = Terminal::new(TestBackend::new(width, 6)).unwrap();
            expected_terminal
                .draw(|f| expected.draw(f, f.area(), offset))
                .unwrap();

            assert_eq!(
                actual_terminal.backend().buffer(),
                expected_terminal.backend().buffer()
            );
        }
    }

    #[test]
    fn user_turns_use_a_framed_bubble_and_wrap_inside_its_side_rails() {
        let mut layout = TranscriptLayout::default();
        layout.update(
            &[TranscriptItem::User(
                "一段很长的用户输入，需要在边框内换行。".repeat(4),
            )],
            None,
            24,
        );
        let rows = &layout.entries[0].rows;

        assert_eq!(
            rows.first().unwrap().line.to_string().chars().next(),
            Some('╭')
        );
        assert_eq!(rows.last().unwrap().line.to_string(), "");
        assert!(rows.iter().any(|row| {
            row.gutter
                .as_ref()
                .is_some_and(|gutter| gutter.first.to_string() == "│ ")
        }));
        assert!(rows.iter().any(|row| row.line.to_string().starts_with('╰')));
        assert!(rows.iter().all(|row| row.line.width() <= 24));
    }

    #[test]
    fn streaming_keeps_completed_lines_and_scrolling_reuses_layout() {
        let mut items = vec![TranscriptItem::Assistant("history line\n".repeat(10000))];
        let mut layout = TranscriptLayout::default();
        layout.update(&items, None, 100);
        let stable = layout.entries[0].rows[500].line.spans[0].content.as_ptr();

        for _ in 0..30 {
            let TranscriptItem::Assistant(text) = &mut items[0] else {
                unreachable!()
            };

            text.push_str("stream delta ");
            layout.invalidate_from(0);
            layout.update(&items, None, 100);

            assert_eq!(
                stable,
                layout.entries[0].rows[500].line.spans[0].content.as_ptr()
            );
        }

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let started = std::time::Instant::now();

        for offset in (0..10000).step_by(100) {
            layout.update(&items, None, 100);
            terminal.draw(|f| layout.draw(f, f.area(), offset)).unwrap();
        }

        eprintln!(
            "100 scroll frames over 10000 lines: {:?}",
            started.elapsed()
        );

        assert_eq!(
            stable,
            layout.entries[0].rows[500].line.spans[0].content.as_ptr()
        );
    }

    #[test]
    fn wrapped_indented_code_blocks_keep_rails_on_continuation_lines() {
        let text = "代码片段:\n  ```rust\n  const FRAME_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);\n  ```";
        let mut layout = TranscriptLayout::default();
        layout.update(&[TranscriptItem::Assistant(text.into())], None, 45);

        let code_row = layout.entries[0]
            .rows
            .iter()
            .find(|row| row.height > 1 && row.line.to_string().contains("FRAME_DEADLINE"))
            .expect("code line should wrap into multiple rows");

        let gutter = code_row
            .gutter
            .as_ref()
            .expect("indented code block retains gutter");
        assert!(gutter.width >= 4);
        assert_eq!(gutter.first.to_string(), gutter.continuation.to_string());
        assert!(gutter.continuation.to_string().contains('│'));

        let mut terminal = Terminal::new(TestBackend::new(45, 6)).unwrap();
        terminal
            .draw(|frame| layout.draw(frame, frame.area(), 0))
            .unwrap();
        let buffer = terminal.backend().buffer();

        let rail_col = gutter.continuation.to_string().find('│').unwrap();
        assert_eq!(buffer[(rail_col as u16, 2)].symbol(), "│");
        assert_eq!(buffer[(rail_col as u16, 3)].symbol(), "│");
    }
}
