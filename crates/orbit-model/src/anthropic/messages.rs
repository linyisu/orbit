use std::time::Duration;

use reqwest::{
    Response,
    header::{ACCEPT, HeaderMap, HeaderValue},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{client::sse_stream, error::ModelError, types::*};

pub(crate) fn encode(
    model: &str,
    request: ChatRequest,
    adapter: Option<ReasoningAdapter>,
    extra: &Value,
    stream: bool,
) -> (Value, HeaderMap) {
    let effort = request.reasoning_effort;
    let system = request
        .messages
        .iter()
        .filter_map(|message| match message {
            ChatMessage::System { content } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut messages = Vec::new();
    for message in request.messages {
        match message {
            ChatMessage::System { .. } => {}
            ChatMessage::User { content } => {
                messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "text",
                        "text": content,
                    }],
                }));
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
            } => {
                let mut blocks = Vec::new();
                if let Some(content) = content {
                    blocks.push(json!({
                        "type": "text",
                        "text": content,
                    }));
                }

                for call in tool_calls {
                    let input = serde_json::from_str::<Value>(&call.function.arguments)
                        .unwrap_or_else(|_| json!({}));
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.function.name,
                        "input": input,
                    }));
                }

                messages.push(json!({
                    "role": "assistant",
                    "content": blocks,
                }));
            }
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => {
                messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": tool_call_id,
                        "content": content,
                    }],
                }));
            }
        }
    }

    let tools = request
        .tools
        .into_iter()
        .map(|tool| {
            json!({
                "name": tool.function.name,
                "description": tool.function.description,
                "input_schema": tool.function.parameters,
            })
        })
        .collect::<Vec<_>>();

    let mut body = json!({
        "model": model,
        "max_tokens": request.max_tokens.unwrap_or(4096),
        "system": system,
        "messages": messages,
        "tools": tools,
        "temperature": request.temperature,
        "stream": stream,
    });
    if let (Some(effort), Some(ReasoningAdapter::AnthropicMessages)) = (effort, adapter) {
        crate::reasoning::apply_anthropic_messages(&mut body, effort, extra);
    }

    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static(if stream {
            "text/event-stream"
        } else {
            "application/json"
        }),
    );
    headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));

    (body, headers)
}

pub(crate) async fn decode_completion(response: Response) -> Result<ChatCompletion, ModelError> {
    let payload: AnthropicResponse = response.json().await?;
    let mut text = String::new();
    let mut calls = Vec::new();

    for block in payload.content {
        match block.r#type.as_str() {
            "text" => text.push_str(block.text.as_deref().unwrap_or_default()),
            "tool_use" => {
                if let (Some(id), Some(name)) = (block.id, block.name) {
                    calls.push(ToolCall::function(
                        id,
                        name,
                        block.input.unwrap_or_else(|| json!({})),
                    ));
                }
            }
            _ => {}
        }
    }

    Ok(ChatCompletion {
        id: payload.id,
        model: payload.model,
        message: ChatMessage::Assistant {
            content: (!text.is_empty()).then_some(text),
            tool_calls: calls,
        },
        finish_reason: payload.stop_reason,
        usage: payload.usage.map(Into::into),
    })
}

pub(crate) fn stream(
    response: Response,
    timeout: Duration,
    cancellation: CancellationToken,
) -> ModelStream {
    sse_stream(response, timeout, cancellation, |data| {
        let event: AnthropicEvent = serde_json::from_str(data)?;
        let mut events = Vec::new();

        match event.r#type.as_str() {
            "content_block_start" => {
                if let Some(block) = event
                    .content_block
                    .filter(|block| block.r#type == "tool_use")
                {
                    events.push(StreamEvent::ToolCallDelta(ToolCallDelta {
                        index: event.index.unwrap_or_default(),
                        id: block.id,
                        name: block.name,
                        arguments: String::new(),
                    }));
                }
            }
            "content_block_delta" => {
                if let Some(delta) = event.delta {
                    match delta.r#type.as_deref() {
                        Some("text_delta") => {
                            if let Some(text) = delta.text {
                                events.push(StreamEvent::TextDelta(text));
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(json) = delta.partial_json {
                                events.push(StreamEvent::ToolCallDelta(ToolCallDelta {
                                    index: event.index.unwrap_or_default(),
                                    id: None,
                                    name: None,
                                    arguments: json,
                                }));
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(delta) = event.delta {
                    events.push(StreamEvent::Completed {
                        finish_reason: delta.stop_reason,
                        usage: delta.usage.map(Into::into),
                    });
                }
            }
            _ => {}
        }

        Ok(events)
    })
}

#[derive(Deserialize)]
struct AnthropicResponse {
    id: Option<String>,
    model: Option<String>,
    content: Vec<AnthropicBlock>,
    stop_reason: Option<String>,
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize)]
struct AnthropicBlock {
    r#type: String,
    text: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<Value>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

impl From<AnthropicUsage> for Usage {
    fn from(value: AnthropicUsage) -> Self {
        Self {
            prompt_tokens: value.input_tokens,
            completion_tokens: value.output_tokens,
            total_tokens: None,
        }
    }
}

#[derive(Deserialize)]
struct AnthropicEvent {
    r#type: String,
    index: Option<usize>,
    content_block: Option<AnthropicBlock>,
    delta: Option<AnthropicDelta>,
}

#[derive(Deserialize)]
struct AnthropicDelta {
    r#type: Option<String>,
    text: Option<String>,
    partial_json: Option<String>,
    stop_reason: Option<String>,
    usage: Option<AnthropicUsage>,
}
