use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{Paragraph, Wrap},
};

use crate::{
    tui::{components, connect_custom::form, input::InputState, render},
    workspace::interaction::Challenge,
};

pub(crate) struct Prompt {
    challenge: Option<Challenge>,
    input: InputState,
}

impl std::fmt::Debug for Prompt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SshPrompt { input: [redacted] }")
    }
}

impl Prompt {
    pub fn new(challenge: Challenge) -> Self {
        Self {
            challenge: Some(challenge),
            input: InputState::default(),
        }
    }

    pub fn closed(&self) -> bool {
        self.challenge
            .as_ref()
            .is_none_or(|challenge| challenge.reply.is_closed())
    }

    pub fn handle(&mut self, event: &Event) -> bool {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc | KeyCode::Char('c')
                    if key.code == KeyCode::Esc
                        || key
                            .modifiers
                            .contains(KeyModifiers::CONTROL) =>
                {
                    return true;
                }
                KeyCode::Enter => {
                    if let Some(challenge) = self.challenge.take() {
                        let value = std::mem::take(&mut self.input.text);
                        let _ = challenge.reply.send(Some(value));
                    }
                }
                _ => form::edit(&mut self.input, key.code, key.modifiers),
            },
            Event::Paste(text) => form::paste(&mut self.input, text),
            _ => {}
        }
        false
    }

    pub fn draw(&self, frame: &mut Frame<'_>) {
        let Some(challenge) = &self.challenge else {
            return;
        };
        let bounds = frame.area();
        let width = bounds.width.saturating_sub(4).min(88);
        let prompt = render::safe_multiline_output(&challenge.prompt);
        let text = Paragraph::new(prompt).wrap(Wrap { trim: false });
        let lines = text
            .line_count(width.saturating_sub(4))
            .min(18) as u16;
        let height = (lines + 6).min(bounds.height.saturating_sub(2));
        let area = Rect::new(
            bounds.x + (bounds.width - width) / 2,
            bounds.y + (bounds.height - height) / 2,
            width,
            height,
        );
        components::surface(frame, area, components::panel().title(" SSH 认证 "));

        let inner = area.inner(ratatui::layout::Margin::new(2, 1));
        frame.render_widget(
            text,
            Rect::new(
                inner.x,
                inner.y,
                inner.width,
                inner.height.saturating_sub(3),
            ),
        );
        let field = Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width, 1);
        frame.render_widget(
            Paragraph::new(form::input_line(
                &self.input,
                field.width as usize,
                !challenge.confirm,
                true,
                if challenge.confirm {
                    "输入 yes 确认主机，其他回答拒绝"
                } else {
                    "输入密码／私钥口令／验证码"
                },
            )),
            field,
        );
        frame.render_widget(
            Paragraph::new(Line::from("Enter 提交 · Esc 取消连接 · 可切换其他标签")),
            Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
        );
        if field.width > 0 {
            frame.set_cursor_position((
                field.x
                    + (form::cursor_column(&self.input, field.width as usize, !challenge.confirm)
                        as u16)
                        .min(field.width - 1),
                field.y,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn password_is_masked_and_never_enters_debug_output_or_input_history() {
        let (reply, mut answer) = tokio::sync::oneshot::channel();
        let mut prompt = Prompt::new(Challenge {
            prompt: "test password:".into(),
            confirm: false,
            reply,
        });
        prompt.handle(&Event::Paste("private-test-secret".into()));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| prompt.draw(frame))
            .unwrap();
        let rendered = format!("{:?}", terminal.backend());
        assert!(!rendered.contains("private-test-secret"));
        assert!(!format!("{prompt:?}").contains("private-test-secret"));
        prompt.handle(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        assert_eq!(
            answer.try_recv().unwrap().as_deref(),
            Some("private-test-secret")
        );
        assert!(prompt.input.history.is_empty());
        assert!(prompt.input.text.is_empty());
        assert!(prompt.closed());
    }
}
