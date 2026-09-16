use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug, Default)]
pub struct InputState {
    pub text: String,
    pub cursor: usize,
    pub history: Vec<String>,
    history_index: Option<usize>,
    draft: String,
}

impl InputState {
    pub fn insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let text: String = text
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect();
        let text = text.replace('\t', "    ");
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
        self.history_index = None;
    }

    pub fn set(&mut self, text: String) {
        self.cursor = text.len();
        self.text = text;
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
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
            .map(|(i, _)| i)
            .unwrap_or(0);
    }

    pub fn move_right(&mut self) {
        self.cursor = self.text[self.cursor..]
            .grapheme_indices(true)
            .nth(1)
            .map(|(i, _)| self.cursor + i)
            .unwrap_or(self.text.len());
    }

    pub fn home(&mut self) {
        self.cursor = self.text[..self.cursor]
            .rfind('\n')
            .map(|i| i + 1)
            .unwrap_or(0);
    }

    pub fn end(&mut self) {
        self.cursor = self.text[self.cursor..]
            .find('\n')
            .map(|i| self.cursor + i)
            .unwrap_or(self.text.len());
    }

    pub fn take(&mut self) -> Option<String> {
        let value = self.text.trim().to_owned();
        if value.is_empty() {
            return None;
        }
        self.history.push(value.clone());
        self.clear();
        self.draft.clear();
        Some(value)
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
        if index + 1 >= self.history.len() {
            self.history_index = None;
            self.set(self.draft.clone());
        } else {
            self.history_index = Some(index + 1);
            self.set(self.history[index + 1].clone());
        }
    }

    pub fn visual_lines(&self, width: usize) -> (Vec<String>, (usize, usize)) {
        let width = width.max(2);
        let mut lines = vec![String::new()];
        let mut column = 0;
        let mut cursor = (0, 0);
        for (index, grapheme) in self.text.grapheme_indices(true) {
            let size = grapheme.width();
            if grapheme != "\n" && column + size > width {
                lines.push(String::new());
                column = 0;
            }
            if index == self.cursor {
                cursor = (column, lines.len() - 1);
            }
            if grapheme == "\n" {
                lines.push(String::new());
                column = 0;
            } else {
                lines.last_mut().unwrap().push_str(grapheme);
                column += size;
            }
        }
        if self.cursor == self.text.len() {
            if column == width {
                lines.push(String::new());
                column = 0;
            }
            cursor = (column, lines.len() - 1);
        }
        (lines, cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_draft_and_edits_graphemes() {
        let mut input = InputState::default();
        input.insert("你好👩‍💻");
        input.backspace();
        assert_eq!(input.text, "你好");
        assert_eq!(input.take().as_deref(), Some("你好"));
        input.insert("draft");
        input.history_up();
        input.history_down();
        assert_eq!(input.text, "draft");
    }

    #[test]
    fn positions_wide_and_multiline_input() {
        let mut input = InputState::default();
        input.insert("你好ab\n世界");
        let (lines, cursor) = input.visual_lines(4);
        assert_eq!(lines, ["你好", "ab", "世界", ""]);
        assert_eq!(cursor, (0, 3));
    }
}
