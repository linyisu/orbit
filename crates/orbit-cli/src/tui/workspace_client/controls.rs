use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Block, Borders, Paragraph},
};

use crate::tui::{connect_custom::form, input::InputState, render, theme::THEME};

pub(super) fn panel() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(THEME.dialog_accent))
}

pub(super) fn button(frame: &mut Frame<'_>, area: Rect, label: &str, focused: bool) {
    frame.render_widget(
        Paragraph::new(render::padded_label(label, area.width as usize, 1)).style(if focused {
            Style::default().bg(THEME.dialog_accent).fg(THEME.on_accent)
        } else {
            Style::default().fg(THEME.dialog_accent)
        }),
        area,
    );
}

pub(super) fn field(
    frame: &mut Frame<'_>,
    area: Rect,
    label: &str,
    input: &InputState,
    focused: bool,
    hovered: bool,
    placeholder: &str,
) {
    let inner = field_label(frame, area, label, focused || hovered);
    frame.render_widget(
        Paragraph::new(form::input_line(
            input,
            inner.width as usize,
            false,
            focused,
            placeholder,
        )),
        inner,
    );
    if focused && !inner.is_empty() {
        frame.set_cursor_position((
            inner.x
                + (form::cursor_column(input, inner.width as usize, false) as u16)
                    .min(inner.width - 1),
            inner.y,
        ));
    }
}

fn field_label(frame: &mut Frame<'_>, area: Rect, label: &str, focused: bool) -> Rect {
    frame.render_widget(
        Paragraph::new(label).style(Style::default().fg(if focused {
            THEME.dialog_accent
        } else {
            THEME.description
        })),
        Rect::new(area.x, area.y, area.width, area.height.min(1)),
    );
    Rect::new(
        area.x,
        area.y + area.height.min(1),
        area.width,
        area.height.saturating_sub(1).min(1),
    )
}
