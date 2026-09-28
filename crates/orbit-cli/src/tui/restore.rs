//! Multi-select restore, drawn in the command completion area rather than a modal.
use super::{app::UiState, theme::THEME};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Clear, Paragraph},
};

#[derive(Clone, Debug)]
struct Entry {
    id: String,
    title: String,
    selected: bool,
}
#[derive(Clone, Debug)]
pub struct RestorePicker {
    entries: Vec<Entry>,
    query: String,
    cursor: usize,
    offset: usize,
    rows: Vec<(Rect, usize)>,
    all: Rect,
    none: Rect,
    area: Rect,
    pointer: Option<(u16, u16)>,
}
impl RestorePicker {
    pub async fn load(store: &orbit_store::store::Store, ids: &[String]) -> anyhow::Result<Self> {
        let mut entries = Vec::new();
        for id in ids {
            // A session deleted since the snapshot should not prevent restoring the rest.
            let Some(title) = store.tui_tab_title(id).await? else {
                continue;
            };
            entries.push(Entry {
                id: id.clone(),
                title: if title.is_empty() {
                    "未命名会话".into()
                } else {
                    super::render::safe_output(&title).replace(['\n', '\r'], " ")
                },
                selected: true,
            });
        }
        Ok(Self {
            entries,
            query: String::new(),
            cursor: 0,
            offset: 0,
            rows: Vec::new(),
            all: Rect::default(),
            none: Rect::default(),
            area: Rect::default(),
            pointer: None,
        })
    }
    fn filtered(&self) -> Vec<usize> {
        super::search::ranked(0..self.entries.len(), |index| {
            super::search::rank(&self.entries[*index].title, &self.query)
        })
    }
    pub fn height(&self) -> u16 {
        self.filtered().len().clamp(1, 8) as u16 + 3
    }
    fn selected(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.id.clone())
            .collect()
    }
    fn hover(&mut self) {
        if let Some(point) = self.pointer
            && let Some((_, index)) = self
                .rows
                .iter()
                .find(|(rect, _)| rect.contains(point.into()))
            && let Some(position) = self.filtered().iter().position(|i| i == index)
        {
            self.cursor = position;
        }
    }
    pub fn draw(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.rows.clear();
        self.all = Rect::default();
        self.none = Rect::default();
        self.area = area;
        if area.height < 3 || area.width < 8 {
            return;
        }
        frame.render_widget(Clear, area);
        let inner = Rect::new(
            area.x + 2,
            area.y,
            area.width.saturating_sub(4),
            area.height,
        );
        self.all = Rect::new(inner.x, inner.y, 4.min(inner.width), 1);
        self.none = Rect::new(
            inner.x + 6,
            inner.y,
            4.min(inner.width.saturating_sub(6)),
            1,
        );
        frame.render_widget(
            Paragraph::new("全选").style(Style::default().fg(THEME.accent)),
            self.all,
        );
        frame.render_widget(
            Paragraph::new("清空").style(Style::default().fg(THEME.accent)),
            self.none,
        );
        let ids = self.filtered();
        let slots = inner.height.saturating_sub(3) as usize;
        self.offset = self.offset.min(ids.len().saturating_sub(slots));
        self.cursor = self.cursor.min(ids.len().saturating_sub(1));
        for (row, &index) in ids.iter().skip(self.offset).take(slots).enumerate() {
            let entry = &self.entries[index];
            let text = super::render::fit_label(
                &format!(
                    "[{}] {}",
                    if entry.selected { "x" } else { " " },
                    entry.title
                ),
                inner.width as usize,
                false,
            );
            use unicode_width::UnicodeWidthStr;
            let hit = Rect::new(inner.x, inner.y + 1 + row as u16, text.width() as u16, 1);
            self.rows.push((hit, index));
        }
        self.hover();
        for (hit, index) in &self.rows {
            let entry = &self.entries[*index];
            let focused = ids.get(self.cursor) == Some(index);
            let text = super::render::fit_label(
                &format!(
                    "[{}] {}",
                    if entry.selected { "x" } else { " " },
                    entry.title
                ),
                hit.width as usize,
                false,
            );
            let style = if focused {
                Style::default().fg(THEME.on_accent).bg(THEME.accent)
            } else {
                Style::default().fg(THEME.foreground)
            };
            frame.render_widget(Paragraph::new(text).style(style), *hit);
        }
        if ids.is_empty() {
            frame.render_widget(
                Paragraph::new(if self.entries.is_empty() {
                    "没有待恢复会话"
                } else {
                    "没有匹配的会话"
                })
                .style(Style::default().fg(THEME.secondary)),
                Rect::new(inner.x, inner.y + 1, inner.width, 1),
            );
        }
        let footer = format!(
            "已选 {} / {} · Enter 恢复 · Esc 返回",
            self.selected().len(),
            self.entries.len()
        );
        frame.render_widget(
            Paragraph::new(footer).style(Style::default().fg(THEME.secondary)),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
    }
}

pub(super) fn handle(state: &mut UiState, event: &Event) -> bool {
    let Some(picker) = &mut state.restore_picker else {
        return false;
    };
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Release => return true,
        Event::Key(key)
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') =>
        {
            return false;
        }
        Event::Key(key) => {
            picker.pointer = None;
            let ids = picker.filtered();
            match key.code {
                KeyCode::Esc => {
                    state.restore_picker = None;
                    state.input.set(String::new());
                    return true;
                }
                KeyCode::Enter => {
                    let ids = picker.selected();
                    if !ids.is_empty() {
                        state.restore_selection = Some(ids);
                        state.restore_picker = None;
                        state.input.set(String::new());
                    }
                    return true;
                }
                KeyCode::Up => picker.cursor = picker.cursor.saturating_sub(1),
                KeyCode::Down => {
                    picker.cursor = (picker.cursor + 1).min(ids.len().saturating_sub(1))
                }
                KeyCode::Char(' ') => {
                    if let Some(&index) = ids.get(picker.cursor) {
                        picker.entries[index].selected ^= true;
                    }
                }
                KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => picker
                    .entries
                    .iter_mut()
                    .for_each(|entry| entry.selected = true),
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => picker
                    .entries
                    .iter_mut()
                    .for_each(|entry| entry.selected = false),
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    picker.query.push(c);
                    picker.cursor = 0;
                    picker.offset = 0;
                }
                KeyCode::Backspace => {
                    picker.query.pop();
                    picker.cursor = 0;
                    picker.offset = 0;
                }
                _ => {}
            }
            let slots = picker.area.height.saturating_sub(3).max(1) as usize;
            picker.offset = picker
                .offset
                .min(picker.cursor)
                .max(picker.cursor.saturating_sub(slots - 1));
            state.input.set(format!("/restore {}", picker.query));
        }
        Event::Mouse(mouse) => {
            let point = (mouse.column, mouse.row);
            picker.pointer = Some(point);
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if picker.all.contains(point.into()) {
                        picker
                            .entries
                            .iter_mut()
                            .for_each(|entry| entry.selected = true);
                    } else if picker.none.contains(point.into()) {
                        picker
                            .entries
                            .iter_mut()
                            .for_each(|entry| entry.selected = false);
                    } else if let Some((_, index)) = picker
                        .rows
                        .iter()
                        .find(|(rect, _)| rect.contains(point.into()))
                    {
                        picker.entries[*index].selected ^= true;
                    }
                }
                MouseEventKind::ScrollDown if picker.area.contains(point.into()) => {
                    picker.offset = (picker.offset + 3).min(
                        picker
                            .filtered()
                            .len()
                            .saturating_sub(picker.area.height.saturating_sub(3) as usize),
                    )
                }
                MouseEventKind::ScrollUp if picker.area.contains(point.into()) => {
                    picker.offset = picker.offset.saturating_sub(3)
                }
                _ => {}
            }
            picker.hover();
        }
        Event::Paste(text) => {
            picker.query.push_str(&text.replace(['\n', '\r'], " "));
            picker.cursor = 0;
            picker.offset = 0;
            state.input.set(format!("/restore {}", picker.query));
        }
        Event::Resize(_, _) => {
            picker.pointer = None;
        }
        _ => {}
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, MouseEvent};
    use ratatui::{Terminal, backend::TestBackend};

    fn state() -> UiState {
        let mut state = UiState::new("test");
        state.restore_picker = Some(RestorePicker {
            entries: (0..15)
                .map(|i| Entry {
                    id: i.to_string(),
                    title: format!("会话 {i:02}"),
                    selected: true,
                })
                .collect(),
            query: String::new(),
            cursor: 0,
            offset: 0,
            rows: Vec::new(),
            all: Rect::default(),
            none: Rect::default(),
            area: Rect::default(),
            pointer: None,
        });
        state
    }
    fn key(state: &mut UiState, code: KeyCode) {
        handle(state, &Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }
    #[test]
    fn picker_fits_the_home_completion_area_without_opening_a_dialog() {
        let mut state = state();
        state.input.set("/restore ".into());
        for (width, height) in [(80, 24), (40, 16), (120, 40)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| super::super::render::draw(frame, &mut state))
                .unwrap();
            let picker = state.restore_picker.as_ref().unwrap();
            assert!(!picker.rows.is_empty());
            assert!(picker.area.bottom() <= state.input_area.y);
            assert!(picker.rows.iter().all(|(area, _)| area.right() <= width));
            assert!(state.picker.is_none());
        }
    }

    #[test]
    fn enter_restores_all_by_default_and_escape_does_not_restore() {
        let mut state = state();
        key(&mut state, KeyCode::Enter);
        assert_eq!(state.restore_selection.as_ref().unwrap().len(), 15);
        assert!(state.restore_picker.is_none());
        let mut state = self::state();
        key(&mut state, KeyCode::Esc);
        assert!(state.restore_selection.is_none());
        assert!(state.restore_picker.is_none());
    }
    #[test]
    fn filtering_preserves_selection_in_hidden_rows() {
        let mut state = state();
        key(&mut state, KeyCode::Char(' ')); // deselect 00
        key(&mut state, KeyCode::Char('1'));
        key(&mut state, KeyCode::Char('4'));
        assert_eq!(state.restore_picker.as_ref().unwrap().filtered(), [14]);
        key(&mut state, KeyCode::Char(' ')); // deselect 14
        key(&mut state, KeyCode::Backspace);
        key(&mut state, KeyCode::Backspace);
        key(&mut state, KeyCode::Enter);
        let selected = state.restore_selection.unwrap();
        assert_eq!(selected.len(), 13);
        assert!(!selected.contains(&"0".into()));
        assert!(!selected.contains(&"14".into()));
    }
    #[test]
    fn wheel_scrolls_rows_and_recomputes_hover_without_mouse_motion() {
        let mut state = state();
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();
        let draw = |state: &mut UiState, terminal: &mut Terminal<TestBackend>| {
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    state.restore_picker.as_mut().unwrap().draw(frame, area);
                })
                .unwrap();
        };
        draw(&mut state, &mut terminal);
        let mouse = |kind, column| {
            Event::Mouse(MouseEvent {
                kind,
                column,
                row: 2,
                modifiers: KeyModifiers::NONE,
            })
        };
        handle(&mut state, &mouse(MouseEventKind::Moved, 4));
        handle(&mut state, &mouse(MouseEventKind::ScrollDown, 4));
        draw(&mut state, &mut terminal);
        let picker = state.restore_picker.as_ref().unwrap();
        assert_eq!(picker.offset, 3);
        assert_eq!(picker.cursor, 4);
        let before = picker.selected();
        handle(
            &mut state,
            &mouse(MouseEventKind::Down(MouseButton::Left), 50),
        );
        assert_eq!(state.restore_picker.as_ref().unwrap().selected(), before);
        handle(
            &mut state,
            &mouse(MouseEventKind::Down(MouseButton::Left), 4),
        );
        assert!(!state.restore_picker.as_ref().unwrap().entries[4].selected);
    }
}
