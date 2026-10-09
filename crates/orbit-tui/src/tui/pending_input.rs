//! Local delivery feedback is separate from the canonical model transcript.
//! Receipts and consumption events are reconciled by identity, never by text.
use super::{
    components,
    render::{fit_label, safe_output},
};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
};
use std::collections::VecDeque;

const VISIBLE_MESSAGES: usize = 3;
const RECENT_RECEIPTS: usize = 64;

#[derive(Debug)]
struct Message {
    ticket: u64,
    message_id: Option<i64>,
    text: String,
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
        self.messages.push_back(Message {
            ticket: self.sequence,
            message_id: None,
            text,
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

    pub fn height(&self) -> u16 {
        (self.messages.len().min(VISIBLE_MESSAGES)
            + usize::from(self.messages.len() > VISIBLE_MESSAGES)) as u16
    }

    pub fn draw(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.is_empty() {
            return;
        }
        let mut lines = Vec::new();
        if self.messages.len() > VISIBLE_MESSAGES {
            lines.push(Line::styled(
                format!("  待处理消息 · {} 条", self.messages.len()),
                components::muted(),
            ));
        }
        for message in self
            .messages
            .iter()
            .skip(self.messages.len().saturating_sub(VISIBLE_MESSAGES))
        {
            let status = if message.message_id.is_some() {
                " · 已发送，等待处理"
            } else {
                " · 发送中…"
            };
            let width = area
                .width
                .saturating_sub(unicode_width::UnicodeWidthStr::width(status) as u16 + 4)
                as usize;
            // Do not scan/allocate a potentially megabyte-sized draft on each frame.
            let mut chars = message.text.chars();
            let mut preview = chars.by_ref().take(width).collect::<String>();
            if chars.next().is_some() {
                preview.push('…');
            }
            lines.push(Line::from(vec![
                Span::styled("  › ", components::accent()),
                Span::styled(
                    fit_label(&safe_output(&preview), width, false),
                    components::normal(),
                ),
                Span::styled(status, components::muted()),
            ]));
        }
        frame.render_widget(Paragraph::new(lines), area);
    }
}
