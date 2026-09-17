//! Cache logical-line layout, and hand only visible lines to Ratatui.
//! Ratatui still owns word wrapping and grapheme widths. Scrolling never asks a
//! Paragraph to wrap and skip the entire conversation from its beginning.
use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{Paragraph, Wrap},
};

use super::{app::TranscriptItem, render};

#[derive(Debug)]
struct Row {
    line: Line<'static>,
    start: usize,
    height: usize,
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
        let height = Paragraph::new(line.clone())
            .wrap(Wrap { trim: false })
            .line_count(width);
        self.rows.push(Row {
            line,
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
        let mut code = self.code;
        let mut index = self.logical_index;
        let mut consumed = self.parsed_bytes;
        for part in text[self.parsed_bytes..].split_inclusive('\n') {
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
                    render::thinking_line(line)
                } else {
                    render::assistant_line(line, index, &mut code)
                },
                width,
            );
            index += 1;
            consumed += part.len();
            if complete {
                self.stable_rows = self.rows.len();
                self.parsed_bytes = consumed;
                self.logical_index = index;
                self.code = code;
            }
        }
        if !thinking && !text.is_empty() {
            self.push(Line::from(""), width);
        }
        self.observed_bytes = Some(text.len());
    }
}

#[derive(Debug, Default)]
pub(super) struct TranscriptLayout {
    entries: Vec<Entry>,
    width: u16,
    expanded: Option<usize>,
    dirty_from: usize,
}

impl TranscriptLayout {
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

    pub fn height(&self) -> usize {
        self.entries.last().map_or(0, |entry| entry.end)
    }

    pub fn draw(&self, frame: &mut Frame<'_>, area: Rect, offset: usize) {
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
                frame.render_widget(
                    Paragraph::new(row.line.clone())
                        .wrap(Wrap { trim: false })
                        .scroll((skip.min(u16::MAX as usize) as u16, 0)),
                    Rect::new(area.x, y, area.width, height),
                );
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
    fn incremental_layout_matches_ratatui_for_unicode_wrapping_and_split_fences() {
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
                let reference = render::transcript_lines(&items, false);
                assert_eq!(
                    layout.height(),
                    Paragraph::new(reference.clone())
                        .wrap(Wrap { trim: false })
                        .line_count(width)
                );
                for offset in 0..layout.height() {
                    let mut actual = Terminal::new(TestBackend::new(width, 5)).unwrap();
                    actual.draw(|f| layout.draw(f, f.area(), offset)).unwrap();
                    let mut expected = Terminal::new(TestBackend::new(width, 5)).unwrap();
                    expected
                        .draw(|f| {
                            f.render_widget(
                                Paragraph::new(reference.clone())
                                    .wrap(Wrap { trim: false })
                                    .scroll((offset as u16, 0)),
                                f.area(),
                            )
                        })
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
            TranscriptItem::User("question".into()),
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
