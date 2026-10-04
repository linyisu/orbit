use super::{Context, Fragment, Source};
use crate::tui::{
    icons,
    render::{fit_label, safe_output},
    theme::THEME,
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};
use unicode_width::UnicodeWidthStr;

#[derive(Debug)]
pub(in crate::tui) struct Preview {
    pub context: Context,
    pub source: Source,
    area: Rect,
    close: Rect,
    hovered: bool,
}

impl Preview {
    pub fn new(context: Context, source: Source) -> Self {
        Self {
            context,
            source,
            area: Rect::default(),
            close: Rect::default(),
            hovered: false,
        }
    }
    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = super::super::dialogs::transcript_area(frame.area());
        self.area = area;
        super::super::overlay::clear(frame, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(THEME.accent));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let header = Rect::new(
            inner.x + 1,
            inner.y,
            inner.width.saturating_sub(2),
            1.min(inner.height),
        );
        let close_width = (icons::CLOSE_THICK.width() + 2).min(usize::from(header.width)) as u16;
        self.close = Rect::new(
            header.right().saturating_sub(close_width),
            header.y,
            close_width,
            header.height,
        );
        let title = match &self.source {
            Source::Image(path) => path.as_str(),
            Source::Math { tex, .. } => tex.as_str(),
        };
        frame.render_widget(
            Paragraph::new(Line::styled(
                fit_label(
                    &safe_output(title),
                    header.width.saturating_sub(close_width + 1) as usize,
                    true,
                ),
                Style::default()
                    .fg(THEME.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            Rect {
                width: header.width.saturating_sub(close_width + 1),
                ..header
            },
        );
        frame.render_widget(
            Paragraph::new(icons::CLOSE_THICK)
                .alignment(Alignment::Center)
                .style(super::super::theme::control_style(
                    self.hovered
                        .then_some(std::time::Duration::from_millis(240)),
                )),
            self.close,
        );
        let body = Rect::new(
            inner.x + 1,
            inner.y + 2,
            inner.width.saturating_sub(2),
            inner.height.saturating_sub(3),
        );
        if let Some(picture) = self
            .context
            .picture(&self.source, body.width, body.height, true)
        {
            let x = body.x + body.width.saturating_sub(picture.size.width) / 2;
            let y = body.y + body.height.saturating_sub(picture.size.height) / 2;
            for row in 0..picture.size.height.min(body.height) {
                super::schedule(
                    &Fragment {
                        picture: picture.clone(),
                        x: 0,
                        row,
                    },
                    Rect::new(x, y + row, picture.size.width, 1),
                );
            }
        } else {
            let message = self
                .context
                .error(&self.source, body.width, body.height, true)
                .unwrap_or_else(|| "正在生成预览…".into());
            frame.render_widget(
                Paragraph::new(safe_output(&message)).wrap(ratatui::widgets::Wrap { trim: false }),
                body,
            );
        }
    }
    /// Returns (consumed, close). Events do not leak into the underlying composer.
    pub fn handle(&mut self, event: &Event) -> (bool, bool) {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => (
                true,
                key.code == KeyCode::Esc
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)),
            ),
            Event::Mouse(mouse) => {
                let point = (mouse.column, mouse.row).into();
                if mouse.kind == MouseEventKind::Moved {
                    self.hovered = self.close.contains(point);
                }
                (
                    true,
                    mouse.kind == MouseEventKind::Down(MouseButton::Left)
                        && (self.close.contains(point) || !self.area.contains(point)),
                )
            }
            Event::Paste(_) => (true, false),
            _ => (false, false),
        }
    }
}
