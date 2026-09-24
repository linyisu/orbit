//! Grapheme-aware prompt editing with a small local history.

use unicode_segmentation::UnicodeSegmentation;

#[derive(Default)]
pub struct Input {
    text: String,
    cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    draft: String,
}

impl Input {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn insert(&mut self, value: &str) {
        let value = value.replace("\r\n", "\n").replace('\r', "\n");
        let value: String = value
            .chars()
            .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
            .collect();
        self.text.insert_str(self.cursor, &value);
        self.cursor += value.len();
        self.history_index = None;
    }

    pub fn backspace(&mut self) {
        let end = self.cursor;
        self.move_left();
        self.text.drain(self.cursor..end);
    }

    pub fn delete(&mut self) {
        let start = self.cursor;
        self.move_right();
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }

    pub fn move_left(&mut self) {
        self.cursor = self.text[..self.cursor]
            .grapheme_indices(true)
            .next_back()
            .map_or(0, |(index, _)| index);
    }

    pub fn move_right(&mut self) {
        self.cursor = self.text[self.cursor..]
            .grapheme_indices(true)
            .nth(1)
            .map_or(self.text.len(), |(index, _)| self.cursor + index);
    }

    pub fn move_home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map_or(0, |index| index + 1);
    }

    pub fn move_end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |index| self.cursor + index);
    }

    pub fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        if self.history_index.is_none() {
            self.draft = self.text.clone();
        }
        let index = self
            .history_index
            .unwrap_or(self.history.len())
            .saturating_sub(1);
        self.history_index = Some(index);
        self.set(self.history[index].clone());
    }

    pub fn history_down(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 == self.history.len() {
            self.history_index = None;
            self.set(self.draft.clone());
        } else {
            self.history_index = Some(index + 1);
            self.set(self.history[index + 1].clone());
        }
    }

    pub fn take(&mut self) -> Option<String> {
        let submitted = self.text.trim().to_owned();
        if submitted.is_empty() {
            return None;
        }

        self.history.push(submitted.clone());
        self.clear();
        Some(submitted)
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.history_index = None;
        self.draft.clear();
    }

    pub fn replace(&mut self, text: String) {
        self.set(text);
        self.history_index = None;
    }

    fn set(&mut self, text: String) {
        self.cursor = text.len();
        self.text = text;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_graphemes_without_splitting_unicode() {
        let mut input = Input::default();
        input.insert("A👩‍💻中");
        input.move_left();
        input.backspace();
        assert_eq!(input.text(), "A中");
        input.delete();
        assert_eq!(input.text(), "A");
    }

    #[test]
    fn history_restores_the_draft() {
        let mut input = Input::default();
        input.insert("first");
        assert_eq!(input.take().as_deref(), Some("first"));
        input.insert("draft");
        input.history_up();
        assert_eq!(input.text(), "first");
        input.history_down();
        assert_eq!(input.text(), "draft");
    }
}
