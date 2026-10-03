//! Small editable forms shared by service and per-model settings.
use anyhow::Result;
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures_util::StreamExt;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::super::{input::InputState, overlay, terminal};

pub(super) const ACCENT: Color = crate::tui::theme::THEME.dialog_accent;
// Explicit RGB keeps secondary text readable when a terminal theme maps
// ANSI dark gray close to the transparent background.
const HINT_TEXT: Color = crate::tui::theme::THEME.placeholder;

pub(super) struct Field {
    pub label: String,
    pub input: InputState,
    pub choices: Vec<String>,
    pub secret: bool,
    pub readonly: bool,
    pub hint: String,
}

impl Field {
    pub fn text(label: &str, value: &str, hint: &str) -> Self {
        let mut input = InputState::default();
        input.set(value.into());
        Self {
            label: label.into(),
            input,
            choices: Vec::new(),
            secret: false,
            readonly: false,
            hint: hint.into(),
        }
    }

    pub fn choice(label: &str, value: &str, choices: &[&str]) -> Self {
        let mut field = Self::text(label, value, "");
        field.choices = choices.iter().map(|value| (*value).to_owned()).collect();
        field
    }

    pub fn value(&self) -> &str {
        self.input.text.trim()
    }
}

pub(super) fn area(bounds: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(bounds.width.saturating_sub(4));
    let height = height.min(bounds.height.saturating_sub(2));
    Rect::new(
        bounds.x + (bounds.width - width) / 2,
        bounds.y + (bounds.height - height) / 2,
        width,
        height,
    )
}

/// Byte boundary for a single-line viewport, keeping the insertion point visible.
fn start(input: &InputState, width: usize, secret: bool) -> usize {
    let mut used = 1;
    let mut start = input.cursor;
    for (index, grapheme) in input.text[..input.cursor].grapheme_indices(true).rev() {
        used += if secret { 1 } else { grapheme.width() };
        if used > width {
            break;
        }
        start = index;
    }
    start
}

pub(super) fn input_line(
    input: &InputState,
    width: usize,
    secret: bool,
    focused: bool,
    hint: &str,
) -> Line<'static> {
    if input.text.is_empty() && !focused {
        return Line::styled(hint.to_owned(), Style::default().fg(HINT_TEXT));
    }
    if input.text.is_empty() && focused {
        return Line::from(vec![
            Span::styled(
                " ",
                Style::default().fg(ACCENT).add_modifier(Modifier::REVERSED),
            ),
            Span::styled(hint.to_owned(), Style::default().fg(HINT_TEXT)),
        ]);
    }
    let offset = start(input, width, secret);
    let mut spans = Vec::new();
    for (index, grapheme) in input.text[offset..].grapheme_indices(true) {
        let value = if secret { "•" } else { grapheme };
        let style = if focused && offset + index == input.cursor {
            Style::default().fg(ACCENT).add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        spans.push(Span::styled(value.to_owned(), style));
    }
    if focused && input.cursor == input.text.len() {
        spans.push(Span::styled(
            " ",
            Style::default().fg(ACCENT).add_modifier(Modifier::REVERSED),
        ));
    }
    Line::from(spans)
}

pub(super) fn edit(input: &mut InputState, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Left => input.move_left(),
        KeyCode::Right => input.move_right(),
        KeyCode::Home => input.home(),
        KeyCode::End => input.end(),
        KeyCode::Backspace => input.backspace(),
        KeyCode::Delete => input.delete(),
        KeyCode::Char('u') if modifiers.contains(KeyModifiers::CONTROL) => input.clear(),
        KeyCode::Char(c)
            if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && !c.is_control()
                && input.text.len() + c.len_utf8() <= 16_384 =>
        {
            input.insert(&c.to_string())
        }
        _ => {}
    }
}

pub(super) fn paste(input: &mut InputState, text: &str) {
    let text: String = text.chars().filter(|c| !c.is_control()).collect();
    if input.text.len() + text.len() <= 16_384 {
        input.insert(&text);
    }
}

pub(super) async fn choose(
    terminal: &mut terminal::AppTerminal,
    events: &mut EventStream,
    title: &str,
    choices: &[String],
    current: &str,
) -> Result<Option<String>> {
    let mut selected = choices.iter().position(|v| v == current).unwrap_or(0);
    let mut offset = selected;
    let mut pointer = None;
    loop {
        let mut rows = Vec::new();
        let mut viewport = Rect::default();
        terminal::draw_synchronized(terminal, |frame| {
            let outer = area(
                frame.area(),
                56,
                choices.len().saturating_add(4).min(u16::MAX as usize) as u16,
            );
            overlay::clear(frame, outer);
            let block = Block::default()
                .title(format!(" {title} "))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(ACCENT));
            viewport = block.inner(outer);
            frame.render_widget(block, outer);
            offset = offset.min(choices.len().saturating_sub(viewport.height as usize));
            if let Some(point) = pointer
                && viewport.contains(point)
            {
                let index = offset + (point.y - viewport.y) as usize;
                if let Some(choice) = choices.get(index) {
                    let label = crate::tui::render::padded_label(
                        choice,
                        viewport.width.saturating_sub(2) as usize,
                        1,
                    );
                    let hit = Rect::new(viewport.x + 1, point.y, label.width() as u16, 1);
                    if hit.contains(point) {
                        selected = index;
                    }
                }
            }
            for (index, choice) in choices
                .iter()
                .enumerate()
                .skip(offset)
                .take(viewport.height as usize)
            {
                let label = crate::tui::render::padded_label(
                    choice,
                    viewport.width.saturating_sub(2) as usize,
                    1,
                );
                let row = Rect::new(
                    viewport.x + 1,
                    viewport.y + (index - offset) as u16,
                    label.width() as u16,
                    1,
                );
                frame.render_widget(
                    Paragraph::new(label).style(if index == selected {
                        Style::default()
                            .bg(ACCENT)
                            .fg(crate::tui::theme::THEME.on_accent)
                    } else {
                        Style::default()
                    }),
                    row,
                );
                rows.push((index, row));
            }
        })?;
        let Some(event) = events.next().await else {
            return Ok(None);
        };
        match event? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                pointer = None;
                match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(None);
                    }
                    KeyCode::Enter => return Ok(Some(choices[selected].clone())),
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(choices.len() - 1),
                    _ => {}
                }
                if selected < offset {
                    offset = selected;
                }
                if selected >= offset + viewport.height as usize {
                    offset = selected.saturating_sub(viewport.height.saturating_sub(1) as usize);
                }
            }
            Event::Mouse(mouse) => {
                let point = (mouse.column, mouse.row).into();
                pointer = Some(point);
                if viewport.contains(point) {
                    match mouse.kind {
                        MouseEventKind::ScrollDown => {
                            offset = (offset + 3)
                                .min(choices.len().saturating_sub(viewport.height as usize))
                        }
                        MouseEventKind::ScrollUp => offset = offset.saturating_sub(3),
                        MouseEventKind::Down(MouseButton::Left) => {
                            if let Some((index, _)) =
                                rows.iter().find(|(_, row)| row.contains(point))
                            {
                                return Ok(Some(choices[*index].clone()));
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_editing_keeps_grapheme_boundaries_and_secret_rendering() {
        let mut input = InputState::default();
        input.set("你好e\u{301}".into());

        edit(&mut input, KeyCode::Left, KeyModifiers::NONE);
        edit(&mut input, KeyCode::Char('中'), KeyModifiers::NONE);
        edit(&mut input, KeyCode::Delete, KeyModifiers::NONE);

        assert_eq!(input.text, "你好中");
        assert!(input.text.is_char_boundary(start(&input, 3, false)));
        let hidden = input_line(&input, 30, true, true, "");
        assert!(!hidden.to_string().contains("你好"));
        assert!(hidden.to_string().contains('•'));
    }
}
