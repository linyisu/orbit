//! Multi-select restore, drawn in the command completion area rather than a modal.
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    widgets::{Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use super::{app::UiState, icons, theme::THEME};

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
    title_scroll: Option<(usize, usize, Instant)>,
    title_scroll_tick: u128,
}

impl RestorePicker {
    pub fn from_tabs(tabs: &[crate::workspace::windows::SavedTab]) -> Self {
        Self {
            entries: tabs
                .iter()
                .enumerate()
                .map(|(index, tab)| Entry {
                    id: index.to_string(),
                    title: format!("{} · {}", tab.target.label(), tab.title),
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
            title_scroll: None,
            title_scroll_tick: 0,
        }
    }

    fn filtered(&self) -> Vec<usize> {
        super::search::ranked(0..self.entries.len(), |index| {
            super::search::rank(&self.entries[*index].title, &self.query)
        })
    }

    pub fn height(&self) -> u16 {
        self.filtered().len().clamp(1, 8) as u16 + 1
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

    fn prepare_title_scroll(&mut self, width: usize) -> Option<Duration> {
        let focused = self.filtered().get(self.cursor).copied().filter(|index| {
            self.rows.iter().any(|(_, row)| row == index)
                && width > 0
                && self.entries[*index].title.width() > width
        });
        let Some(index) = focused else {
            self.title_scroll = None;
            return None;
        };

        if !self
            .title_scroll
            .as_ref()
            .is_some_and(|(previous, previous_width, _)| {
                *previous == index && *previous_width == width
            })
        {
            self.title_scroll = Some((index, width, Instant::now()));
            self.title_scroll_tick = 0;
        }

        self.title_scroll
            .as_ref()
            .map(|(_, _, since)| since.elapsed() / 2)
    }

    pub fn advance_title_scroll(&mut self) -> bool {
        let Some((_, _, since)) = &self.title_scroll else {
            return false;
        };
        let elapsed = since.elapsed() / 2;
        let tick = elapsed.as_millis().saturating_sub(100) / 120;
        let changed = tick != self.title_scroll_tick;
        self.title_scroll_tick = tick;
        changed
    }

    pub fn draw(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.rows.clear();
        self.all = Rect::default();
        self.none = Rect::default();
        self.area = area;
        if area.height < 2 || area.width < 8 {
            self.title_scroll = None;
            return;
        }

        frame.render_widget(Clear, area);
        let inner = Rect::new(
            area.x + 2,
            area.y,
            area.width.saturating_sub(4),
            area.height,
        );

        self.all = Rect::new(inner.x, inner.y, 6.min(inner.width), 1);
        self.none = Rect::new(
            inner.x + 8.min(inner.width),
            inner.y,
            6.min(inner.width.saturating_sub(8)),
            1,
        );
        for (label, hit) in [("全选", &mut self.all), ("清空", &mut self.none)] {
            let label = super::render::padded_label(label, hit.width as usize, 1);
            hit.width = label.width() as u16;
            let style = if self.pointer.is_some_and(|point| hit.contains(point.into())) {
                Style::default()
                    .fg(THEME.on_accent)
                    .bg(THEME.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(THEME.accent)
            };

            frame.render_widget(Paragraph::new(label).style(style), *hit);
        }

        let total = self.entries.len();
        let selected = self.entries.iter().filter(|entry| entry.selected).count();
        let count = format!(
            "已选 {selected:>width$}/{total}",
            width = total.to_string().len()
        );
        let count_width = inner.width.saturating_sub(16);
        if count.width() <= count_width as usize {
            frame.render_widget(
                Paragraph::new(count)
                    .alignment(Alignment::Right)
                    .style(Style::default().fg(THEME.secondary)),
                Rect::new(inner.x + 16, inner.y, count_width, 1),
            );
        }

        let ids = self.filtered();
        let slots = inner.height.saturating_sub(1) as usize;
        self.offset = self.offset.min(ids.len().saturating_sub(slots));
        self.cursor = self.cursor.min(ids.len().saturating_sub(1));
        for (row, &index) in ids.iter().skip(self.offset).take(slots).enumerate() {
            let entry = &self.entries[index];
            let hit = Rect::new(
                inner.x,
                inner.y + 1 + row as u16,
                (4 + entry.title.width()).min(inner.width as usize) as u16,
                1,
            );

            self.rows.push((hit, index));
        }

        self.hover();
        let title_width = inner.width.saturating_sub(4) as usize;
        let title_elapsed = self.prepare_title_scroll(title_width);
        for (hit, index) in &self.rows {
            let entry = &self.entries[*index];
            let focused = ids.get(self.cursor) == Some(index);
            let title = super::render::scrolling_title(
                &entry.title,
                title_width,
                if focused { title_elapsed } else { None },
            );
            let text = super::render::padded_label(
                &format!("{} {title}", icons::checkbox(entry.selected)),
                hit.width as usize,
                1,
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
    }
}

pub(super) fn handle(state: &mut UiState, event: &Event) -> bool {
    let Some(picker) = &mut state.restore_picker else {
        return false;
    };
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Release => return true,
        Event::Key(key)
            if key.code == KeyCode::Esc
                || (key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.code == KeyCode::Char('c')) =>
        {
            state.restore_picker = None;
            state.input.set(String::new());
            return true;
        }

        Event::Key(key) => {
            picker.pointer = None;
            let ids = picker.filtered();
            match key.code {
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
                    picker.title_scroll = None;
                }

                KeyCode::Backspace => {
                    picker.query.pop();
                    picker.cursor = 0;
                    picker.offset = 0;
                    picker.title_scroll = None;
                }

                _ => {}
            }

            let slots = picker.area.height.saturating_sub(1).max(1) as usize;
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
                            .saturating_sub(picker.area.height.saturating_sub(1) as usize),
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
            picker.title_scroll = None;
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
    use crossterm::event::{KeyEvent, MouseEvent};
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

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
            title_scroll: None,
            title_scroll_tick: 0,
        });
        state
    }

    fn key(state: &mut UiState, code: KeyCode) {
        handle(state, &Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    fn mouse(state: &mut UiState, kind: MouseEventKind, column: u16, row: u16) {
        handle(
            state,
            &Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            }),
        );
    }

    fn draw(state: &mut UiState, terminal: &mut Terminal<TestBackend>) -> ratatui::buffer::Buffer {
        let mut buffer = ratatui::buffer::Buffer::empty(Rect::default());
        terminal
            .draw(|frame| {
                super::super::render::draw(frame, state);
                buffer = frame.buffer_mut().clone();
            })
            .unwrap();
        buffer
    }

    #[test]
    fn toolbar_buttons_highlight_and_update_all_rows_after_filtering_and_resize() {
        for (width, height) in [(28, 18), (40, 16), (80, 24), (120, 40)] {
            let mut state = state();
            state.input.set("/restore ".into());
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            draw(&mut state, &mut terminal);
            let hit = state.restore_picker.as_ref().unwrap().none;
            assert_eq!(hit.width, 6);
            let original = terminal.backend().buffer()[(hit.x + 1, hit.y)].clone();

            mouse(&mut state, MouseEventKind::Moved, hit.x + 1, hit.y);
            draw(&mut state, &mut terminal);
            let cell = &terminal.backend().buffer()[(hit.x + 1, hit.y)];
            assert_eq!(cell.bg, THEME.accent);
            assert_eq!(cell.fg, THEME.on_accent);
            assert!(cell.modifier.contains(Modifier::BOLD));
            mouse(&mut state, MouseEventKind::Moved, width - 1, hit.y);
            draw(&mut state, &mut terminal);
            assert_eq!(terminal.backend().buffer()[(hit.x + 1, hit.y)], original);

            mouse(
                &mut state,
                MouseEventKind::Down(MouseButton::Left),
                hit.right() - 1,
                hit.y,
            );
            draw(&mut state, &mut terminal);
            let picker = state.restore_picker.as_ref().unwrap();
            assert!(picker.selected().is_empty());
            let row = picker.rows[0].0;
            assert_eq!(
                terminal.backend().buffer()[(row.x + 1, row.y)].symbol(),
                icons::CHECKBOX_BLANK_OUTLINE
            );

            key(&mut state, KeyCode::Char('1'));
            key(&mut state, KeyCode::Char('4'));
            draw(&mut state, &mut terminal);
            let picker = state.restore_picker.as_ref().unwrap();
            assert_eq!(picker.filtered(), [14]);
            let hit = picker.all;
            mouse(&mut state, MouseEventKind::Moved, hit.x + 1, hit.y);
            draw(&mut state, &mut terminal);
            assert_eq!(
                terminal.backend().buffer()[(hit.x + 1, hit.y)].bg,
                THEME.accent
            );
            mouse(
                &mut state,
                MouseEventKind::Down(MouseButton::Left),
                hit.right() - 1,
                hit.y,
            );
            let buffer = draw(&mut state, &mut terminal);
            let picker = state.restore_picker.as_ref().unwrap();
            assert_eq!(picker.selected().len(), 15);
            let row = picker.rows[0].0;
            assert_eq!(
                terminal.backend().buffer()[(row.x + 1, row.y)].symbol(),
                icons::CHECKBOX_MARKED_OUTLINE
            );
            let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
            assert!(!text.contains("Enter"));
            assert!(!text.contains("Esc/Ctrl+C"));
            if picker.area.width >= 31 {
                assert!(
                    text.replace(' ', "").contains("已选15/15"),
                    "{width}x{height}"
                );
            }

            assert_eq!(picker.height(), 2);
        }
    }

    #[test]
    fn focused_title_scrolls_after_a_tick_and_resets_for_focus_search_and_width() {
        let mut state = state();
        state.input.set("/restore ".into());
        let picker = state.restore_picker.as_mut().unwrap();
        picker.entries[0].title = "Orbit 文档 e\u{301} 👩‍💻 这是需要完整查看的恢复会话标题".into();
        picker.entries[1].title = "Orbit another long session title that needs to scroll".into();
        let mut terminal = Terminal::new(TestBackend::new(40, 24)).unwrap();
        let before = draw(&mut state, &mut terminal);
        let row = state.restore_picker.as_ref().unwrap().rows[0].0;
        let picker = state.restore_picker.as_mut().unwrap();
        assert_eq!(picker.title_scroll.as_ref().unwrap().0, 0);
        picker.title_scroll.as_mut().unwrap().2 = Instant::now() - Duration::from_millis(500);
        assert!(picker.advance_title_scroll());
        let after = draw(&mut state, &mut terminal);
        assert!((row.x + 3..row.right() - 1).any(|x| before[(x, row.y)] != after[(x, row.y)]));
        for x in row.x..row.x + 3 {
            assert_eq!(before[(x, row.y)], after[(x, row.y)]);
        }

        key(&mut state, KeyCode::Down);
        draw(&mut state, &mut terminal);
        let picker = state.restore_picker.as_ref().unwrap();
        assert_eq!(picker.title_scroll.as_ref().unwrap().0, 1);
        assert_eq!(picker.title_scroll_tick, 0);

        state
            .restore_picker
            .as_mut()
            .unwrap()
            .title_scroll
            .as_mut()
            .unwrap()
            .2 = Instant::now() - Duration::from_secs(1);
        terminal.backend_mut().resize(48, 24);
        handle(&mut state, &Event::Resize(48, 24));
        draw(&mut state, &mut terminal);
        let picker = state.restore_picker.as_ref().unwrap();
        assert_eq!(picker.title_scroll_tick, 0);
        assert!(picker.title_scroll.as_ref().unwrap().2.elapsed() < Duration::from_secs(1));
        assert_eq!(
            picker.title_scroll.as_ref().unwrap().1,
            picker.area.width as usize - 8
        );

        key(&mut state, KeyCode::Down);
        draw(&mut state, &mut terminal);
        let picker = state.restore_picker.as_mut().unwrap();
        assert!(picker.title_scroll.is_none());
        assert!(!picker.advance_title_scroll());
        key(&mut state, KeyCode::Char('文'));
        draw(&mut state, &mut terminal);
        let picker = state.restore_picker.as_ref().unwrap();
        assert_eq!(picker.title_scroll.as_ref().unwrap().0, 0);
        key(&mut state, KeyCode::Char('无'));
        draw(&mut state, &mut terminal);
        let picker = state.restore_picker.as_mut().unwrap();
        assert!(picker.rows.is_empty());
        assert!(picker.title_scroll.is_none());
        assert!(!picker.advance_title_scroll());
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
    fn control_c_closes_empty_or_populated_picker_before_global_shortcuts() {
        use super::super::{Action, keyboard_action};

        for empty in [true, false] {
            for busy in [false, true] {
                let mut state = state();
                state.input.set("/restore ".into());
                if empty {
                    state.restore_picker.as_mut().unwrap().entries.clear();
                }
                if busy {
                    state.begin_run();
                }
                let status = state.status.clone();
                let event = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
                let action = if handle(&mut state, &event) {
                    Action::None
                } else {
                    keyboard_action(&mut state, event.clone(), busy)
                };

                assert_eq!(action, Action::None);
                assert!(state.restore_picker.is_none());
                assert!(state.input.text.is_empty());
                assert!(state.restore_selection.is_none());
                assert!(state.resume_request.is_none());
                assert_eq!(state.status, status);
                assert_eq!(
                    keyboard_action(&mut state, event, busy),
                    if busy { Action::Cancel } else { Action::Quit }
                );
            }
        }
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
