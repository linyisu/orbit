use std::time::Instant;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
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
    Submit(String),
}

#[derive(Debug)]
pub(super) struct TranscriptDialog {
    pub ui: UiState,
    area: Rect,
    close_area: Rect,
    close_hover: Option<Instant>,
    close_hover_tick: u128,
    close_pressed: bool,
    input_enabled: bool,
    input_focus: bool,
    submit_blocked: bool,
    input_message: Option<String>,
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
            input_enabled: false,
            input_focus: true,
            submit_blocked: false,
            input_message: None,
        }
    }

    pub fn enable_input(&mut self) {
        self.input_enabled = true;
    }

    pub fn block_submission(&mut self, blocked: bool) {
        self.submit_blocked = blocked;
    }

    pub fn submitted(&mut self) {
        self.ui.input.take();
        self.ui.selection.clear();
        self.ui.follow_bottom = true;
        self.ui.scroll = 0;
        self.input_message = None;
    }

    pub fn submit_error(&mut self, error: impl ToString) {
        self.input_message = Some(error.to_string());
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
        let focus_expired = self.ui.expire_footnote_focus();
        let preview_changed = self
            .ui
            .media_preview
            .as_ref()
            .is_some_and(|preview| preview.tick());
        direction != 0 || hover_changed || focus_expired || preview_changed
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

        self.ui.ime_cursor = None;
        self.ui.input_area = Rect::default();
        self.ui.input_width = usize::from(header.width.saturating_sub(2));
        let (lines, cursor) = self.ui.input.visual_lines(self.ui.input_width);
        let composer_height = if self.input_enabled && inner.height >= 7 {
            (lines.len().clamp(1, 4) as u16 + 2).min(inner.height.saturating_sub(4))
        } else {
            0
        };
        let body = Rect::new(
            header.x,
            header.bottom().saturating_add(1).min(inner.bottom()),
            header.width,
            inner.height.saturating_sub(3 + composer_height),
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

        if composer_height > 0 {
            let composer = Rect::new(
                header.x,
                inner.bottom() - composer_height,
                header.width,
                composer_height,
            );
            let block = Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(THEME.accent));
            let prompt = block.inner(composer);
            frame.render_widget(block, composer);
            super::render::draw_prompt(
                frame,
                &mut self.ui,
                Rect {
                    height: prompt.height.saturating_sub(1),
                    ..prompt
                },
                lines,
                cursor,
            );
            let hint = self
                .input_message
                .as_deref()
                .unwrap_or(if self.submit_blocked {
                    "生成中，可先输入下一条追问"
                } else {
                    "继续追问"
                });

            frame.render_widget(
                Paragraph::new(fit_label(&safe_output(hint), prompt.width as usize, false)).style(
                    Style::default().fg(if self.input_message.is_some() {
                        THEME.tool_failed
                    } else {
                        THEME.tool_detail
                    }),
                ),
                Rect::new(
                    prompt.x + 2,
                    prompt.bottom() - 1,
                    prompt.width.saturating_sub(2),
                    1,
                ),
            );
            if !self.input_focus {
                self.ui.ime_cursor = None;
            }
        }

        if let Some(preview) = &mut self.ui.media_preview {
            preview.draw(frame);
            self.ui.ime_cursor = None;
        }
    }

    pub fn handle(&mut self, event: &Event) -> std::io::Result<Action> {
        if let Some(preview) = &mut self.ui.media_preview {
            let (_, close) = preview.handle(event);
            if close {
                self.ui.media_preview = None;
            }
            return Ok(Action::Redraw);
        }

        if self.input_enabled {
            if let Event::Key(key) = event
                && key.kind != KeyEventKind::Release
                && key.code == KeyCode::Char('c')
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && let Some(text) = self.ui.input.selected_text()
            {
                super::selection::copy_text(text)?;
                return Ok(Action::Redraw);
            }

            if super::handle_input_mouse(&mut self.ui, event) {
                self.input_focus = true;
                return Ok(Action::Redraw);
            }

            if let Event::Mouse(mouse) = event
                && mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && self
                    .ui
                    .selection
                    .viewport
                    .contains((mouse.column, mouse.row).into())
            {
                self.input_focus = false;
            }

            if let Event::Key(key) = event
                && key.kind != KeyEventKind::Release
            {
                if key.code == KeyCode::Tab {
                    self.input_focus = !self.input_focus;
                    return Ok(Action::Redraw);
                }

                if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                    return Ok(
                        if !self.submit_blocked && !self.ui.input.text.trim().is_empty() {
                            Action::Submit(self.ui.input.text.clone())
                        } else {
                            Action::Redraw
                        },
                    );
                }

                if key.code == KeyCode::Char('v') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
                        Ok(text) => {
                            self.ui.input.insert(&text);
                            self.input_focus = true;
                        }

                        Err(error) => self.submit_error(error),
                    }

                    return Ok(Action::Redraw);
                }
            }

            let text_input = matches!(
                event,
                Event::Paste(_)
                    | Event::Key(KeyEvent {
                        code: KeyCode::Char(_),
                        ..
                    })
            );

            if (self.input_focus || text_input) && self.ui.input.edit(event, self.ui.input_width) {
                self.input_focus = true;
                self.ui.selection.clear();
                self.input_message = None;
                return Ok(Action::Redraw);
            }
        }

        if let Event::Key(key) = event
            && key.kind != KeyEventKind::Release
            && ((key.code == KeyCode::Esc || (!self.input_enabled && key.code == KeyCode::Enter))
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
                    if super::jump_footnote_at(&mut self.ui, (mouse.column, mouse.row)) {
                        return Ok(Action::Redraw);
                    }
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
            && (super::jump_footnote_at(&mut self.ui, point)
                || super::toggle_transcript_at(&mut self.ui, point))
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

    #[test]
    fn follow_up_composer_preserves_unicode_drafts_until_submission_succeeds() {
        let mut view = TranscriptDialog::new("orbit-model");
        view.enable_input();
        for (width, height) in [(180, 50), (80, 35), (40, 18), (10, 8), (1, 1)] {
            render(&mut view, width, height);
            if let Some(cursor) = view.ui.ime_cursor {
                assert!(view.ui.input_area.contains(cursor));
            }
        }

        render(&mut view, 100, 30);
        assert!(view.ui.ime_cursor.is_some());
        view.handle(&Event::Paste("Orbit 中文🦀\n后续问题".into()))
            .unwrap();
        view.block_submission(true);
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(view.handle(&enter).unwrap(), Action::Redraw));
        view.block_submission(false);
        assert!(
            matches!(view.handle(&enter).unwrap(), Action::Submit(text) if text == "Orbit 中文🦀\n后续问题")
        );
        view.submit_error("Orbit 请求未发送");
        assert_eq!(view.ui.input.text, "Orbit 中文🦀\n后续问题");
        view.submitted();
        assert!(view.ui.input.text.is_empty());
        assert!(view.input_message.is_none());
    }
}
