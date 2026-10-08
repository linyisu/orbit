//! Masked API-key entry. Secrets never enter the conversation or project config.
use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::{
    layout::Rect,
    text::Line,
    widgets::{Paragraph, Wrap},
};
use tokio_util::task::AbortOnDropHandle;

use super::connect_custom::form;
use super::{ModelOption, input::InputState, terminal};

type VerificationTask = AbortOnDropHandle<Result<(String, Vec<String>)>>;

pub(super) async fn run(
    terminal: &mut terminal::AppTerminal,
    events: &mut EventStream,
    options: &crate::ConnectionSettings,
    provider: orbit_auth::ApiKeyProvider,
) -> Result<Option<Vec<ModelOption>>> {
    let mut key = InputState::default();
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

            let inner = super::components::surface(
                frame,
                area,
                super::components::panel().title(format!(" {} API Key ", provider.label())),
            );
            let field = Rect::new(
                inner.x + 10,
                inner.y,
                inner.width.saturating_sub(10),
                u16::from(inner.height > 0),
            );

            frame.render_widget(
                Paragraph::new(vec![
                    Line::raw(" API Key:"),
                    Line::raw(""),
                    Line::raw(message.as_str()),
                    Line::raw(""),
                    Line::raw(" Enter 连接 · Esc 返回"),
                ])
                .wrap(Wrap { trim: false }),
                inner,
            );
            if field.width > 0 && field.height > 0 {
                frame.render_widget(
                    Paragraph::new(form::input_line(
                        &key,
                        field.width as usize,
                        true,
                        task.is_none(),
                        "",
                    )),
                    field,
                );
                if task.is_none() {
                    frame.set_cursor_position((
                        field.x + form::cursor_column(&key, field.width as usize, true) as u16,
                        field.y,
                    ));
                }
            }
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
                KeyCode::Enter if task.is_none() && !key.text.trim().is_empty() => {
                    let secret = key.text.trim().to_owned();
                    key.clear();
                    message = "正在验证密钥并读取模型…".into();
                    task = Some(AbortOnDropHandle::new(tokio::spawn(async move {
                        let ids = provider.verify(&secret).await?;
                        Ok((secret, ids))
                    })));
                }

                code if task.is_none() => form::edit(&mut key, code, event.modifiers),
                _ => {}
            },

            Event::Paste(text) if task.is_none() => form::paste(&mut key, text.trim()),
            _ => {}
        }
    }
}
