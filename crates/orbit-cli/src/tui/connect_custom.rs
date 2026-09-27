//! Service setup and model selection, isolated from the conversation transcript.
mod form;
mod model;
mod models;
mod panel;

use anyhow::Result;
use crossterm::event::EventStream;
use orbit_model::client::ApiFormat;

use super::{ModelOption, terminal};
use crate::connections::edit::ConnectionDraft;
use form::Field;

const FORMATS: [&str; 3] = [
    "OpenAI Chat Completions",
    "OpenAI Responses",
    "Anthropic Messages",
];

fn format_name(format: ApiFormat) -> &'static str {
    match format {
        ApiFormat::OpenaiChatCompletions => FORMATS[0],
        ApiFormat::OpenaiResponses => FORMATS[1],
        ApiFormat::AnthropicMessages => FORMATS[2],
        ApiFormat::GoogleCodeAssist => "Google Code Assist",
        ApiFormat::ChatgptCodex => "ChatGPT Codex",
    }
}

pub(super) async fn run(
    terminal: &mut terminal::AppTerminal,
    events: &mut EventStream,
    options: &crate::TuiOptions,
    existing: Option<&str>,
) -> Result<Option<Vec<ModelOption>>> {
    let mut draft = ConnectionDraft::load(&options.config, existing)?;
    let mut name = Field::text("连接名称", &draft.name, "例如：本地服务、工作网关");
    name.readonly = existing.is_some();
    let mut protocol = Field::choice("API 协议", format_name(draft.format), &FORMATS);
    protocol.readonly = draft.account();
    let mut endpoint = Field::text("Base URL", &draft.endpoint, "例如 http://localhost:8000/v1");
    endpoint.readonly = draft.account();
    let saved_key = draft
        .provider
        .as_ref()
        .is_some_and(|provider| provider.credential.is_some() || !provider.api_key.is_empty());
    let mut key = Field::text(
        "API Key",
        "",
        if saved_key {
            "已保存 API Key，留空保留，输入新密钥替换"
        } else {
            "输入 API Key"
        },
    );
    key.secret = true;
    key.readonly = draft.account();
    panel::run(
        terminal,
        events,
        options,
        &mut draft,
        vec![name, endpoint, protocol, key],
    )
    .await
}

pub(super) async fn manage(
    terminal: &mut terminal::AppTerminal,
    events: &mut EventStream,
    options: &crate::TuiOptions,
) -> Result<Option<Vec<ModelOption>>> {
    let names = crate::connections::edit::saved_connections(&options.config)?;
    if names.is_empty() {
        return run(terminal, events, options, None).await;
    }
    if let Some(name) = form::choose(terminal, events, "选择已有连接", &names, &names[0]).await?
    {
        return run(terminal, events, options, Some(&name)).await;
    }
    Ok(None)
}
