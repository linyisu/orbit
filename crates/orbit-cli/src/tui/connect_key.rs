//! Masked API-key entry. Secrets never enter the conversation or project config.
use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::{
    layout::Rect,
    style::Style,
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
};
use tokio_util::task::AbortOnDropHandle;

use super::{ModelOption, terminal};

type VerificationTask = AbortOnDropHandle<Result<(String, Vec<String>)>>;

pub(super) async fn run(
    terminal: &mut terminal::AppTerminal,
    events: &mut EventStream,
    options: &crate::TuiOptions,
    provider: orbit_auth::ApiKeyProvider,
) -> Result<Option<Vec<ModelOption>>> {
    let mut key = String::new();
    let mut message = String::new();
    let mut task: Option<VerificationTask> = None;

    loop {
        terminal::draw_synchronized(terminal, |frame| {
            let bounds = frame.area();
            let width = bounds.width.saturating_sub(4).min(76);
            let height = bounds.height.saturating_sub(2).min(9);
            let area = Rect::new(
                bounds.x + bounds.width.saturating_sub(width) / 2,
                bounds.y + bounds.height.saturating_sub(height) / 2,
                width,
                height,
            );
            super::overlay::clear(frame, area);
            let block = Block::default()
                .borders(Borders::ALL)
                .title(format!(" {} API Key ", provider.label()))
                .border_style(Style::default().fg(crate::tui::theme::THEME.dialog_accent));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let mask = "*".repeat(
                key.chars()
                    .count()
                    .min(inner.width.saturating_sub(10) as usize),
            );
            frame.render_widget(
                Paragraph::new(vec![
                    Line::raw(format!(" API Key: {mask}")),
                    Line::raw(""),
                    Line::raw(message.as_str()),
                    Line::raw(""),
                    Line::raw(" Enter 连接 · Esc 返回"),
                ])
                .wrap(Wrap { trim: false }),
                inner,
            );
        })?;

        let event = if let Some(handle) = task.as_mut() {
            tokio::select! {
                result = handle => {
                    task = None;
                    match result? {
                        Ok((secret, ids)) => {
                            return crate::connections::save_api_key(
                                &options.config, &options.auth_directory, provider, secret, ids,
                            ).await.map(Some);
                        }
                        Err(error) => message = format!("连接失败：{error}"),
                    }
                    continue;
                }
                event = events.next() => event,
            }
        } else {
            events.next().await
        };
        let Some(event) = event else {
            return Ok(None);
        };
        match event? {
            Event::Key(event) if event.kind != KeyEventKind::Release => match event.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Ok(None);
                }
                KeyCode::Enter if task.is_none() && !key.trim().is_empty() => {
                    let secret = std::mem::take(&mut key).trim().to_owned();
                    message = "正在验证密钥并读取模型…".into();
                    task = Some(AbortOnDropHandle::new(tokio::spawn(async move {
                        let ids = provider.verify(&secret).await?;
                        Ok((secret, ids))
                    })));
                }
                KeyCode::Backspace if task.is_none() => {
                    key.pop();
                }
                KeyCode::Char(character)
                    if task.is_none()
                        && !event
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && !character.is_control()
                        && key.len() + character.len_utf8() <= 16_384 =>
                {
                    key.push(character);
                }
                _ => {}
            },
            Event::Paste(text) if task.is_none() => {
                for character in text.trim().chars().filter(|ch| !ch.is_control()) {
                    if key.len() + character.len_utf8() > 16_384 {
                        break;
                    }
                    key.push(character);
                }
            }
            _ => {}
        }
    }
}
