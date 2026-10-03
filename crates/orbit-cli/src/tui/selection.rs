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
    style::Style,
};
use std::{
    collections::BTreeMap,
    io::{self, Write},
    time::{Duration, Instant},
};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Region {
    Transcript,
    Input,
    Footer,
    Approval,
}

#[derive(Debug, Default)]
pub struct Selection {
    pub viewport: Rect,
    pub input: Rect,
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
    filled: Option<(usize, usize)>,
    latest: Option<Buffer>,
    frozen: Option<Buffer>,
    caret: Option<((u16, u16), Cell)>,
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

    pub fn reset_caret(&mut self) {
        self.caret = None;
    }

    pub fn set_caret(&mut self, position: (u16, u16), cell: Cell) {
        self.caret = Some((position, cell));
    }

    fn area(&self) -> Rect {
        match self.region {
            Some(Region::Transcript) => self.viewport,
            Some(Region::Input) => self.input,
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
                        (Region::Input, self.input),
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
                        write!(
                            io::stdout(),
                            "\x1b]52;c;{}\x07",
                            STANDARD.encode(text.as_bytes())
                        )?;
                        io::stdout().flush()?;
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

            let mut text = String::new();
            let mut x = 0;

            while x < cells.len() {
                let width = cells[x].symbol().width().max(1);

                if x <= right as usize && x + width > left as usize {
                    text.push_str(cells[x].symbol());
                }

                x += width;
            }

            lines.push(text.trim_end().to_owned());
        }

        lines.join("\n")
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
        let mut snapshot = frame.buffer_mut().clone();

        if let Some((position, cell)) = &self.caret
            && snapshot.area.contains((*position).into())
        {
            snapshot[*position] = cell.clone();
        }

        self.capture(&snapshot);

        if self.frozen.is_some() {
            self.frozen = Some(snapshot.clone());
        }

        self.latest = Some(snapshot);
        let area = self.area().intersection(frame.area());

        for y in area.y..area.bottom() {
            let row = (y - area.y) as usize + self.base();
            let Some((left, right)) = self.selected_columns(row) else {
                continue;
            };
            let Some(cells) = self.rows.get(&row) else {
                continue;
            };

            let mut x = 0;

            while x < cells.len().min(area.width as usize) {
                let width = cells[x].symbol().width().max(1);

                if x <= right as usize && x + width > left as usize {
                    for (column, cell) in cells
                        .iter()
                        .enumerate()
                        .take((x + width).min(area.width as usize))
                        .skip(x)
                    {
                        frame.buffer_mut()[(area.x + column as u16, y)] = cell.clone();
                        frame.buffer_mut()[(area.x + column as u16, y)].set_style(
                            Style::default()
                                .bg(crate::tui::theme::THEME.selection)
                                .fg(crate::tui::theme::THEME.white),
                        );
                    }
                }

                x += width;
            }
        }

        // Frozen snapshots omit the caret so copying only captures text.
        // Repaint it after selection, including when the selected range overlaps it.
        if let Some((position, _)) = &self.caret
            && frame.area().contains((*position).into())
        {
            frame.buffer_mut()[*position].set_style(
                Style::default()
                    .bg(crate::tui::theme::THEME.caret)
                    .fg(crate::tui::theme::THEME.on_accent),
            );
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
    fn dismissing_a_selection_keeps_the_tool_open_and_the_next_click_folds_it() {
        use crate::tui::app::{ToolRow, ToolStatus};

        let mut state = UiState::new("orbit-model");
        state.transcript.push(TranscriptItem::Tool(ToolRow {
            id: "orbit-tool".into(),
            name: "run_command".into(),
            arguments: serde_json::json!({ "command": "orbit --version" }).to_string(),
            status: ToolStatus::Finished,
            output: "Orbit output\nsecond line".into(),
        }));
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

        let input = state.selection.input;
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
        assert_eq!(state.selection.click(&release), Some(point));
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

        for region in [Region::Transcript, Region::Input, Region::Footer] {
            state.selection.clear();
            draw(&mut state, &mut terminal);
            let area = match region {
                Region::Transcript => state.selection.viewport,
                Region::Input => state.selection.input,
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
                Region::Input => assert_eq!(text, "你好 draft"),
                Region::Footer => {
                    assert!(text.contains("/96k"));
                    assert!(!text.contains("draft"));
                }
                _ => unreachable!(),
            }
        }
    }
    #[test]
    fn reverse_drag_preserves_wide_text_and_click_only_positions_on_release() {
        let (mut state, mut terminal) = setup();
        let area = state.selection.input;
        let down = mouse(MouseEventKind::Down(MouseButton::Left), area.x + 3, area.y);
        let up = mouse(MouseEventKind::Up(MouseButton::Left), area.x + 3, area.y);
        state.selection.handle(&down).unwrap();

        assert_eq!(state.selection.click(&up), Some((area.x + 3, area.y)));

        state
            .selection
            .handle(&mouse(
                MouseEventKind::Drag(MouseButton::Left),
                area.x,
                area.y,
            ))
            .unwrap();

        assert!(state.selection.click(&up).is_none());

        terminal
            .draw(|frame| assert!(state.selection.draw_frozen(frame)))
            .unwrap();

        assert_eq!(state.selection.text(), "你好");
    }
}
