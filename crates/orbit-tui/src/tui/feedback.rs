//! Local operational errors stay visible independently of transcript scrolling.
//! Bounded, deduplicated notices share the application's surface/detail dialog.
use crossterm::event::{Event, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{BorderType, Paragraph, Wrap},
};
use std::collections::VecDeque;

use super::{
    components, icons,
    render::{fit_label, safe_output},
    theme::THEME,
};

#[derive(Debug)]
struct Notice {
    text: String,
    count: usize,
}

#[derive(Debug, Default)]
pub(super) struct Feedback {
    notices: VecDeque<Notice>,
    area: Rect,
    close: Rect,
    pressed: Option<bool>,
}

pub(super) enum Action {
    None,
    Redraw,
    Details(String),
}

impl Feedback {
    pub fn error(&mut self, text: impl AsRef<str>) {
        const MAX_CHARS: usize = 8192;
        let mut chars = text.as_ref().chars();
        let prefix = chars.by_ref().take(MAX_CHARS).collect::<String>();
        let mut text = prefix
            .split('\n')
            .map(safe_output)
            .collect::<Vec<_>>()
            .join("\n");
        if chars.next().is_some() {
            text.push_str("\n…错误详情过长，已截断");
        }
        let count = self
            .notices
            .iter()
            .position(|n| n.text == text)
            .and_then(|i| self.notices.remove(i))
            .map_or(1, |n| n.count.saturating_add(1));
        self.notices.push_front(Notice { text, count });
        self.notices.truncate(8);
        // An asynchronous error must not reuse hit targets from a different card.
        self.area = Rect::default();
        self.close = Rect::default();
        self.pressed = None;
    }

    pub fn draw(&mut self, frame: &mut Frame<'_>, bounds: Rect) {
        self.area = Rect::default();
        self.close = Rect::default();
        let Some(notice) = self.notices.front() else {
            return;
        };
        if bounds.is_empty() {
            return;
        }
        let compact = bounds.width < 20 || bounds.height < 7;
        self.area = if compact {
            Rect::new(bounds.x, bounds.y, bounds.width, 1)
        } else {
            let width = bounds.width.saturating_sub(4).min(64);
            Rect::new(bounds.right() - width - 2, bounds.y + 2, width, 6)
        };
        let title = format!(
            "操作失败{}{}",
            if notice.count > 1 {
                format!(" · {} 次", notice.count)
            } else {
                String::new()
            },
            if self.notices.len() > 1 {
                format!(" · 另 {} 条", self.notices.len() - 1)
            } else {
                String::new()
            }
        );
        let inner = if compact {
            super::overlay::clear(frame, self.area);
            self.area
        } else {
            components::surface(
                frame,
                self.area,
                components::panel()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(THEME.border_muted))
                    .style(Style::default().bg(THEME.overlay_background)),
            )
        };
        let padding = u16::from(!compact);
        let content = Rect::new(
            inner.x + padding,
            inner.y,
            inner.width.saturating_sub(padding * 2),
            inner.height,
        );
        self.close = Rect::new(
            content.right().saturating_sub(3.min(content.width)),
            content.y,
            3.min(content.width),
            1,
        );
        let text = if compact {
            format!("{title}：{}", notice.text)
        } else {
            title
        };
        frame.render_widget(
            Paragraph::new(fit_label(
                &text,
                content.width.saturating_sub(self.close.width) as usize,
                false,
            ))
            .style(Style::default().fg(THEME.error)),
            content,
        );
        frame.render_widget(
            Paragraph::new(Line::from(icons::CLOSE_THICK).centered()).style(components::muted()),
            self.close,
        );
        if !compact {
            frame.render_widget(
                Paragraph::new(notice.text.as_str())
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(THEME.overlay_foreground)),
                Rect::new(content.x, content.y + 1, content.width, 2),
            );
            frame.render_widget(
                Paragraph::new("点击查看详情").style(components::muted()),
                Rect::new(content.x, content.bottom() - 1, content.width, 1),
            );
        }
    }

    pub fn handle(&mut self, event: &Event) -> Action {
        let Event::Mouse(mouse) = event else {
            return Action::None;
        };
        let inside = self.area.contains((mouse.column, mouse.row).into());
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if inside => {
                self.pressed = Some(self.close.contains((mouse.column, mouse.row).into()));
                Action::Redraw
            }
            MouseEventKind::Up(MouseButton::Left) if self.pressed.is_some() => {
                let close = self.pressed.take().unwrap();
                if !inside || close != self.close.contains((mouse.column, mouse.row).into()) {
                    return Action::Redraw;
                }
                let notice = self.notices.pop_front().unwrap();
                self.area = Rect::default();
                self.close = Rect::default();
                if close {
                    Action::Redraw
                } else {
                    Action::Details(notice.text)
                }
            }
            _ if inside => Action::Redraw,
            _ => Action::None,
        }
    }
}

#[cfg(test)]
mod tests;
