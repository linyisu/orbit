use std::{sync::Arc, time::Duration};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Alignment, Margin, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use super::{
    Context, Fragment, Prepared, Source, View,
    view::{PAN_RANGE, PAN_STEP, ZOOM_LEVELS},
};
use crate::tui::{
    icons,
    render::{fit_label, safe_output},
    theme::THEME,
};

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub(in crate::tui) struct Preview {
    pub context: Context,
    pub source: Source,
    area: Rect,
    close: Rect,
    body: Rect,
    controls: Vec<(Rect, Control)>,
    hovered: Option<Control>,
    view: View,
    picture: Option<Arc<Prepared>>,
    drag: Option<Drag>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Close,
    ZoomOut,
    ZoomIn,
    Fit,
}

#[derive(Debug)]
struct Drag {
    origin: (u16, u16),
    pan: (i16, i16),
    extent: (f32, f32),
}

impl Preview {
    pub fn new(context: Context, source: Source) -> Self {
        Self {
            context,
            source,
            area: Rect::default(),
            close: Rect::default(),
            body: Rect::default(),
            controls: Vec::new(),
            hovered: None,
            view: View::default(),
            picture: None,
            drag: None,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        let area = frame.area().inner(Margin::new(2, 1));
        self.area = area;
        self.close = Rect::default();
        self.body = Rect::default();
        self.controls.clear();
        if area.is_empty() {
            return;
        }
        super::super::overlay::clear(frame, area);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
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
        self.controls.push((self.close, Control::Close));
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
                .style(self.control_style(Control::Close)),
            self.close,
        );
        self.body = Rect::new(
            inner.x + 1,
            inner.y + 2,
            inner.width.saturating_sub(2),
            inner.height.saturating_sub(4),
        );
        self.draw_picture(frame);
        if inner.height >= 4 {
            self.draw_controls(
                frame,
                Rect::new(
                    inner.x + 1,
                    inner.bottom() - 1,
                    inner.width.saturating_sub(2),
                    1,
                ),
            );
        }
    }

    fn draw_picture(&mut self, frame: &mut Frame<'_>) {
        let body = self.body;
        if body.is_empty() {
            return;
        }
        if let Some(picture) =
            self.context
                .preview_picture(&self.source, body.width, body.height, self.view)
        {
            self.picture = Some(picture);
        }
        let error = self
            .context
            .preview_error(&self.source, body.width, body.height, self.view);
        if error.is_none()
            && let Some(picture) = &self.picture
            && picture.size.width <= body.width
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
            let message = error.unwrap_or_else(|| "正在生成预览…".into());
            frame.render_widget(
                Paragraph::new(safe_output(&message)).wrap(ratatui::widgets::Wrap { trim: false }),
                body,
            );
        }
    }

    fn control_style(&self, control: Control) -> Style {
        super::super::theme::control_style(
            (self.hovered == Some(control)).then_some(Duration::from_millis(240)),
        )
    }

    fn draw_controls(&mut self, frame: &mut Frame<'_>, footer: Rect) {
        let mut x = footer.x;
        for (label, control) in [
            (" 缩小 ".into(), Some(Control::ZoomOut)),
            (format!(" {}% ", self.view.zoom), None),
            (" 放大 ".into(), Some(Control::ZoomIn)),
            (" 适应 ".into(), Some(Control::Fit)),
        ] {
            let width = (label.width() as u16).min(footer.right().saturating_sub(x));
            let area = Rect::new(x, footer.y, width, footer.height);
            let style = control.map_or(Style::default().fg(THEME.muted), |c| self.control_style(c));
            frame.render_widget(Paragraph::new(label).style(style), area);
            if let Some(control) = control {
                self.controls.push((area, control));
            }
            x += width;
        }
        frame.render_widget(
            Paragraph::new(fit_label(
                "  滚轮/± 缩放 · 拖动/方向键移动 · Esc 关闭",
                usize::from(footer.right().saturating_sub(x)),
                false,
            ))
            .style(Style::default().fg(THEME.muted)),
            Rect::new(x, footer.y, footer.right().saturating_sub(x), 1),
        );
    }

    fn zoom(&mut self, increase: bool) {
        let next = if increase {
            ZOOM_LEVELS
                .iter()
                .copied()
                .find(|level| *level > self.view.zoom)
        } else {
            ZOOM_LEVELS
                .iter()
                .copied()
                .rev()
                .find(|level| *level < self.view.zoom)
        };
        if let Some(next) = next {
            self.view.zoom = next;
            self.drag = None;
        }
    }

    fn activate(&mut self, control: Control) -> bool {
        match control {
            Control::Close => return true,
            Control::ZoomOut => self.zoom(false),
            Control::ZoomIn => self.zoom(true),
            Control::Fit => {
                self.view = View::default();
                self.drag = None;
            }
        }
        false
    }

    fn pan(&mut self, delta: (i16, i16)) {
        if let Some(picture) = &self.picture {
            for (position, change, extent) in [
                (&mut self.view.pan.0, delta.0, picture.pan_extent.0),
                (&mut self.view.pan.1, delta.1, picture.pan_extent.1),
            ] {
                if extent > 0.0 {
                    *position = (*position + change).clamp(-PAN_RANGE, PAN_RANGE);
                }
            }
        }
    }

    /// Returns (consumed, close). Events do not leak into the underlying composer.
    pub fn handle(&mut self, event: &Event) -> (bool, bool) {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                if key.code == KeyCode::Esc
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    return (true, true);
                }
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    match key.code {
                        KeyCode::Char('+' | '=') => self.zoom(true),
                        KeyCode::Char('-') => self.zoom(false),
                        KeyCode::Char('0') | KeyCode::Home => {
                            self.activate(Control::Fit);
                        }
                        KeyCode::Left => self.pan((-PAN_STEP, 0)),
                        KeyCode::Right => self.pan((PAN_STEP, 0)),
                        KeyCode::Up => self.pan((0, -PAN_STEP)),
                        KeyCode::Down => self.pan((0, PAN_STEP)),
                        _ => {}
                    }
                }
                (true, false)
            }

            Event::Mouse(mouse) => {
                let point = (mouse.column, mouse.row);
                let control = self
                    .controls
                    .iter()
                    .find(|(area, _)| area.contains(point.into()))
                    .map(|(_, control)| *control);
                match mouse.kind {
                    MouseEventKind::Moved => self.hovered = control,
                    MouseEventKind::Down(MouseButton::Left) => {
                        if !self.area.contains(point.into()) {
                            return (true, true);
                        }
                        if let Some(control) = control {
                            return (true, self.activate(control));
                        }
                        if self.body.contains(point.into())
                            && let Some(picture) = &self.picture
                        {
                            self.drag = Some(Drag {
                                origin: point,
                                pan: self.view.pan,
                                extent: picture.pan_extent,
                            });
                        }
                    }
                    MouseEventKind::Drag(MouseButton::Left) => {
                        if let Some(drag) = &self.drag {
                            let shift = |origin, current, initial: i16, extent: f32| {
                                if extent <= 0.0 {
                                    return initial;
                                }
                                let delta = f32::from(origin) - f32::from(current);
                                (f32::from(initial) + delta * f32::from(2 * PAN_RANGE) / extent)
                                    .round()
                                    .clamp(-f32::from(PAN_RANGE), f32::from(PAN_RANGE))
                                    as i16
                            };
                            self.view.pan = (
                                shift(drag.origin.0, point.0, drag.pan.0, drag.extent.0),
                                shift(drag.origin.1, point.1, drag.pan.1, drag.extent.1),
                            );
                        }
                    }
                    MouseEventKind::Up(MouseButton::Left) => self.drag = None,
                    MouseEventKind::ScrollUp if self.area.contains(point.into()) => self.zoom(true),
                    MouseEventKind::ScrollDown if self.area.contains(point.into()) => {
                        self.zoom(false)
                    }
                    _ => {}
                }
                (true, false)
            }

            Event::FocusLost | Event::Resize(_, _) => {
                self.drag = None;
                self.hovered = None;
                (true, false)
            }
            Event::Paste(_) | Event::Key(_) => (true, false),
            _ => (false, false),
        }
    }
}
