//! Shared terminal controls. Geometry used for painting is also the hit target.
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

use super::theme::THEME;

mod interaction;
pub(super) use interaction::Pointer;

pub(super) const ROW_PADDING: u16 = 2;

/// Footer hints share wrapping, clamping and the same overlay surface.
pub(super) fn hint_above(frame: &mut Frame<'_>, anchor: Rect, text: &str) {
    let bounds = frame.area();
    let width = text
        .lines()
        .map(UnicodeWidthStr::width)
        .max()
        .unwrap_or(0)
        .saturating_add(4)
        .min(72)
        .min(bounds.width as usize) as u16;
    if width < 3 || anchor.y.saturating_sub(bounds.y) < 3 {
        return;
    }
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false }).style(
        Style::default()
            .fg(THEME.overlay_foreground)
            .bg(THEME.overlay_background),
    );
    let height = paragraph
        .line_count(width - 2)
        .saturating_add(2)
        .min(anchor.y.saturating_sub(bounds.y) as usize) as u16;
    let x = anchor
        .right()
        .saturating_sub(width)
        .clamp(bounds.x, bounds.right() - width);
    let area = Rect::new(x, anchor.y - height, width, height);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(THEME.border))
        .style(Style::default().bg(THEME.overlay_background));
    let inner = surface(frame, area, block);
    frame.render_widget(paragraph, inner);
}

pub(super) fn centered(bounds: Rect, width: u16, height: u16) -> Rect {
    let width = bounds.width.saturating_sub(4).min(width);
    let height = bounds.height.saturating_sub(2).min(height);
    Rect::new(
        bounds.x + (bounds.width - width) / 2,
        bounds.y + (bounds.height - height) / 2,
        width,
        height,
    )
}

/// Paint the modal surface; callers place content inside the returned inset.
pub(super) fn modal(frame: &mut Frame<'_>, area: Rect) -> Rect {
    let inner = surface(frame, area, panel());
    Rect::new(
        inner.x.saturating_add(1.min(inner.width)),
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    )
}

pub(super) fn panel() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(accent())
}

/// Framing shared by forms and rich transcript/image viewers. Insets belong to
/// the content layout, so border titles do not consume an extra body row.
pub(super) fn surface(frame: &mut Frame<'_>, area: Rect, block: Block<'_>) -> Rect {
    let area = area.intersection(frame.area());
    super::overlay::clear(frame, area);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    inner
}

pub(super) fn normal() -> Style {
    Style::default().fg(THEME.foreground)
}
pub(super) fn muted() -> Style {
    Style::default().fg(THEME.secondary)
}
pub(super) fn accent() -> Style {
    Style::default().fg(THEME.accent)
}
pub(super) fn highlighted() -> Style {
    Style::default()
        .fg(THEME.on_accent)
        .bg(THEME.accent)
        .add_modifier(Modifier::BOLD)
}

#[derive(Clone, Copy, Default)]
pub(super) struct Control {
    pub highlighted: bool,
    pub selected: bool,
    pub disabled: bool,
}
impl Control {
    pub fn style(self, base: Style) -> Style {
        if self.disabled {
            muted()
        } else if self.highlighted {
            highlighted()
        } else if self.selected {
            accent()
        } else {
            base
        }
    }
}

/// Returns only the visible, enabled hit target, never an off-screen rectangle.
pub(super) fn button(
    frame: &mut Frame<'_>,
    area: Rect,
    label: &str,
    state: Control,
) -> Option<Rect> {
    let area = area.intersection(frame.area());
    frame.render_widget(
        Paragraph::new(super::render::safe_output(label))
            .alignment(Alignment::Center)
            .style(state.style(accent())),
        area,
    );
    (!state.disabled && !area.is_empty()).then_some(area)
}

/// A wrapping action row, clipped to its own allocation instead of the body.
pub(super) struct ActionBar {
    area: Rect,
    x: u16,
    y: u16,
}
impl ActionBar {
    pub fn new(area: Rect) -> Self {
        Self {
            x: area.x,
            y: area.y,
            area,
        }
    }
    pub fn button(&mut self, frame: &mut Frame<'_>, label: &str, state: Control) -> Option<Rect> {
        let width =
            (label.width().saturating_add(4).min(u16::MAX as usize) as u16).min(self.area.width);
        if self.x.saturating_add(width) > self.area.right() {
            self.x = self.area.x;
            self.y = self.y.saturating_add(1);
        }
        if self.y >= self.area.bottom() || width == 0 {
            return None;
        }
        let target = button(frame, Rect::new(self.x, self.y, width, 1), label, state);
        self.x = self.x.saturating_add(width).saturating_add(2);
        target
    }
}

/// Every wrapped line keeps the same inset inside its full-width background.
pub(super) fn choice_lines(text: &str, style: Style, width: u16) -> Vec<Line<'static>> {
    let padding = ROW_PADDING.min(width / 2);
    let content_width = width.saturating_sub(padding * 2).max(1);
    text.split('\n')
        .flat_map(|text| {
            let line = Line::styled(super::render::safe_output(text), style);
            super::transcript::wrap_line(&line, content_width).0
        })
        .map(|line| inset_line(line, width))
        .collect()
}

pub(super) fn inset_line(mut line: Line<'static>, width: u16) -> Line<'static> {
    let padding = ROW_PADDING.min(width / 2) as usize;
    line.spans.insert(0, Span::raw(" ".repeat(padding)));
    line.spans.push(Span::raw(" ".repeat(padding)));
    line
}

pub(super) fn title(frame: &mut Frame<'_>, area: Rect, text: impl Into<String>) {
    frame.render_widget(
        Paragraph::new(super::render::safe_output(&text.into()))
            .style(accent().add_modifier(Modifier::BOLD)),
        Rect {
            height: area.height.min(1),
            ..area
        },
    );
}

#[cfg(test)]
mod tests;
