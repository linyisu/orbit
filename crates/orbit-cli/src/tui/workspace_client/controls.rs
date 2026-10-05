use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Span,
    widgets::{Block, BorderType, Borders, Paragraph},
};

use crate::tui::{connect_custom::form, input::InputState, render, theme::THEME};

pub(super) fn panel(focused: bool) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if focused {
            THEME.dialog_accent
        } else {
            THEME.border_muted
        }))
}

pub(super) fn button(frame: &mut Frame<'_>, area: Rect, label: &str, focused: bool, primary: bool) {
    let style = if primary {
        Style::default().bg(THEME.dialog_accent).fg(THEME.on_accent)
    } else {
        Style::default().fg(THEME.foreground)
    };
    let border = if primary && focused {
        Style::default().fg(THEME.foreground)
    } else if primary {
        Style::default().fg(THEME.dialog_accent)
    } else {
        panel_border(focused)
    };
    frame.render_widget(panel(focused).border_style(border), area);
    frame.render_widget(
        Paragraph::new(label)
            .alignment(ratatui::layout::Alignment::Center)
            .style(if focused {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            }),
        area.inner(ratatui::layout::Margin::new(1, 1)),
    );
}

fn panel_border(focused: bool) -> Style {
    Style::default().fg(if focused {
        THEME.dialog_accent
    } else {
        THEME.border_muted
    })
}

pub(super) fn field(
    frame: &mut Frame<'_>,
    area: Rect,
    label: &str,
    input: &InputState,
    focused: bool,
    placeholder: &str,
) {
    let inner = area.inner(ratatui::layout::Margin::new(1, 1));
    frame.render_widget(
        panel(focused).title(Span::styled(
            format!(" {label} "),
            Style::default().fg(if focused {
                THEME.dialog_accent
            } else {
                THEME.description
            }),
        )),
        area,
    );
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
    if focused && inner.width > 0 && inner.height > 0 {
        frame.set_cursor_position((
            inner.x
                + (form::cursor_column(input, inner.width as usize, false) as u16)
                    .min(inner.width - 1),
            inner.y,
        ));
    }
}

pub(super) fn select(frame: &mut Frame<'_>, area: Rect, label: &str, value: &str, focused: bool) {
    frame.render_widget(
        Paragraph::new(render::safe_output(&format!("{value}  ▾"))).block(panel(focused).title(
            Span::styled(
                format!(" {label} "),
                Style::default().fg(if focused {
                    THEME.dialog_accent
                } else {
                    THEME.description
                }),
            ),
        )),
        area,
    );
}
