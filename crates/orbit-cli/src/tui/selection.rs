//! Application-owned selection: endpoints use content coordinates, while each
//! drag is constrained to the region in which it started. Edge scrolling runs
//! on a timer so it continues when the pointer stops moving, as in tmux's
//! window_copy_scroll_timer: https://github.com/tmux/tmux/blob/master/window-copy.c
//! Textarea mouse interaction reference:
//! https://github.com/ratatui/ratatui-textarea/blob/main/examples/minimal.rs
use base64::{Engine, engine::general_purpose::STANDARD};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    buffer::{Buffer, Cell},
    layout::Rect,
};
use std::{
    collections::{BTreeMap, HashMap},
    io::{self, Write},
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthStr;

pub(super) fn copy_text(text: &str) -> io::Result<()> {
    write!(
        io::stdout(),
        "\x1b]52;c;{}\x07",
        STANDARD.encode(text.as_bytes())
    )?;
    io::stdout().flush()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    Transcript,
    Footer,
    Approval,
}

#[derive(Debug, Default)]
pub struct Selection {
    pub viewport: Rect,
    pub footer: Rect,
    pub approval: Rect,
    pub offset: usize,
    region: Option<Region>,
    anchor: Option<(u16, usize)>,
    end: Option<(u16, usize)>,
    pointer: Option<(u16, u16)>,
    down: Option<(u16, u16)>,
    dragging: bool,
    moved: bool,
    cleared_on_press: bool,
    extend: bool,
    rows: BTreeMap<usize, Vec<Cell>>,
    decorations: BTreeMap<usize, super::transcript::Decorations>,
    filled: Option<(usize, usize)>,
    latest: Option<Buffer>,
    frozen: Option<Buffer>,
    last_auto: Option<Instant>,
}

impl Selection {
    pub(super) fn clear(&mut self) {
        self.region = None;
        self.anchor = None;
        self.end = None;
        self.pointer = None;
        self.down = None;
        self.dragging = false;
        self.moved = false;
        self.cleared_on_press = false;
        self.extend = false;
        self.rows.clear();
        self.filled = None;
        self.frozen = None;
        self.last_auto = None;
    }

    pub fn active(&self) -> bool {
        self.anchor.is_some()
    }

    pub fn transcript_drag(&self) -> bool {
        self.dragging && self.region == Some(Region::Transcript)
    }

    pub fn dragging(&self) -> bool {
        self.dragging
    }

    fn area(&self) -> Rect {
        match self.region {
            Some(Region::Transcript) => self.viewport,
            Some(Region::Footer) => self.footer,
            Some(Region::Approval) => self.approval,
            None => Rect::default(),
        }
    }

    fn base(&self) -> usize {
        if self.region == Some(Region::Transcript) {
            self.offset
        } else {
            0
        }
    }

    fn position(&self, (x, y): (u16, u16)) -> (u16, usize) {
        let area = self.area();
        (
            x.clamp(area.x, area.right().saturating_sub(1)) - area.x,
            (y.clamp(area.y, area.bottom().saturating_sub(1)) - area.y) as usize + self.base(),
        )
    }

    pub fn click(&self, event: &Event) -> Option<(u16, u16)> {
        if let Event::Mouse(mouse) = event
            && mouse.kind == MouseEventKind::Up(MouseButton::Left)
            && self.dragging
            && !self.moved
            && !(self.cleared_on_press && self.region == Some(Region::Transcript))
            && self.down == Some((mouse.column, mouse.row))
        {
            self.down
        } else {
            None
        }
    }

    pub fn begin_scroll(&mut self) {
        self.frozen = None;

        if self.dragging && self.region == Some(Region::Transcript) {
            self.extend = true;
            self.moved = true;
        }
    }

    pub fn auto_scroll(&mut self, now: Instant) -> i8 {
        if !self.dragging || !self.moved || self.region != Some(Region::Transcript) {
            return 0;
        }
        let Some((_, y)) = self.pointer else {
            return 0;
        };

        let direction = if y <= self.viewport.y {
            -1
        } else if y >= self.viewport.bottom().saturating_sub(1) {
            1
        } else {
            return 0;
        };

        if self
            .last_auto
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(80))
        {
            return 0;
        }

        self.last_auto = Some(now);
        self.begin_scroll();

        direction
    }

    pub fn handle(&mut self, event: &Event) -> io::Result<bool> {
        match event {
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let cleared_on_press = self.active() && self.moved;
                    self.clear();
                    self.cleared_on_press = cleared_on_press;
                    let point = (mouse.column, mouse.row);
                    self.region = [
                        (Region::Transcript, self.viewport),
                        (Region::Footer, self.footer),
                        (Region::Approval, self.approval),
                    ]
                    .into_iter()
                    .find(|(_, area)| area.contains(point.into()))
                    .map(|(region, _)| region);

                    if self.region.is_some() {
                        self.anchor = Some(self.position(point));
                        self.end = self.anchor;
                        self.pointer = Some(point);
                        self.down = Some(point);
                        self.dragging = true;
                        self.frozen = self.latest.clone();

                        if let Some(buffer) = self.latest.clone() {
                            self.capture(&buffer);
                        }
                    }

                    return Ok(true);
                }

                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
                    if self.dragging =>
                {
                    let point = (mouse.column, mouse.row);
                    self.moved |= self.down != Some(point);
                    self.pointer = Some(point);
                    self.end = Some(self.position(point));

                    if matches!(mouse.kind, MouseEventKind::Up(_)) {
                        self.dragging = false;

                        if !self.moved {
                            self.clear();
                        }
                    }

                    return Ok(true);
                }

                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    self.pointer = Some((mouse.column, mouse.row));
                    self.begin_scroll();

                    // A drag belongs to its starting region, even outside its bounds.
                    if self.dragging && self.region != Some(Region::Transcript) {
                        return Ok(true);
                    }
                }

                _ => {}
            },

            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if self.active()
                    && key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    let text = self.text();

                    if !text.is_empty() {
                        copy_text(&text)?;
                    }

                    return Ok(true);
                }
                if self.active() && key.code == KeyCode::Esc {
                    self.clear();
                    return Ok(true);
                }

                self.clear();
            }

            Event::Resize(..) | Event::Paste(_) => self.clear(),
            _ => {}
        }

        Ok(false)
    }

    fn bounds(&self) -> Option<((u16, usize), (u16, usize))> {
        let (a, b) = (self.anchor?, self.end?);

        Some(if (a.1, a.0) <= (b.1, b.0) {
            (a, b)
        } else {
            (b, a)
        })
    }

    fn selected_columns(&self, row: usize) -> Option<(u16, u16)> {
        if !self.moved {
            return None;
        }

        let (start, end) = self.bounds()?;

        if row < start.1 || row > end.1 {
            return None;
        }

        Some((
            if row == start.1 { start.0 } else { 0 },
            if row == end.1 {
                end.0
            } else {
                self.area().width.saturating_sub(1)
            },
        ))
    }

    fn capture(&mut self, buffer: &Buffer) {
        if !self.active() {
            return;
        }

        let area = self.area().intersection(buffer.area);

        for y in area.y..area.bottom() {
            let row = (y - area.y) as usize + self.base();
            self.rows.entry(row).or_insert_with(|| {
                (area.x..area.right())
                    .map(|x| buffer[(x, y)].clone())
                    .collect()
            });
        }
    }

    /// Fill rows skipped by a wheel tick, including when the viewport is shorter
    /// than the scroll step. Copying must include the entire selected interval.
    pub(super) fn transcript_view(
        &mut self,
        offset: usize,
        layout: &super::transcript::TranscriptLayout,
    ) {
        self.offset = offset;
        if !self.active() {
            self.decorations.clear();
        }
        for row in offset
            ..offset
                .saturating_add(usize::from(self.viewport.height))
                .min(layout.height())
        {
            self.decorations
                .entry(row)
                .or_insert_with(|| layout.selection_metadata(row).cloned().unwrap_or_default());
        }

        if self.region != Some(Region::Transcript) {
            return;
        }

        if self.extend {
            if let Some(point) = self.pointer {
                self.end = Some(self.position(point));
            }

            self.extend = false;
        }

        let Some((start, end)) = self.bounds() else {
            return;
        };
        if self.filled == Some((start.1, end.1)) {
            return;
        }

        for row in start.1..=end.1 {
            self.decorations
                .entry(row)
                .or_insert_with(|| layout.selection_metadata(row).cloned().unwrap_or_default());
            self.rows.entry(row).or_insert_with(|| {
                let area = Rect::new(0, 0, self.viewport.width, 1);
                let mut buffer = Buffer::empty(area);
                layout.draw_buffer(&mut buffer, area, row);
                buffer.content
            });
        }

        self.filled = Some((start.1, end.1));
    }

    pub fn text(&self) -> String {
        let mut lines = Vec::new();

        for (&row, cells) in &self.rows {
            let Some((left, right)) = self.selected_columns(row) else {
                continue;
            };
            let metadata = self.row_decorations(row);
            if metadata.is_some_and(|metadata| metadata.chrome) {
                continue;
            }
            let decorations = metadata.map_or(&[][..], |metadata| metadata.ranges.as_slice());

            let mut text = String::new();
            let mut x = 0;

            while x < cells.len() {
                let width = cells[x].symbol().width().max(1);

                if x <= right as usize && x + width > left as usize {
                    if decorations
                        .iter()
                        .any(|range| range.start < x + width && range.end > x)
                    {
                        // Remove gutters, but keep interior columns separated (e.g. tables).
                        // Trailing padding is trimmed with the rest of the terminal row.
                        if !text.is_empty() {
                            text.extend(std::iter::repeat_n(' ', width));
                        }
                    } else {
                        text.push_str(cells[x].symbol());
                    }
                }

                x += width;
            }

            lines.push(text.trim_end().to_owned());
        }

        lines.join("\n")
    }

    fn row_decorations(&self, row: usize) -> Option<&super::transcript::Decorations> {
        (self.region == Some(Region::Transcript))
            .then(|| self.decorations.get(&row))
            .flatten()
    }

    pub fn draw_frozen(&mut self, frame: &mut Frame<'_>) -> bool {
        let Some(buffer) = &self.frozen else {
            return false;
        };
        if buffer.area != frame.area() {
            return false;
        }

        *frame.buffer_mut() = buffer.clone();
        self.paint(frame);

        true
    }

    pub fn paint(&mut self, frame: &mut Frame<'_>) {
        let snapshot = frame.buffer_mut().clone();

        self.capture(&snapshot);

        if self.frozen.is_some() {
            self.frozen = Some(snapshot.clone());
        }

        self.latest = Some(snapshot);
        let area = self.area().intersection(frame.area());
        let mut styles = HashMap::new();

        for y in area.y..area.bottom() {
            let row = (y - area.y) as usize + self.base();
            let Some((left, right)) = self.selected_columns(row) else {
                continue;
            };
            let Some(cells) = self.rows.get(&row) else {
                continue;
            };

            let decorations = self
                .row_decorations(row)
                .map_or(&[][..], |metadata| metadata.ranges.as_slice());
            let is_content = |index: usize| {
                !cells[index].symbol().trim().is_empty()
                    && !decorations.iter().any(|range| range.contains(&index))
            };
            let Some(start) = (0..cells.len()).find(|&index| is_content(index)) else {
                continue;
            };
            let last = (start..cells.len())
                .rfind(|&index| is_content(index))
                .unwrap();
            let end = last + cells[last].symbol().width().max(1);

            let mut x = 0;

            while x < cells.len().min(area.width as usize) {
                let width = cells[x].symbol().width().max(1);

                if x >= start
                    && x < end
                    && x <= right as usize
                    && x + width > left as usize
                    && !decorations
                        .iter()
                        .any(|range| range.start < x + width && range.end > x)
                {
                    let style = *styles
                        .entry(cells[x].style())
                        .or_insert_with_key(|style| super::theme::selection_style(*style));
                    for (column, cell) in cells
                        .iter()
                        .enumerate()
                        .take((x + width).min(area.width as usize))
                        .skip(x)
                    {
                        frame.buffer_mut()[(area.x + column as u16, y)] = cell.clone();
                        frame.buffer_mut()[(area.x + column as u16, y)].set_style(style);
                    }
                }

                x += width;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::{
        app::{TranscriptItem, UiState},
        render,
    };
    use crossterm::event::MouseEvent;
    use ratatui::{Terminal, backend::TestBackend};

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })
    }
    fn draw(state: &mut UiState, terminal: &mut Terminal<TestBackend>) {
        terminal
            .draw(|frame| {
                render::draw(frame, state);
                state.selection.paint(frame);
            })
            .unwrap();
    }
    fn setup() -> (UiState, Terminal<TestBackend>) {
        let mut state = UiState::new("footer-model");
        state.transcript.push(TranscriptItem::Assistant(
            (0..40).map(|i| format!("line{i:02}\n")).collect(),
        ));
        state.input.set("你好 draft".into());
        state.follow_bottom = false;
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        draw(&mut state, &mut terminal);
        state.scroll = state.max_scroll;
        draw(&mut state, &mut terminal);

        (state, terminal)
    }

    fn select_transcript(items: &[TranscriptItem], width: u16, expanded: &[usize]) -> Selection {
        let mut layout = super::super::transcript::TranscriptLayout::default();
        layout.update_expanded(
            items,
            &expanded.iter().copied().collect(),
            &Default::default(),
            width,
        );
        let mut selection = Selection {
            viewport: Rect::new(3, 2, width, layout.height() as u16),
            region: Some(Region::Transcript),
            anchor: Some((0, 0)),
            end: Some((width - 1, layout.height() - 1)),
            moved: true,
            ..Default::default()
        };
        selection.transcript_view(0, &layout);
        selection
    }

    #[test]
    fn copying_code_omits_generated_frame_but_keeps_source_indentation_and_blank_lines() {
        let source = "fn main() {\n    let glyphs = \"│─╭╮┊\";\n\n    println!(\"你好 👩‍💻\");\n}";
        let mut selection = select_transcript(
            &[TranscriptItem::Assistant(format!("```rust\n{source}\n```"))],
            80,
            &[],
        );
        assert_eq!(selection.text().trim_end_matches('\n'), source);

        // Selecting just the language badge and top border copies nothing.
        selection.end = Some((79, 0));
        assert_eq!(selection.text(), "");
    }

    #[test]
    fn copying_thinking_and_nested_code_omits_both_rails_even_after_wrapping() {
        let source = "先检查你好 👩‍💻\n\n```text\n    保留缩进与正文 │─\n\n最后一行\n```\n检查完成";
        for width in [16, 80] {
            let selection = select_transcript(
                &[TranscriptItem::Thinking(source.to_owned().into())],
                width,
                &[],
            );
            let copied = selection.text();
            assert_eq!(
                copied.replace(['\n', ' '], ""),
                "先检查你好👩‍💻保留缩进与正文│─最后一行检查完成"
            );
            if width == 80 {
                assert_eq!(
                    copied.trim_end_matches('\n'),
                    "先检查你好 👩‍💻\n\n    保留缩进与正文 │─\n\n最后一行\n检查完成"
                );
            }
            assert!(copied.contains("    保留"));
            assert!(!copied.contains(['┊', '╭', '╰']));
            assert!(copied.contains("👩‍💻\n\n"));
            assert!(copied.contains("\n\n最后一行"));
        }
    }

    #[test]
    fn partial_and_reversed_copy_include_whole_wide_glyphs_without_the_code_rail() {
        let mut selection = select_transcript(
            &[TranscriptItem::Assistant("```text\n你好 👩‍💻 │─\n```".into())],
            40,
            &[],
        );
        // Both endpoints touch a half of a wide Chinese glyph.
        for (anchor, end) in [((5, 1), (7, 1)), ((7, 1), (5, 1))] {
            selection.anchor = Some(anchor);
            selection.end = Some(end);
            assert_eq!(selection.text(), "你好");
        }
        selection.anchor = Some((0, 1));
        selection.end = Some((7, 1));
        assert_eq!(selection.text(), "你好");
    }

    #[test]
    fn copying_message_and_tool_frames_keeps_literal_borders_and_body_blank_lines() {
        use super::super::app::{ToolRow, ToolStatus};
        use serde_json::json;

        let source = "    first │─╭╮\n\n    second 你好";
        let user = select_transcript(&[TranscriptItem::User(source.into())], 60, &[]);
        assert_eq!(user.text().trim_end_matches('\n'), source);

        let tool = select_transcript(
            &[TranscriptItem::Tool(ToolRow {
                id: "read".into(),
                name: "read_file".into(),
                arguments: json!({"path": "example.txt"}).to_string(),
                status: ToolStatus::Finished,
                output: json!({"path": "example.txt", "content": "1:     first │─╭╮\n2: \n3:     second 你好\n"}).to_string(),
            })],
            60,
            &[0],
        );
        let copied = tool.text();
        assert!(copied.starts_with("read_file example.txt"), "{copied:?}");
        assert!(copied.contains(source), "{copied:?}");
        assert!(!copied.contains('╰'));
    }

    #[test]
    fn copying_tables_omits_grid_rows_and_keeps_columns_separate() {
        let selection = select_transcript(
            &[TranscriptItem::Assistant(
                "| 名称 | 值 |\n|---|---|\n| Orbit | 你好 |".into(),
            )],
            60,
            &[],
        );
        assert_eq!(
            selection
                .text()
                .trim_end_matches('\n')
                .lines()
                .map(|line| line.split_whitespace().collect::<Vec<_>>())
                .collect::<Vec<_>>(),
            [vec!["名称", "值"], vec!["Orbit", "你好"]]
        );
    }

    #[test]
    fn selected_markdown_and_code_keep_colors_and_wide_glyphs_through_redraws() {
        use ratatui::style::{Color, Modifier};

        let mut state = UiState::new("orbit-model");
        state.transcript.push(TranscriptItem::Assistant(
            "Orbit **加粗** _斜体_ [文档](https://orbit.example/docs) 👩‍💻\n\n```python\n# Orbit\nprint('Orbit')\n```".into(),
        ));
        state.follow_bottom = false;
        let mut terminal = Terminal::new(TestBackend::new(70, 24)).unwrap();
        draw(&mut state, &mut terminal);
        let before = terminal.backend().buffer().clone();
        let area = state.selection.viewport;
        state
            .selection
            .handle(&mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x,
                area.y,
            ))
            .unwrap();
        state
            .selection
            .handle(&mouse(
                MouseEventKind::Drag(MouseButton::Left),
                area.right() - 1,
                area.bottom() - 1,
            ))
            .unwrap();
        let copied = state.selection.text();
        assert!(copied.contains("加粗"));
        assert!(copied.contains("👩‍💻"));
        assert!(copied.contains("print('Orbit')"));

        for _ in 0..3 {
            let mut painted = None;
            terminal
                .draw(|frame| {
                    assert!(state.selection.draw_frozen(frame));
                    painted = Some(frame.buffer_mut().clone());
                })
                .unwrap();
            let selected = painted.unwrap();
            let mut colored = 0;
            let mut wide = 0;

            for y in area.y..area.bottom() {
                let mut x = area.x;
                while x < area.right() {
                    let original = &before[(x, y)];
                    let cell = &selected[(x, y)];
                    let width = original.symbol().width().max(1) as u16;
                    assert_eq!(cell.symbol(), original.symbol());
                    assert_eq!(terminal.backend().buffer()[(x, y)].bg, cell.bg);
                    if cell.bg == super::super::theme::THEME.selection {
                        assert_eq!(
                            cell.modifier,
                            original.modifier - (Modifier::REVERSED | Modifier::DIM)
                        );
                        if let Color::Rgb(red, green, blue) = original.fg
                            && let Color::Rgb(selected_red, selected_green, selected_blue) = cell.fg
                            && red.max(green).max(blue) - red.min(green).min(blue) > 8
                        {
                            assert_eq!(red.cmp(&green), selected_red.cmp(&selected_green));
                            assert_eq!(red.cmp(&blue), selected_red.cmp(&selected_blue));
                            assert_eq!(green.cmp(&blue), selected_green.cmp(&selected_blue));
                            colored += 1;
                        }
                        for column in x + 1..(x + width).min(area.right()) {
                            assert_eq!(selected[(column, y)].bg, cell.bg);
                            assert_eq!(selected[(column, y)].fg, cell.fg);
                            assert_eq!(
                                selected[(column, y)].symbol(),
                                before[(column, y)].symbol()
                            );
                            wide += 1;
                        }
                    } else {
                        assert_eq!(cell, original);
                    }
                    x += width;
                }
            }
            assert!(colored > 10);
            assert!(wide >= 7);
            assert_eq!(state.selection.text(), copied);
        }

        state.selection.clear();
        draw(&mut state, &mut terminal);
        assert_eq!(terminal.backend().buffer(), &before);
    }

    #[test]
    fn selection_leaves_frames_table_grid_and_padding_unchanged() {
        use super::super::app::{ThinkingRow, ToolRow, ToolStatus};
        use super::super::theme::THEME;
        use serde_json::json;

        let source = "| 项 | 值 |\n|---|---|\n| Orbit | **重点** 与 `code` |\n\n```rust\nlet rail = \"│─\";\n```";
        for width in [28, 72, 100] {
            let mut state = UiState::new("orbit-model");
            state
                .transcript
                .push(TranscriptItem::User("Orbit 框选 👩‍💻".into()));
            state
                .transcript
                .push(TranscriptItem::Assistant(format!("{source}\n\n---\n")));
            state.transcript.push(TranscriptItem::Tool(ToolRow {
                id: "orbit-read".into(), name: "read_file".into(),
                arguments: json!({"path": "ORBIT.md"}).to_string(), status: ToolStatus::Finished,
                output: json!({"path":"ORBIT.md", "content":source.lines().enumerate().map(|(index, line)| format!("{}: {line}\n", index + 1)).collect::<String>()}).to_string(),
            }));
            let mut thinking = ThinkingRow::from("```rust\nlet rail = \"│─\";\n```".to_owned());
            thinking.finished = true;
            state.transcript.push(TranscriptItem::Thinking(thinking));
            state.expanded.extend([2, 3]);
            state.follow_bottom = false;
            let mut terminal = Terminal::new(TestBackend::new(width, 70)).unwrap();
            draw(&mut state, &mut terminal);
            state.scroll = state.max_scroll;
            draw(&mut state, &mut terminal);
            let before = terminal.backend().buffer().clone();
            let area = state.selection.viewport;
            state
                .selection
                .handle(&mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    area.x,
                    area.y,
                ))
                .unwrap();
            state
                .selection
                .handle(&mouse(
                    MouseEventKind::Drag(MouseButton::Left),
                    area.right() - 1,
                    area.bottom() - 1,
                ))
                .unwrap();
            terminal
                .draw(|frame| {
                    assert!(state.selection.draw_frozen(frame));
                })
                .unwrap();
            let after = terminal.backend().buffer();
            let mut borders = 0;
            let mut literal_rails = 0;
            let mut selected = 0;
            let mut padding = 0;
            let mut badges = 0;
            for y in area.y..area.bottom() {
                let mut has_text = false;
                for x in area.x..area.right() {
                    let cell = &before[(x, y)];
                    let painted = &after[(x, y)];
                    let literal =
                        cell.symbol() == "│" && x > area.x && before[(x - 1, y)].symbol() == "\""
                            || cell.symbol() == "─"
                                && x + 1 < area.right()
                                && before[(x + 1, y)].symbol() == "\"";
                    if literal {
                        assert_eq!(painted.bg, THEME.selection);
                        literal_rails += 1;
                    } else if cell
                        .symbol()
                        .chars()
                        .any(|ch| "│─╭╮╰╯┌┐└┘├┤┼┬┴┊▎".contains(ch))
                    {
                        assert_eq!(
                            painted, cell,
                            "border changed at {x},{y} with width {width}"
                        );
                        borders += 1;
                    }
                    if !cell.symbol().trim().is_empty() {
                        has_text = true;
                    }
                    if painted.bg != cell.bg {
                        assert_eq!(painted.bg, THEME.selection);
                        selected += 1;
                    }
                }
                for x in area.x + 1..area.right().saturating_sub(4) {
                    if ["r", "u", "s", "t"]
                        .iter()
                        .enumerate()
                        .all(|(i, text)| before[(x + i as u16, y)].symbol() == *text)
                        && before[(x, y)].bg != ratatui::style::Color::Reset
                    {
                        for column in x - 1..x + 5 {
                            assert_eq!(after[(column, y)], before[(column, y)]);
                        }
                        badges += 1;
                    }
                }
                for x in area.x..area.right().saturating_sub(5) {
                    if ["O", "r", "b", "i", "t"]
                        .iter()
                        .enumerate()
                        .all(|(i, text)| before[(x + i as u16, y)].symbol() == *text)
                        && let Some(border) = (x + 5..area.right())
                            .find(|&column| before[(column, y)].symbol() == "│")
                        && (x + 5..border)
                            .all(|column| before[(column, y)].symbol().trim().is_empty())
                    {
                        for column in x + 5..border {
                            assert_eq!(after[(column, y)], before[(column, y)]);
                            padding += 1;
                        }
                    }
                }
                if !has_text {
                    for x in area.x..area.right() {
                        assert_eq!(after[(x, y)], before[(x, y)]);
                    }
                }
                let tail = area.right() - 1;
                if before[(tail, y)].symbol().trim().is_empty()
                    && before[(tail - 1, y)].symbol().width() <= 1
                {
                    assert_eq!(after[(tail, y)], before[(tail, y)]);
                }
            }
            assert!(borders > 50);
            assert!(literal_rails >= 2);
            assert!(selected > 20);
            assert!(padding > 0);
            assert!(badges >= 2);
            let copied = state.selection.text();
            assert!(copied.contains("│─"));
            assert!(!copied.contains(['╭', '╮', '╰', '╯', '┌', '┐', '└', '┘', '┊']));
            state.selection.clear();
            draw(&mut state, &mut terminal);
            assert_eq!(terminal.backend().buffer(), &before);
        }
    }

    #[test]
    fn press_does_not_select_until_pointer_moves() {
        let (mut state, mut terminal) = setup();
        let area = state.selection.viewport;
        let point = (area.x + 2, area.y);
        let before = terminal.backend().buffer().clone();
        state
            .selection
            .handle(&mouse(
                MouseEventKind::Down(MouseButton::Left),
                point.0,
                point.1,
            ))
            .unwrap();
        terminal
            .draw(|frame| assert!(state.selection.draw_frozen(frame)))
            .unwrap();

        assert_eq!(terminal.backend().buffer(), &before);
        assert!(state.selection.text().is_empty());
        assert_eq!(state.selection.auto_scroll(Instant::now()), 0);

        let release = mouse(MouseEventKind::Up(MouseButton::Left), point.0, point.1);
        assert_eq!(state.selection.click(&release), Some(point));
        state.selection.handle(&release).unwrap();
        assert!(!state.selection.active());

        state
            .selection
            .handle(&mouse(
                MouseEventKind::Down(MouseButton::Left),
                point.0,
                point.1,
            ))
            .unwrap();
        state
            .selection
            .handle(&mouse(
                MouseEventKind::Drag(MouseButton::Left),
                point.0 + 3,
                point.1,
            ))
            .unwrap();
        terminal
            .draw(|frame| assert!(state.selection.draw_frozen(frame)))
            .unwrap();

        assert_eq!(
            terminal.backend().buffer()[point].bg,
            crate::tui::theme::THEME.selection
        );
        assert!(!state.selection.text().is_empty());
        assert!(state.selection.click(&release).is_none());
    }

    #[test]
    fn dismissing_a_selection_keeps_blocks_open_and_the_next_content_click_folds_them() {
        use crate::tui::app::{ThinkingRow, ToolRow, ToolStatus};

        for kind in ["tool", "group", "thinking"] {
            let mut state = UiState::new("orbit-model");
            let tool = ToolRow {
                id: "orbit-tool".into(),
                name: "run_command".into(),
                arguments: serde_json::json!({ "command": "orbit --version" }).to_string(),
                status: ToolStatus::Finished,
                output: "Orbit output\nsecond line".into(),
            };
            if kind == "thinking" {
                let mut thinking = ThinkingRow::from(tool.output);
                thinking.finished = true;
                state.transcript.push(TranscriptItem::Thinking(thinking));
            } else {
                state.transcript.push(TranscriptItem::Tool(tool.clone()));
                if kind == "group" {
                    state.transcript.push(TranscriptItem::Tool(ToolRow {
                        id: "orbit-other".into(),
                        output: "Orbit other output".into(),
                        ..tool
                    }));
                    state.expanded_tool_groups.insert(0);
                }
            }
            state.expanded.insert(0);
            state.follow_bottom = false;
            let mut terminal = Terminal::new(TestBackend::new(70, 24)).unwrap();
            draw(&mut state, &mut terminal);
            let area = state.selection.viewport;
            let output_y = (area.top()..area.bottom())
                .find(|&y| {
                    (area.left()..area.right())
                        .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                        .collect::<String>()
                        .contains("Orbit output")
                })
                .unwrap();
            let point = (area.x + 4, output_y);

            state
                .selection
                .handle(&mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    point.0,
                    point.1,
                ))
                .unwrap();
            state
                .selection
                .handle(&mouse(
                    MouseEventKind::Drag(MouseButton::Left),
                    point.0 + 8,
                    point.1,
                ))
                .unwrap();
            let release = mouse(MouseEventKind::Up(MouseButton::Left), point.0 + 8, point.1);
            assert!(state.selection.click(&release).is_none());
            state.selection.handle(&release).unwrap();
            draw(&mut state, &mut terminal);
            assert!(!state.selection.text().is_empty());

            let release = mouse(MouseEventKind::Up(MouseButton::Left), point.0, point.1);
            state
                .selection
                .handle(&mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    point.0,
                    point.1,
                ))
                .unwrap();
            if let Some(point) = state.selection.click(&release) {
                super::super::toggle_transcript_at(&mut state, point);
            }
            state.selection.handle(&release).unwrap();
            draw(&mut state, &mut terminal);
            assert!(
                state.expanded.contains(&0),
                "the dismissal click must not fold the tool"
            );
            assert!(!state.selection.active());

            state
                .selection
                .handle(&mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    point.0,
                    point.1,
                ))
                .unwrap();
            let point = state.selection.click(&release).unwrap();
            assert!(super::super::toggle_transcript_at(&mut state, point));
            assert!(!state.expanded.contains(&0));
            assert_eq!(state.expanded_tool_groups.contains(&0), kind == "group");
        }
    }

    #[test]
    fn replacing_a_selection_by_drag_and_clicking_the_input_remain_immediate() {
        let (mut state, mut terminal) = setup();
        let area = state.selection.viewport;
        for (start, end) in [(area.x + 2, area.x + 6), (area.x + 4, area.x + 8)] {
            for event in [
                mouse(MouseEventKind::Down(MouseButton::Left), start, area.y),
                mouse(MouseEventKind::Drag(MouseButton::Left), end, area.y),
                mouse(MouseEventKind::Up(MouseButton::Left), end, area.y),
            ] {
                assert!(state.selection.click(&event).is_none());
                state.selection.handle(&event).unwrap();
            }
            draw(&mut state, &mut terminal);
            assert!(state.selection.active());
            assert!(!state.selection.text().is_empty());
        }
        assert_eq!(state.selection.text(), "ne00");

        let input = state.input_area;
        let point = (input.x + 3, input.y);
        state
            .selection
            .handle(&mouse(
                MouseEventKind::Down(MouseButton::Left),
                point.0,
                point.1,
            ))
            .unwrap();
        let release = mouse(MouseEventKind::Up(MouseButton::Left), point.0, point.1);
        assert!(state.selection.click(&release).is_none());
        state.selection.handle(&release).unwrap();
        assert!(!state.selection.active());

        let release = mouse(MouseEventKind::Up(MouseButton::Left), area.x + 2, area.y);
        state
            .selection
            .handle(&mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x + 2,
                area.y,
            ))
            .unwrap();
        assert_eq!(state.selection.click(&release), Some((area.x + 2, area.y)));
    }

    #[test]
    fn held_wheel_extends_across_skipped_rows_and_release_stops_extension() {
        let (mut state, mut terminal) = setup();
        let area = state.selection.viewport;
        let (x, y) = (area.x + 2, area.y);
        state
            .selection
            .handle(&mouse(MouseEventKind::Down(MouseButton::Left), x, y))
            .unwrap();
        state
            .selection
            .handle(&mouse(MouseEventKind::ScrollDown, x + 5, y))
            .unwrap();

        assert!(state.selection.dragging());

        state.scroll_down(8);
        draw(&mut state, &mut terminal);

        assert_eq!(
            state.selection.text(),
            (0..=8)
                .map(|i| if i == 0 {
                    "line00".into()
                } else {
                    format!("  line{i:02}")
                })
                .collect::<Vec<_>>()
                .join("\n")
        );

        state
            .selection
            .handle(&mouse(MouseEventKind::Up(MouseButton::Left), x + 5, y))
            .unwrap();
        let text = state.selection.text();
        state
            .selection
            .handle(&mouse(MouseEventKind::ScrollDown, x, y))
            .unwrap();
        state.scroll_down(8);
        draw(&mut state, &mut terminal);

        assert_eq!(state.selection.text(), text);
        assert!(!state.selection.dragging());

        state
            .selection
            .handle(&mouse(MouseEventKind::ScrollUp, x, y))
            .unwrap();
        state.scroll_up(8);
        draw(&mut state, &mut terminal);

        assert_eq!(
            terminal.backend().buffer()[(x, y)].bg,
            crate::tui::theme::THEME.selection
        );
    }
    #[test]
    fn boundary_timer_repeats_without_motion_and_stops_on_release() {
        let (mut state, mut terminal) = setup();
        let area = state.selection.viewport;
        state
            .selection
            .handle(&mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x,
                area.y,
            ))
            .unwrap();
        state
            .selection
            .handle(&mouse(
                MouseEventKind::Drag(MouseButton::Left),
                area.x + 5,
                9,
            ))
            .unwrap();
        let now = Instant::now();

        for i in 0..3 {
            assert_eq!(
                state
                    .selection
                    .auto_scroll(now + Duration::from_millis(i * 100)),
                1
            );

            state.scroll_down(2);
            draw(&mut state, &mut terminal);
        }

        assert!(state.selection.text().contains("line06"));

        state
            .selection
            .handle(&mouse(MouseEventKind::Drag(MouseButton::Left), area.x, 0))
            .unwrap();

        assert_eq!(
            state
                .selection
                .auto_scroll(now + Duration::from_millis(400)),
            -1
        );

        state.scroll_up(2);
        draw(&mut state, &mut terminal);
        state
            .selection
            .handle(&mouse(MouseEventKind::Up(MouseButton::Left), area.x, 0))
            .unwrap();

        assert_eq!(state.selection.auto_scroll(now + Duration::from_secs(1)), 0);
    }
    #[test]
    fn selection_stays_in_starting_region_and_excludes_chrome() {
        let (mut state, mut terminal) = setup();

        for region in [Region::Transcript, Region::Footer] {
            state.selection.clear();
            draw(&mut state, &mut terminal);
            let area = match region {
                Region::Transcript => state.selection.viewport,
                _ => state.selection.footer,
            };

            state
                .selection
                .handle(&mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    area.x,
                    area.y,
                ))
                .unwrap();
            state
                .selection
                .handle(&mouse(MouseEventKind::Drag(MouseButton::Left), 39, 9))
                .unwrap();
            terminal
                .draw(|frame| assert!(state.selection.draw_frozen(frame)))
                .unwrap();

            for y in 0..10 {
                for x in 0..40 {
                    if !area.contains((x, y).into()) {
                        assert_ne!(
                            terminal.backend().buffer()[(x, y)].bg,
                            crate::tui::theme::THEME.selection
                        );
                    }
                }
            }

            let text = state.selection.text();

            match region {
                Region::Transcript => {
                    assert!(text.contains("line00"));
                    assert!(!text.contains("draft"));
                    assert!(!text.contains("footer-model"));
                    assert!(!text.contains("Orbit"));
                }
                Region::Footer => {
                    assert!(text.contains("/96k"));
                    assert!(!text.contains("draft"));
                }
                _ => unreachable!(),
            }
        }
    }
}
