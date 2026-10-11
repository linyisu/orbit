//! Local delivery feedback is separate from the canonical model transcript.
//! Receipts and consumption events are reconciled by identity, never by text.
use super::{
    components,
    markdown::LinkedLine,
    render::{fit_label, safe_multiline_output},
};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
};
use std::collections::VecDeque;

const VISIBLE_MESSAGES: usize = 3;
const PREVIEW_LINES: usize = 3;
const PREVIEW_CHARACTERS: usize = 4096;
const RECENT_RECEIPTS: usize = 64;

#[derive(Debug)]
struct Message {
    ticket: u64,
    message_id: Option<i64>,
    preview: String,
}

#[derive(Debug, Default)]
pub(super) struct State {
    messages: VecDeque<Message>,
    received: VecDeque<i64>,
    sequence: u64,
}

impl State {
    pub fn sending(&mut self, text: String) -> u64 {
        self.sequence += 1;
        // Delivery feedback only needs a bounded preview. The submission owns
        // the original message; do not retain another full draft here.
        let mut chars = text.chars();
        let mut preview: String = chars.by_ref().take(PREVIEW_CHARACTERS).collect();
        if chars.next().is_some() {
            preview.push('…');
        }
        self.messages.push_back(Message {
            ticket: self.sequence,
            message_id: None,
            preview: safe_multiline_output(&preview.replace("\r\n", "\n")),
        });
        self.sequence
    }

    pub fn queued(&mut self, ticket: u64, message_id: i64) {
        if self.received.contains(&message_id) {
            self.reject(ticket);
        } else if let Some(message) = self.messages.iter_mut().find(|m| m.ticket == ticket) {
            message.message_id = Some(message_id);
        }
    }

    pub fn reject(&mut self, ticket: u64) {
        self.messages.retain(|m| m.ticket != ticket);
    }

    pub fn received(&mut self, message_id: i64) {
        self.messages.retain(|m| m.message_id != Some(message_id));
        self.received.push_back(message_id);
        if self.received.len() > RECENT_RECEIPTS {
            self.received.pop_front();
        }
    }

    pub fn count(&self) -> usize {
        self.messages.len()
    }

    pub fn height(&self, width: u16) -> u16 {
        if self.messages.is_empty() || width <= 6 {
            return 0;
        }
        1 + self
            .messages
            .iter()
            .rev()
            .take(VISIBLE_MESSAGES)
            .map(|message| preview_lines(&message.preview, width - 6, PREVIEW_LINES).len() as u16)
            .sum::<u16>()
    }

    pub fn draw(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.width <= 6 || area.height < 2 || self.messages.is_empty() {
            return;
        }
        let sending = self
            .messages
            .iter()
            .filter(|message| message.message_id.is_none())
            .count();
        let queued = self.count() - sending;
        let status = match (queued, sending) {
            (0, _) => format!("发送中 · {sending} 条"),
            (_, 0) => format!("等待处理 · {queued} 条"),
            _ => format!("等待处理 {queued} 条 · 发送中 {sending} 条"),
        };
        let mut lines = vec![Line::styled(
            format!(
                "  {}",
                fit_label(&status, area.width.saturating_sub(4) as usize, false)
            ),
            components::muted(),
        )];
        let visible = self
            .count()
            .min(VISIBLE_MESSAGES)
            .min(area.height.saturating_sub(1) as usize);
        for (index, message) in self
            .messages
            .iter()
            .skip(self.count() - visible)
            .enumerate()
        {
            let budget =
                ((area.height as usize - lines.len()) / (visible - index)).min(PREVIEW_LINES);
            for (row, line) in preview_lines(&message.preview, area.width - 6, budget)
                .into_iter()
                .enumerate()
            {
                let mut spans = vec![Span::styled(
                    if row == 0 { "  › " } else { "    " },
                    components::accent(),
                )];
                spans.extend(line.spans);
                lines.push(Line::from(spans).style(components::normal()));
            }
        }
        frame.render_widget(Paragraph::new(lines), area);
    }
}

fn preview_lines(text: &str, width: u16, limit: usize) -> Vec<Line<'static>> {
    if width == 0 || limit == 0 {
        return Vec::new();
    }
    let mut rows = text
        .split('\n')
        .flat_map(|line| LinkedLine::plain(Line::from(line.to_owned())).wrap(width))
        .take(limit + 1)
        .map(|line| line.line)
        .collect::<Vec<_>>();
    if rows.len() > limit {
        rows.truncate(limit);
        let last = rows.last_mut().unwrap();
        *last = Line::from(format!(
            "{}…",
            fit_label(&last.to_string(), width.saturating_sub(1) as usize, false)
        ));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn pending_preview_keeps_newlines_and_literal_backslashes_with_status_separate() {
        let mut state = State::default();
        let ticket = state.sending("blink.cmp\r\n中文问题\n\\n is literal".into());
        state.queued(ticket, 42);
        let mut terminal = Terminal::new(TestBackend::new(50, 8)).unwrap();
        terminal
            .draw(|frame| state.draw(frame, Rect::new(0, 0, 50, state.height(50))))
            .unwrap();
        let rows: Vec<String> = (0..4)
            .map(|row| {
                (0..50)
                    .map(|col| terminal.backend().buffer()[(col, row)].symbol())
                    .collect()
            })
            .collect();
        assert!(rows[0].replace(' ', "").contains("等待处理"));
        assert!(!rows[0].contains("blink.cmp"));
        assert!(rows[1].contains("blink.cmp") && !rows[1].contains("\\n"));
        assert!(rows[2].replace(' ', "").contains("中文问题"));
        assert!(rows[3].contains("\\n is literal"));
        state.received(42);
        assert_eq!(state.height(50), 0);
    }

    #[test]
    fn previews_wrap_unicode_and_keep_recent_messages_visible_in_small_allocations() {
        let mut state = State::default();
        state.sending("中文问题 ".repeat(PREVIEW_CHARACTERS));
        assert!(state.messages[0].preview.chars().count() <= PREVIEW_CHARACTERS + 1);
        let rows = preview_lines(&state.messages[0].preview, 12, PREVIEW_LINES);
        assert!(rows.len() <= PREVIEW_LINES);
        assert!(rows.iter().all(|row| row.to_string().width() <= 12));
        assert!(rows.last().unwrap().to_string().ends_with('…'));
        for name in ["older", "recent", "newest"] {
            state.sending(name.into());
        }
        let mut terminal = Terminal::new(TestBackend::new(30, 4)).unwrap();
        terminal
            .draw(|frame| state.draw(frame, Rect::new(0, 0, 30, 3)))
            .unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("recent") && screen.contains("newest"));
        assert!(!screen.contains("older"));
    }
}
