use std::time::Instant;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use super::{
    app::UiState,
    icons,
    render::{fit_label, safe_output},
    theme::THEME,
};

pub(super) enum Action {
    None,
    Redraw,
    Close,
    Link(String),
}

#[derive(Debug)]
pub(super) struct TranscriptDialog {
    pub ui: UiState,
    area: Rect,
    close_area: Rect,
    close_hover: Option<Instant>,
    close_hover_tick: u128,
    close_pressed: bool,
}

impl TranscriptDialog {
    pub fn new(model: impl Into<String>) -> Self {
        let mut ui = UiState::new(model);
        ui.subagent_parent = Some(String::new());
        Self {
            ui,
            area: Rect::default(),
            close_area: Rect::default(),
            close_hover: None,
            close_hover_tick: 0,
            close_pressed: false,
        }
    }

    pub fn tick(&mut self) -> bool {
        let direction = self.ui.selection.auto_scroll(Instant::now());
        if direction < 0 {
            self.ui.scroll_up(2);
        } else if direction > 0 {
            self.ui.scroll_down(2);
        }
        let tick = self
            .close_hover
            .map_or(0, |since| since.elapsed().as_millis().min(240) / 40);
        let hover_changed = tick != self.close_hover_tick;
        self.close_hover_tick = tick;
        direction != 0 || hover_changed
    }

    pub fn draw(
        &mut self,
        title: &str,
        metadata: &str,
        error: Option<&str>,
        frame: &mut Frame<'_>,
        area: Rect,
        inner: Rect,
    ) {
        self.area = area;
        let header = Rect::new(
            inner.x + 1,
            inner.y,
            inner.width.saturating_sub(2),
            2.min(inner.height),
        );
        let close_width = (icons::CLOSE_THICK.width() + 2).min(usize::from(header.width)) as u16;
        self.close_area = Rect::new(
            header.right().saturating_sub(close_width),
            header.y,
            close_width,
            1.min(header.height),
        );
        let title = fit_label(
            title,
            usize::from(header.width.saturating_sub(close_width + 1)),
            false,
        );
        frame.render_widget(
            Paragraph::new(Line::styled(
                title,
                Style::default()
                    .fg(THEME.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            Rect {
                width: header.width.saturating_sub(close_width + 1),
                height: 1.min(header.height),
                ..header
            },
        );
        frame.render_widget(
            Paragraph::new(icons::CLOSE_THICK)
                .alignment(Alignment::Center)
                .style(super::theme::control_style(
                    self.close_hover.map(|since| since.elapsed()),
                )),
            self.close_area,
        );
        if header.height > 1 {
            frame.render_widget(
                Paragraph::new(fit_label(
                    &safe_output(metadata),
                    header.width as usize,
                    false,
                ))
                .style(Style::default().fg(THEME.tool_detail)),
                Rect {
                    y: header.y + 1,
                    height: 1,
                    ..header
                },
            );
        }
        let body = Rect::new(
            header.x,
            header.bottom().saturating_add(1).min(inner.bottom()),
            header.width,
            inner.height.saturating_sub(3),
        );
        if let Some(error) = error {
            frame.render_widget(
                Paragraph::new(safe_output(error))
                    .wrap(ratatui::widgets::Wrap { trim: false })
                    .style(Style::default().fg(THEME.tool_failed)),
                body,
            );
            self.ui.selection.viewport = Rect::default();
        } else {
            super::render::draw_transcript_in(frame, &mut self.ui, body, false);
            self.ui.selection.paint(frame);
        }
    }

    pub fn handle(&mut self, event: &Event) -> std::io::Result<Action> {
        if let Event::Key(key) = event
            && key.kind != KeyEventKind::Release
            && (matches!(key.code, KeyCode::Esc | KeyCode::Enter)
                || (key.code == KeyCode::Char('c')
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                    && !self.ui.selection.active()))
        {
            return Ok(Action::Close);
        }
        if let Event::Mouse(mouse) = event {
            let point = (mouse.column, mouse.row).into();
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                if !self.area.contains(point) {
                    return Ok(Action::Close);
                }
                self.close_pressed = self.close_area.contains(point);
                if self.close_pressed {
                    self.close_hover.get_or_insert_with(Instant::now);
                    return Ok(Action::Redraw);
                }
                if mouse.modifiers.contains(KeyModifiers::CONTROL) {
                    let offset = self.ui.max_scroll.saturating_sub(self.ui.scroll);
                    if let Some(url) = self.ui.transcript_layout.link_at(
                        self.ui.selection.viewport,
                        offset,
                        (mouse.column, mouse.row),
                    ) {
                        return Ok(Action::Link(url.into()));
                    }
                }
            }
            if mouse.kind == MouseEventKind::Up(MouseButton::Left) && self.close_pressed {
                self.close_pressed = false;
                return Ok(if self.close_area.contains(point) {
                    Action::Close
                } else {
                    Action::Redraw
                });
            }
            if matches!(mouse.kind, MouseEventKind::Drag(_)) {
                self.close_pressed = false;
            }
            if mouse.kind == MouseEventKind::Moved {
                let hovered = self.close_area.contains(point);
                let changed = self.close_hover.is_some() != hovered;
                if changed {
                    self.close_hover = hovered.then(Instant::now);
                    self.close_hover_tick = 0;
                }
                let links = super::render::handle_link_hover(&mut self.ui, event).unwrap_or(false);
                let disclosure =
                    super::render::handle_transcript_hover(&mut self.ui, event).unwrap_or(false);
                return Ok(if changed || links || disclosure {
                    Action::Redraw
                } else {
                    Action::None
                });
            }
        }
        if let Some(point) = self.ui.selection.click(event)
            && super::toggle_transcript_at(&mut self.ui, point)
        {
            self.ui.selection.clear();
            return Ok(Action::Redraw);
        }
        if self.ui.selection.handle(event)? {
            if self.ui.selection.transcript_drag() {
                self.ui.follow_bottom = false;
            }
            return Ok(Action::Redraw);
        }
        let page = usize::from(self.ui.selection.viewport.height).max(1);
        match event {
            Event::Mouse(mouse) if self.area.contains((mouse.column, mouse.row).into()) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.ui.scroll_up(super::wheel::LINES),
                    MouseEventKind::ScrollDown => self.ui.scroll_down(super::wheel::LINES),
                    _ => return Ok(Action::None),
                }
            }
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Up => self.ui.scroll_up(1),
                KeyCode::Down => self.ui.scroll_down(1),
                KeyCode::PageUp => self.ui.scroll_up(page),
                KeyCode::PageDown => self.ui.scroll_down(page),
                KeyCode::Home => self.ui.scroll_up(self.ui.max_scroll),
                KeyCode::End => self.ui.scroll_down(self.ui.max_scroll),
                _ => return Ok(Action::None),
            },
            Event::Resize(_, _) => {}
            _ => return Ok(Action::None),
        }
        Ok(Action::Redraw)
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::MouseEvent;
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, widgets::Block};

    use super::*;

    fn render(view: &mut TranscriptDialog, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                let area = super::super::dialogs::centered_area(frame.area());
                let block = Block::bordered();
                let inner = block.inner(area);
                frame.render_widget(block, area);
                view.draw("Orbit 子代理", "Orbit 模型", None, frame, area, inner);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn close_button_uses_tab_icon_and_highlights_its_whole_click_target() {
        let mut view = TranscriptDialog::new("orbit-model");
        let plain = render(&mut view, 100, 30);
        let button = view.close_area;
        assert_eq!(button.width, 3);
        assert_eq!(plain[(button.x + 1, button.y)].symbol(), icons::CLOSE_THICK);

        for x in button.x..button.right() {
            assert!(matches!(
                view.handle(&mouse(MouseEventKind::Moved, x, button.y))
                    .unwrap(),
                Action::Redraw | Action::None
            ));
            view.close_hover = Some(Instant::now() - std::time::Duration::from_millis(240));
            let hovered = render(&mut view, 100, 30);
            let style =
                super::super::theme::control_style(Some(std::time::Duration::from_millis(240)));
            for column in button.x..button.right() {
                assert_eq!(hovered[(column, button.y)].bg, style.bg.unwrap());
                assert_eq!(hovered[(column, button.y)].fg, style.fg.unwrap());
            }
            assert_eq!(
                hovered[(view.area.right() - 1, button.y)],
                plain[(view.area.right() - 1, button.y)]
            );
            assert!(matches!(
                view.handle(&mouse(MouseEventKind::Moved, button.x - 1, button.y))
                    .unwrap(),
                Action::Redraw
            ));
            assert_eq!(render(&mut view, 100, 30), plain);
        }
    }

    #[test]
    fn close_button_padding_is_clickable_but_dragging_off_does_not_close() {
        let mut view = TranscriptDialog::new("orbit-model");
        render(&mut view, 100, 30);
        let button = view.close_area;
        for x in button.x..button.right() {
            assert!(matches!(
                view.handle(&mouse(MouseEventKind::Down(MouseButton::Left), x, button.y))
                    .unwrap(),
                Action::Redraw
            ));
            assert!(matches!(
                view.handle(&mouse(MouseEventKind::Up(MouseButton::Left), x, button.y))
                    .unwrap(),
                Action::Close
            ));
        }
        view.handle(&mouse(
            MouseEventKind::Down(MouseButton::Left),
            button.x,
            button.y,
        ))
        .unwrap();
        assert!(matches!(
            view.handle(&mouse(
                MouseEventKind::Up(MouseButton::Left),
                button.x - 1,
                button.y
            ))
            .unwrap(),
            Action::Redraw
        ));
    }

    #[test]
    fn close_button_stays_inside_the_dialog_on_tiny_screens() {
        for (width, height) in [(10, 8), (9, 4), (8, 4), (7, 3), (1, 1)] {
            let mut view = TranscriptDialog::new("orbit-model");
            render(&mut view, width, height);
            if !view.close_area.is_empty() {
                assert_eq!(view.close_area.intersection(view.area), view.close_area);
            }
        }
    }
}
