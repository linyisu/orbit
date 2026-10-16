use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Text coordinates before viewport scrolling; named axes prevent transposition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputPosition {
    pub column: usize,
    pub row: usize,
}

impl InputPosition {
    pub(super) fn in_viewport(
        self,
        area: ratatui::layout::Rect,
        offset: usize,
    ) -> Option<ratatui::layout::Position> {
        let row = self.row.checked_sub(offset)?;
        if area.is_empty() || row >= area.height as usize {
            return None;
        }
        Some(ratatui::layout::Position::new(
            area.x + self.column.min(area.width as usize - 1) as u16,
            area.y + row as u16,
        ))
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Motion {
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
    Start,
    Finish,
    WordLeft,
    WordRight,
}

#[derive(Clone, Debug, Default)]
pub struct InputState {
    pub text: String,
    pub cursor: usize,
    pub history: Vec<String>,
    history_index: Option<usize>,
    draft: Option<(String, usize)>,
    preferred_column: Option<usize>,
    selection_anchor: Option<usize>,
}

impl InputState {
    /// Editing without command completion, attachment handling or submission.
    pub(super) fn edit(&mut self, event: &crossterm::event::Event, width: usize) -> bool {
        use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
        let Event::Key(key) = event else {
            if let Event::Paste(text) = event {
                self.insert(text);
                return true;
            }

            return false;
        };
        if key.kind == KeyEventKind::Release {
            return false;
        }
        let control = key
            .modifiers
            .contains(KeyModifiers::CONTROL);
        let selecting = key
            .modifiers
            .contains(KeyModifiers::SHIFT);
        let motion = match (control, key.code) {
            (true, KeyCode::Left) => Some(Motion::WordLeft),
            (true, KeyCode::Right) => Some(Motion::WordRight),
            (true, KeyCode::Home) => Some(Motion::Start),
            (true, KeyCode::End) => Some(Motion::Finish),
            (true, KeyCode::Char('e')) => Some(Motion::End),
            (false, KeyCode::Left) => Some(Motion::Left),
            (false, KeyCode::Right) => Some(Motion::Right),
            (false, KeyCode::Home) => Some(Motion::Home),
            (false, KeyCode::End) => Some(Motion::End),
            (false, KeyCode::Up) => Some(Motion::Up),
            (false, KeyCode::Down) => Some(Motion::Down),
            _ => None,
        };

        if let Some(motion) = motion {
            self.navigate(motion, width, selecting);
        } else {
            match (control, key.code) {
                (true, KeyCode::Char('a')) => self.select_all(),
                (true, KeyCode::Char('u')) => self.kill_to_home(),
                (true, KeyCode::Char('k')) => self.kill_to_end(),
                (true, KeyCode::Char('d')) | (false, KeyCode::Delete) => self.delete(),
                (true, KeyCode::Char('j')) => self.insert("\n"),
                (true, KeyCode::Up) => self.history_up(),
                (true, KeyCode::Down) => self.history_down(),
                (false, KeyCode::Backspace) => self.backspace(),
                (false, KeyCode::Enter)
                    if key
                        .modifiers
                        .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
                {
                    self.insert("\n")
                }

                (false, KeyCode::Char(c))
                    if !key
                        .modifiers
                        .contains(KeyModifiers::ALT) =>
                {
                    self.insert(&c.to_string())
                }
                _ => return false,
            }
        }

        true
    }

    pub fn insert(&mut self, text: &str) {
        let text = Self::normalize(text);
        if !text.is_empty() {
            self.replace(
                self.selection()
                    .unwrap_or(self.cursor..self.cursor),
                &text,
            );
        }
    }

    pub fn set(&mut self, text: String) {
        let text = Self::normalize(&text);
        let cursor = text.len();
        self.load(text, cursor);
        self.history_index = None;
        self.draft = None;
    }

    fn load(&mut self, text: String, cursor: usize) {
        self.preferred_column = None;
        self.text = text;
        self.cursor = self.boundary_after(cursor);
        self.selection_anchor = None;
    }

    fn normalize(text: &str) -> String {
        text.replace("\r\n", "\n")
            .replace('\r', "\n")
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect::<String>()
            .replace('\t', "    ")
    }

    fn boundary_after(&self, offset: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .find(|index| *index >= offset.min(self.text.len()))
            .unwrap_or(self.text.len())
    }

    // Editing can join previously separate graphemes across either side of the range.
    fn replace(&mut self, range: Range<usize>, text: &str) {
        let cursor = range.start + text.len();
        self.text.replace_range(range, text);
        self.cursor = self.boundary_after(cursor);
        self.preferred_column = None;
        self.history_index = None;
        self.draft = None;
        self.selection_anchor = None;
    }

    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.selection_anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection()
            .map(|range| &self.text[range])
    }

    pub fn clear_selection(&mut self) {
        self.selection_anchor = None;
    }

    pub fn select_all(&mut self) {
        self.selection_anchor = Some(0);
        self.cursor = self.text.len();
        self.preferred_column = None;
    }

    pub fn set_cursor(&mut self, offset: usize) {
        self.cursor = self.boundary_after(offset);
        self.selection_anchor = None;
        self.preferred_column = None;
    }

    pub(super) fn navigate(&mut self, motion: Motion, width: usize, selecting: bool) {
        let selection = self.selection();
        let anchor = selecting.then(|| {
            self.selection_anchor
                .unwrap_or(self.cursor)
        });
        self.selection_anchor = None;
        if !selecting && let Some(range) = selection {
            match motion {
                Motion::Left => {
                    self.set_cursor(range.start);
                    return;
                }

                Motion::Right => {
                    self.set_cursor(range.end);
                    return;
                }

                _ => {}
            }
        }

        match motion {
            Motion::Left => self.move_left(),
            Motion::Right => self.move_right(),
            Motion::Home => self.home(),
            Motion::End => self.end(),
            Motion::Up => self.vertical(false, width, !selecting),
            Motion::Down => self.vertical(true, width, !selecting),
            Motion::Start => self.set_cursor(0),
            Motion::Finish => self.set_cursor(self.text.len()),
            Motion::WordLeft => {
                let offset = self.text[..self.cursor]
                    .split_word_bound_indices()
                    .rev()
                    .find(|(_, word)| !word.trim().is_empty())
                    .map_or(0, |(index, _)| index);

                self.set_cursor(offset);
            }

            Motion::WordRight => {
                let offset = self.text[self.cursor..]
                    .split_word_bound_indices()
                    .find(|(_, word)| !word.trim().is_empty())
                    .map_or(self.text.len(), |(index, word)| {
                        self.cursor + index + word.len()
                    });

                self.set_cursor(offset);
            }
        }

        self.selection_anchor = anchor;
    }

    pub fn clear(&mut self) {
        self.preferred_column = None;
        self.text.clear();
        self.cursor = 0;
        self.history_index = None;
        self.draft = None;
        self.selection_anchor = None;
    }

    pub fn backspace(&mut self) {
        if let Some(range) = self.selection() {
            self.replace(range, "");
            return;
        }

        let end = self.cursor;
        self.move_left();
        if self.cursor != end {
            self.replace(self.cursor..end, "");
        }
    }

    pub fn delete(&mut self) {
        if let Some(range) = self.selection() {
            self.replace(range, "");
            return;
        }

        let start = self.cursor;
        self.move_right();
        if self.cursor != start {
            self.replace(start..self.cursor, "");
        }
    }

    pub fn kill_to_home(&mut self) {
        if let Some(range) = self.selection() {
            self.replace(range, "");
            return;
        }

        let end = self.cursor;
        self.home();
        if self.cursor != end {
            self.replace(self.cursor..end, "");
        }
    }

    pub fn kill_to_end(&mut self) {
        if let Some(range) = self.selection() {
            self.replace(range, "");
            return;
        }

        let start = self.cursor;
        self.end();
        if self.cursor != start {
            self.replace(start..self.cursor, "");
        }
    }

    pub fn move_left(&mut self) {
        self.preferred_column = None;
        self.cursor = self
            .text
            .grapheme_indices(true)
            .take_while(|(index, _)| *index < self.cursor)
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }

    pub fn move_right(&mut self) {
        self.preferred_column = None;
        self.cursor = self
            .text
            .grapheme_indices(true)
            .find(|(index, _)| *index > self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.text.len());
    }

    pub fn home(&mut self) {
        self.preferred_column = None;
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
    }

    pub fn end(&mut self) {
        self.preferred_column = None;
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.text.len());
    }

    pub fn take(&mut self) -> Option<String> {
        if self.text.trim().is_empty() {
            return None;
        }
        let value = self.text.clone();
        self.history.push(value.clone());
        self.clear();

        Some(value)
    }

    pub fn browsing_history(&self) -> bool {
        self.history_index.is_some()
    }

    pub fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }

        if self.history_index.is_none() {
            self.draft = Some((self.text.clone(), self.cursor));
        }

        let index = self
            .history_index
            .unwrap_or(self.history.len())
            .saturating_sub(1);
        self.history_index = Some(index);
        let text = Self::normalize(&self.history[index]);
        let cursor = text.len();
        self.load(text, cursor);
    }

    pub fn history_down(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };

        if index + 1 >= self.history.len() {
            self.history_index = None;
            if let Some((text, cursor)) = self.draft.take() {
                self.load(text, cursor);
            }
        } else {
            self.history_index = Some(index + 1);
            let text = Self::normalize(&self.history[index + 1]);
            let cursor = text.len();
            self.load(text, cursor);
        }
    }

    fn vertical(&mut self, down: bool, width: usize, history: bool) {
        let (lines, positions) = self.layout(width);
        let &(_, column, row) = positions
            .iter()
            .find(|p| p.0 == self.cursor)
            .unwrap();
        let target = if down {
            row.checked_add(1)
        } else {
            row.checked_sub(1)
        };

        let desired = self.preferred_column.unwrap_or(column);

        if let Some(target) = target
            && let Some(&(index, _, _)) = positions
                .iter()
                .filter(|p| p.2 == target)
                .min_by_key(|p| p.1.abs_diff(desired))
        {
            self.cursor = index;
            self.preferred_column = Some(desired);
        } else if history && (lines.len() == 1 || self.history_index.is_some()) {
            if down {
                self.history_down();
            } else {
                self.history_up();
            }
        }
    }

    pub fn position_at(&self, column: usize, row: usize, width: usize) -> usize {
        let (lines, positions) = self.layout(width);
        let row = row.min(lines.len() - 1);
        if column >= lines[row].width() {
            let start = positions
                .iter()
                .find(|p| p.2 == row)
                .unwrap()
                .0;
            return start + lines[row].len();
        }

        positions
            .iter()
            .filter(|p| p.2 == row)
            .min_by_key(|p| p.1.abs_diff(column))
            .map_or(self.cursor, |p| p.0)
    }

    pub fn begin_selection(&mut self, index: usize) {
        self.set_cursor(index);
        self.selection_anchor = Some(self.cursor);
    }

    pub fn extend_selection(&mut self, index: usize) {
        self.selection_anchor
            .get_or_insert(self.cursor);
        self.cursor = self.boundary_after(index);
        self.preferred_column = None;
    }

    pub fn selection_cells(&self, width: usize) -> Vec<(usize, Range<usize>)> {
        let Some(range) = self.selection() else {
            return Vec::new();
        };
        let (_, positions) = self.layout(width);
        positions
            .windows(2)
            .filter_map(|pair| {
                let (index, column, row) = pair[0];
                if !range.contains(&index) {
                    return None;
                }
                let width = self.text[index..pair[1].0]
                    .width()
                    .max(1);
                Some((row, column..column + width))
            })
            .collect()
    }

    pub fn visual_lines(&self, width: usize) -> (Vec<String>, InputPosition) {
        let (lines, positions) = self.layout(width);
        let &(_, column, row) = positions
            .iter()
            .find(|p| p.0 == self.cursor)
            .unwrap();
        (lines, InputPosition { column, row })
    }

    // A single layout supplies both drawing and byte-safe vertical navigation.
    fn layout(&self, width: usize) -> (Vec<String>, Vec<(usize, usize, usize)>) {
        let width = width.max(2);
        let mut lines = vec![String::new()];
        let mut positions = Vec::new();
        let mut column = 0;

        for (index, grapheme) in self.text.grapheme_indices(true) {
            let size = grapheme.width();

            if grapheme != "\n" && column + size > width {
                lines.push(String::new());
                column = 0;
            }

            positions.push((index, column, lines.len() - 1));

            if grapheme == "\n" {
                lines.push(String::new());
                column = 0;
            } else {
                lines
                    .last_mut()
                    .unwrap()
                    .push_str(grapheme);
                column += size;
            }
        }

        if column == width {
            lines.push(String::new());
            column = 0;
        }

        positions.push((self.text.len(), column, lines.len() - 1));

        (lines, positions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_that_join_graphemes_keep_layout_and_navigation_valid() {
        let mut input = InputState::default();
        input.set("👩💻".into());
        input.home();
        input.move_right();
        input.insert("\u{200d}");
        assert_eq!(input.text, "👩‍💻");
        assert_eq!(input.cursor, input.text.len());
        assert_eq!(
            input.visual_lines(10).1,
            InputPosition { column: 2, row: 0 }
        );
        input.navigate(Motion::Up, 10, false);
        input.backspace();
        assert!(input.text.is_empty());

        for delete in [false, true] {
            input.set("👩‍ 💻".into());
            input.home();
            input.move_right();
            if delete {
                input.delete();
            } else {
                input.move_right();
                input.backspace();
            }

            assert_eq!(input.text, "👩‍💻");
            assert_eq!(
                input.visual_lines(10).1,
                InputPosition { column: 2, row: 0 }
            );
        }

        input.set("\u{301}".into());
        input.home();
        input.insert("e");
        assert_eq!(input.cursor, input.text.len());
        assert_eq!(
            input.visual_lines(10).1,
            InputPosition { column: 1, row: 0 }
        );
    }

    #[test]
    fn replacement_deletion_and_navigation_use_editable_grapheme_selections() {
        let mut input = InputState::default();
        input.set("Orbit你好👩‍💻".into());
        input.navigate(Motion::Left, 20, true);
        input.navigate(Motion::Left, 20, true);
        assert_eq!(input.selected_text(), Some("好👩‍💻"));
        input.insert("e\u{301}");
        assert_eq!(input.text, "Orbit你e\u{301}");
        input.navigate(Motion::Left, 20, true);
        input.delete();
        assert_eq!(input.text, "Orbit你");
        input.select_all();
        input.navigate(Motion::Left, 20, false);
        assert_eq!(input.cursor, 0);
        assert!(input.selection().is_none());
        input.select_all();
        input.navigate(Motion::Right, 20, false);
        assert_eq!(input.cursor, input.text.len());
        input.select_all();
        input.backspace();
        assert!(input.text.is_empty());
    }

    #[test]
    fn history_restores_the_draft_cursor_and_detaches_on_any_edit() {
        let mut input = InputState::default();
        input
            .history
            .push("Orbit history".into());
        input.set("Orbit draft".into());
        input.set_cursor(5);
        input.history_up();
        input.history_down();
        assert_eq!((&*input.text, input.cursor), ("Orbit draft", 5));
        for edit in 0..3 {
            input.history_up();
            match edit {
                0 => input.set("Orbit completion".into()),
                1 => input.backspace(),
                _ => {
                    input.home();
                    input.delete();
                }
            }

            let edited = input.text.clone();
            input.history_down();
            assert_eq!(input.text, edited);
        }
    }

    #[test]
    fn take_preserves_indentation_and_trailing_newlines_but_ignores_blank_input() {
        let mut input = InputState::default();
        input.insert("    Orbit\r\nnext\r\tline\n");
        assert_eq!(input.take().as_deref(), Some("    Orbit\nnext\n    line\n"));
        input.insert(" \n ");
        assert!(input.take().is_none());
    }

    #[test]
    fn selection_spans_wrapped_rows_and_full_row_newline_carets() {
        let mut input = InputState::default();
        input.set("Orbit\n你好".into());
        input.set_cursor(5);
        assert_eq!(input.visual_lines(5).1, InputPosition { column: 5, row: 0 });
        input.begin_selection(0);
        input.extend_selection(input.text.len());
        let cells = input.selection_cells(5);
        assert!(cells.contains(&(0, 5..6)));
        assert!(cells.contains(&(1, 0..2)));
        assert!(cells.contains(&(1, 2..4)));
    }

    #[test]
    fn multiline_navigation_preserves_columns_and_stays_in_the_draft() {
        let mut input = InputState::default();
        input.history.push("old prompt".into());
        input.set("你好ab\nx\n世界cd".into());
        input.navigate(Motion::Up, 20, false);

        assert_eq!(
            input.visual_lines(20).1,
            InputPosition { column: 1, row: 1 }
        );

        input.navigate(Motion::Up, 20, false);

        assert_eq!(
            input.visual_lines(20).1,
            InputPosition { column: 6, row: 0 }
        );

        input.navigate(Motion::Down, 20, false);
        input.navigate(Motion::Down, 20, false);

        assert_eq!(
            input.visual_lines(20).1,
            InputPosition { column: 6, row: 2 }
        );

        input.navigate(Motion::Up, 20, false);
        input.navigate(Motion::Up, 20, false);
        input.navigate(Motion::Up, 20, false);

        assert_eq!(input.text, "你好ab\nx\n世界cd");
        input.history_up();
        assert_eq!(input.text, "old prompt");
        input.history_down();
        assert_eq!(input.text, "你好ab\nx\n世界cd");
        assert_eq!(
            input.visual_lines(20).1,
            InputPosition { column: 6, row: 0 }
        );
    }

    #[test]
    fn vertical_navigation_uses_wrapped_rows_and_grapheme_boundaries() {
        let mut input = InputState::default();
        input.set("你好e\u{301}👩‍💻xy".into());

        assert_eq!(input.visual_lines(4).1, InputPosition { column: 1, row: 2 });

        input.navigate(Motion::Up, 4, false);

        assert_eq!(input.visual_lines(4).1, InputPosition { column: 1, row: 1 });
        assert_eq!(&input.text[input.cursor..], "👩‍💻xy");

        input.navigate(Motion::Up, 4, false);

        // ties choose the preceding boundary
        assert_eq!(input.cursor, 0);

        input.navigate(Motion::Down, 4, false);

        assert_eq!(input.visual_lines(4).1, InputPosition { column: 1, row: 1 });

        // reset the preferred column after horizontal movement
        input.move_right();
        input.navigate(Motion::Up, 4, false);

        assert_eq!(input.visual_lines(4).1, InputPosition { column: 2, row: 0 });

        input.backspace();

        assert!(input.text.starts_with("好"));
    }

    #[test]
    fn clicking_the_padding_after_a_soft_wrap_selects_the_entire_row() {
        let mut input = InputState::default();
        input.set("Orbit你好".into());
        input.begin_selection(0);
        input.extend_selection(input.position_at(5, 0, 5));
        assert_eq!(input.selected_text(), Some("Orbit"));
        assert_eq!(input.visual_lines(5).1, InputPosition { column: 0, row: 1 });
    }

    #[test]
    fn mouse_positions_use_grapheme_boundaries_and_wrapped_rows() {
        let mut input = InputState::default();
        input.set("你好e\u{301}👩‍💻xy\nlast".into());
        input.set_cursor(input.position_at(1, 0, 4));

        assert_eq!(input.cursor, 0);

        input.set_cursor(input.position_at(2, 0, 4));

        assert_eq!(input.cursor, "你".len());

        input.set_cursor(input.position_at(1, 1, 4));

        assert_eq!(&input.text[input.cursor..], "👩‍💻xy\nlast");

        input.set_cursor(input.position_at(50, 50, 4));

        assert_eq!(input.cursor, input.text.len());
    }

    #[test]
    fn positions_wide_and_multiline_input() {
        let mut input = InputState::default();
        input.insert("你好ab\n世界");
        let (lines, cursor) = input.visual_lines(4);

        assert_eq!(lines, ["你好", "ab", "世界", ""]);
        assert_eq!(cursor, InputPosition { column: 0, row: 3 });
    }
}
