//! Cache Unicode-aware physical-line layout, and hand only visible lines to
//! Ratatui. Scrolling never asks a Paragraph to wrap and skip the entire
//! conversation from its beginning.
#[cfg(test)]
use ratatui::widgets::Wrap;
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
    text == "• "
        || text
            .strip_suffix(". ")
            .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
}

fn take_gutter(line: &mut Line<'static>) -> Option<Gutter> {
    if line
        .spans
        .first()
        .is_some_and(|span| matches!(span.content.as_ref(), "  │ " | "  ┊ "))
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
        .map(|(index, _)| index)?;
    let prefix = line.spans.drain(..=marker).collect::<Vec<_>>();
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

fn wrap_line(line: &Line<'static>, width: u16) -> (Vec<Line<'static>>, Vec<Range<usize>>) {
    let text = line.to_string();
    let ranges = wrap_ranges(&text, width);
    let lines = ranges
        .iter()
        .cloned()
        .map(|range| slice_line(line, range))
        .collect();
    (lines, ranges)
}

#[derive(Debug, Default)]
struct Entry {
    rows: Vec<Row>,
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
        let gutter = take_gutter(&mut line);
        let content_width = width.saturating_sub(gutter.as_ref().map_or(0, |gutter| gutter.width));
        let (wrapped, ranges) = wrap_line(&line, content_width);
        let height = wrapped.len();
        self.rows.push(Row {
            gutter,
            line,
            wrapped,
            ranges,
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
                    self.push(line, width);
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
            self.push(
                if thinking {
                    render::thinking_line(line, index, &mut code)
                } else {
                    render::assistant_line(line, index, &mut code)
                },
                width,
            );
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
        // A complete frame lets user turns remain visually distinct after their
        // contents wrap. Keep a plain fallback for terminals too narrow to
        // draw the two side rails without stealing all useful text width.
        if width < 9 {
            for (index, line) in text.lines().enumerate() {
                self.push(
                    Line::from(vec![
                        Span::styled(
                            if index == 0 { "❯ " } else { "  " },
                            ratatui::style::Style::default()
                                .fg(ratatui::style::Color::Rgb(218, 139, 109)),
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

        let border = ratatui::style::Style::default().fg(ratatui::style::Color::Rgb(218, 139, 109));
        let top_label = "╭─❯ ";
        self.push(
            Line::from(Span::styled(
                format!(
                    "{top_label}{}╮",
                    "─".repeat(usize::from(width) - top_label.width() - 1)
                ),
                border,
            )),
            width,
        );
        let inner_width = width - 4;
        for source in text.split('\n') {
            let content = Line::from(Span::styled(
                source.to_owned(),
                ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::BOLD),
            ));
            let (wrapped, _) = wrap_line(&content, inner_width);
            for line in wrapped {
                let padding = usize::from(inner_width).saturating_sub(line.width());
                let mut spans = vec![Span::styled("│ ", border)];
                spans.extend(line.spans);
                spans.push(Span::raw(" ".repeat(padding)));
                spans.push(Span::styled(" │", border));
                self.push(Line::from(spans), width);
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
    expanded: Option<usize>,
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

    pub fn update(&mut self, items: &[TranscriptItem], expanded: Option<usize>, width: u16) {
        if self.width != width || items.len() < self.entries.len() {
            self.entries.clear();
            self.dirty_from = 0;
            self.width = width;
        }
        if self.expanded != expanded {
            for index in self.expanded.into_iter().chain(expanded) {
                self.invalidate_from(index);
            }
            self.expanded = expanded;
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
                TranscriptItem::Thinking(text) => entry.stream(text, true, width),
                TranscriptItem::User(text) => {
                    *entry = Entry::default();
                    entry.user(text, width);
                }
                _ => {
                    *entry = Entry::default();
                    for line in render::transcript_lines(
                        std::slice::from_ref(item),
                        expanded == Some(first + relative),
                    ) {
                        entry.push(line, width);
                    }
                }
            }
            end += entry.height;
            entry.end = end;
        }
        self.dirty_from = items.len();
    }

    pub fn visible_tool(
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
                    && matches!(items.get(index), Some(TranscriptItem::Tool(_))))
                .then_some(index)
            })
    }

    pub(super) fn item_offset(&self, index: usize) -> Option<usize> {
        self.entries
            .get(index)
            .map(|entry| entry.end - entry.height)
    }

    pub fn height(&self) -> usize {
        self.entries.last().map_or(0, |entry| entry.end)
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
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn wrapped_thinking_and_code_keep_gutters_even_when_scrolled_mid_line() {
        let text = "中文 words 👋 words ".repeat(80);
        for (item, marker) in [
            (TranscriptItem::Thinking(text.clone()), "┊"),
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
                assert_eq!(buffer[(0, y)].symbol(), " ");
                assert_eq!(buffer[(1, y)].symbol(), " ");
                assert_eq!(buffer[(2, y)].symbol(), marker);
                assert_eq!(buffer[(3, y)].symbol(), " ");
            }
        }
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
    fn thinking_keeps_one_row_before_the_following_answer() {
        let mut layout = TranscriptLayout::default();
        layout.update(
            &[
                TranscriptItem::Thinking("inspect the request".into()),
                TranscriptItem::Assistant("final answer".into()),
            ],
            None,
            40,
        );
        let thinking = &layout.entries[0];
        assert!(thinking.rows.last().unwrap().line.to_string().is_empty());
        assert_eq!(layout.entries[1].rows[0].line.to_string(), "  final answer");
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
            TranscriptItem::Thinking("thinking words ".repeat(80)),
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
            let expected =
                Paragraph::new(render::transcript_lines(&items, false)).wrap(Wrap { trim: false });
            assert_eq!(layout.height(), expected.line_count(width));
            let offset = layout.height().saturating_sub(6);
            let mut actual_terminal = Terminal::new(TestBackend::new(width, 6)).unwrap();
            actual_terminal
                .draw(|f| layout.draw(f, f.area(), offset))
                .unwrap();
            let mut expected_terminal = Terminal::new(TestBackend::new(width, 6)).unwrap();
            expected_terminal
                .draw(|f| f.render_widget(expected.clone().scroll((offset as u16, 0)), f.area()))
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
        assert!(
            rows.iter()
                .any(|row| row.line.to_string().starts_with("│ "))
        );
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
}
