use std::io::{self, Write};

use base64::{Engine, engine::general_purpose::STANDARD};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Default)]
pub struct Selection {
    pub area: Rect,
    latest: Option<Buffer>,
    frozen: Option<Buffer>,
    anchor: Option<(u16, u16)>,
    end: Option<(u16, u16)>,
    dragging: bool,
}

impl Selection {
    pub(super) fn clear(&mut self) {
        self.anchor = None;
        self.end = None;
        self.frozen = None;
        self.dragging = false;
    }

    pub fn handle(&mut self, event: &Event) -> io::Result<bool> {
        match event {
            Event::Mouse(mouse) => {
                let inside = self.area.contains((mouse.column, mouse.row).into());
                match mouse.kind {
                    MouseEventKind::Down(MouseButton::Left) => {
                        self.clear();
                        if inside {
                            self.anchor = Some((mouse.column, mouse.row));
                            self.end = self.anchor;
                            self.frozen = self.latest.clone();
                            self.dragging = true;
                        }
                        return Ok(true);
                    }
                    MouseEventKind::Drag(MouseButton::Left)
                    | MouseEventKind::Up(MouseButton::Left)
                        if self.dragging =>
                    {
                        self.end = Some((
                            mouse
                                .column
                                .clamp(self.area.x, self.area.right().saturating_sub(1)),
                            mouse
                                .row
                                .clamp(self.area.y, self.area.bottom().saturating_sub(1)),
                        ));
                        if matches!(mouse.kind, MouseEventKind::Up(_)) {
                            self.dragging = false;
                            if self.anchor == self.end {
                                self.clear();
                            }
                        }
                        return Ok(true);
                    }
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => self.clear(),
                    _ => {}
                }
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if self.anchor.is_some()
                    && key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    let text = self.text();
                    if !text.is_empty() {
                        // OSC 52 is understood by supporting local/remote terminals; no
                        // shell command or display-server dependency is needed.
                        write!(
                            io::stdout(),
                            "\x1b]52;c;{}\x07",
                            STANDARD.encode(text.as_bytes())
                        )?;
                        io::stdout().flush()?;
                    }
                    return Ok(true);
                }
                if self.anchor.is_some() && key.code == KeyCode::Esc {
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

    fn bounds(&self) -> Option<((u16, u16), (u16, u16))> {
        let a = self.anchor?;
        let b = self.end?;
        if (a.1, a.0) <= (b.1, b.0) {
            Some((a, b))
        } else {
            Some((b, a))
        }
    }

    fn cells(&self) -> Vec<(u16, u16)> {
        let Some((start, end)) = self.bounds() else {
            return Vec::new();
        };
        let Some(buffer) = &self.frozen else {
            return Vec::new();
        };
        let mut cells = Vec::new();
        for y in start.1..=end.1 {
            let left = if y == start.1 { start.0 } else { self.area.x };
            let right = if y == end.1 {
                end.0
            } else {
                self.area.right().saturating_sub(1)
            };
            let mut x = self.area.x;
            while x < self.area.right() {
                let width = buffer[(x, y)].symbol().width().max(1) as u16;
                if x <= right && x.saturating_add(width) > left {
                    cells.push((x, y));
                }
                x = x.saturating_add(width);
            }
        }
        cells
    }

    pub fn text(&self) -> String {
        let Some(buffer) = &self.frozen else {
            return String::new();
        };
        let mut lines = Vec::new();
        let mut line = String::new();
        let mut row = None;
        for (x, y) in self.cells() {
            if row.is_some_and(|old| old != y) {
                lines.push(line.trim_end().to_owned());
                line.clear();
            }
            row = Some(y);
            line.push_str(buffer[(x, y)].symbol());
        }
        if row.is_some() {
            lines.push(line.trim_end().to_owned());
        }
        lines.join("\n")
    }

    /// Redraw a drag directly from the unhighlighted viewport, without laying
    /// out growing conversation history or waiting for the stream render tick.
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

    pub fn restore_viewport(&self, frame: &mut Frame<'_>) -> bool {
        let Some(buffer) = &self.frozen else {
            return false;
        };
        if buffer.area != frame.area() {
            return false;
        }
        for y in self.area.y..self.area.bottom() {
            for x in self.area.x..self.area.right() {
                frame.buffer_mut()[(x, y)] = buffer[(x, y)].clone();
            }
        }
        true
    }

    pub fn paint(&mut self, frame: &mut Frame<'_>) {
        // Hold the selected viewport still while model events continue to accumulate.
        // Copying always uses this unhighlighted snapshot, never the changing stream.
        if let Some(buffer) = &self.frozen {
            if buffer.area != frame.area() {
                self.clear();
            } else {
                for y in self.area.y..self.area.bottom() {
                    for x in self.area.x..self.area.right() {
                        frame.buffer_mut()[(x, y)] = buffer[(x, y)].clone();
                    }
                }
            }
        }
        if self.frozen.is_none() {
            self.latest = Some(frame.buffer_mut().clone());
        }
        for (x, y) in self.cells() {
            let width = frame.buffer_mut()[(x, y)].symbol().width().max(1) as u16;
            for column in x..x.saturating_add(width).min(self.area.right()) {
                frame.buffer_mut()[(column, y)]
                    .set_style(Style::default().bg(Color::Blue).fg(Color::White));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::MouseEvent;
    use ratatui::{Terminal, backend::TestBackend, widgets::Paragraph};

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn direct_drag_redraw_uses_the_snapshot_and_preserves_copied_text() {
        let mut terminal = Terminal::new(TestBackend::new(30, 6)).unwrap();
        let mut selection = Selection {
            area: Rect::new(0, 0, 30, 3),
            ..Default::default()
        };
        terminal
            .draw(|frame| {
                frame.render_widget(Paragraph::new("abcdef"), selection.area);
                selection.paint(frame);
            })
            .unwrap();
        selection
            .handle(&mouse(MouseEventKind::Down(MouseButton::Left), 0, 0))
            .unwrap();
        selection
            .handle(&mouse(MouseEventKind::Drag(MouseButton::Left), 4, 0))
            .unwrap();
        terminal
            .draw(|frame| assert!(selection.draw_frozen(frame)))
            .unwrap();
        assert_eq!(selection.text(), "abcde");
        assert_eq!(terminal.backend().buffer()[(4, 0)].bg, Color::Blue);
        selection
            .handle(&mouse(MouseEventKind::Drag(MouseButton::Left), 2, 0))
            .unwrap();
        terminal
            .draw(|frame| assert!(selection.draw_frozen(frame)))
            .unwrap();
        assert_eq!(selection.text(), "abc");
        assert_ne!(terminal.backend().buffer()[(4, 0)].bg, Color::Blue);
    }

    #[test]
    fn reverse_drag_copies_wide_glyphs_and_freezes_only_output() {
        let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();
        let mut selection = Selection {
            area: Rect::new(0, 0, 20, 2),
            ..Default::default()
        };
        terminal
            .draw(|f| {
                f.render_widget(Paragraph::new("你好abc\nsecond"), selection.area);
                selection.paint(f);
            })
            .unwrap();
        selection
            .handle(&mouse(MouseEventKind::Down(MouseButton::Left), 5, 1))
            .unwrap();
        selection
            .handle(&mouse(MouseEventKind::Drag(MouseButton::Left), 1, 0))
            .unwrap();
        selection
            .handle(&mouse(MouseEventKind::Up(MouseButton::Left), 1, 0))
            .unwrap();
        assert_eq!(selection.text(), "你好abc\nsecond");
        terminal
            .draw(|f| {
                f.render_widget(Paragraph::new("new output"), selection.area);
                selection.paint(f);
            })
            .unwrap();
        assert_eq!(selection.text(), "你好abc\nsecond");
        selection
            .handle(&mouse(MouseEventKind::ScrollUp, 4, 1))
            .unwrap();
        assert!(selection.text().is_empty());
    }
}
