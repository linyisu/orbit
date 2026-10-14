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
    reasoning_adapter: Option<ReasoningAdapter>,
    reasoning_extra: &Value,
    stream: bool,
) -> Result<(Value, HeaderMap), ModelError> {
    let reasoning_effort = request.reasoning_effort;
    let system = request
        .messages
        .iter()
        .filter_map(|message| match message {
            ChatMessage::System { content } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut messages: Vec<Value> = Vec::new();

    for message in request.messages {
        match message {
            ChatMessage::Runtime { kind, content } => {
                let block = json!({
                    "type": "text",
                    "text": crate::runtime_context::render(kind, &content)
                });
                if let Some(last) = messages
                    .last_mut()
                    .filter(|message| message["role"] == "user")
                {
                    last["content"]
                        .as_array_mut()
                        .expect("user content blocks")
                        .push(block);
                } else {
                    messages.push(json!({
                        "role": "user",
                        "content": [block]
                    }));
                }
            }
            ChatMessage::System { .. } => {}

            ChatMessage::User {
                content,
                attachments,
            } => {
                let mut blocks = vec![json!({
                    "type": "text",
                    "text": if content.is_empty() {
                        "[图片]"
                    } else {
                        &content
                    }
                })];

                for attachment in attachments {
                    let Some((media_type, data)) = attachment.image_if_available()? else {
                        continue;
                    };
                    blocks.push(json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": media_type,
                            "data": data
                        }
                    }));
                }

                if let Some(last) = messages.last_mut().filter(|message| {
                    message["role"] == "user"
                        && message["content"]
                            .as_array()
                            .is_some_and(|content| {
                                content
                                    .first()
                                    .is_some_and(|block| block["type"] == "tool_result")
                            })
                }) {
                    last["content"]
                        .as_array_mut()
                        .expect("user content blocks")
                        .extend(blocks);
                } else {
                    messages.push(json!({
                        "role": "user",
                        "content": blocks
                    }));
                }
            }

            ChatMessage::Assistant {
                content,
                tool_calls,
            } => {
                let mut blocks = Vec::new();

                if let Some(content) = content {
                    blocks.push(json!({
                        "type": "text",
                        "text": content
                    }));
                }

                blocks.extend(tool_calls.into_iter().map(|call| {
                    json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.function.name,
                        "input": serde_json::from_str::<Value>(
                            &call.function.arguments
                        )
                        .unwrap_or_else(|_| json!({}))
                    })
                }));
                messages.push(json!({
                    "role": "assistant",
                    "content": blocks
                }));
            }

            ChatMessage::Tool {
                tool_call_id,
                content,
            } => {
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": tool_call_id,
                    "content": content
                });

                // All results for one assistant tool batch must be in the very
                // next user message, not separate messages per tool result.
                if let Some(last) = messages.last_mut().filter(|m| {
                    m["role"] == "user"
                        && m["content"]
                            .as_array()
                            .is_some_and(|blocks| {
                                blocks
                                    .first()
                                    .is_some_and(|b| b["type"] == "tool_result")
                            })
                }) {
                    last["content"]
                        .as_array_mut()
                        .expect("result blocks")
                        .push(block);
                } else {
                    messages.push(json!({
                        "role": "user",
                        "content": [block]
                    }));
                }
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
                "input_schema": tool.function.parameters
            })
        })
        .collect::<Vec<_>>();
    // Anthropic requires max_tokens even when the caller leaves it unset.
    let mut body = json!({
        "model": model,
        "max_tokens": request.max_tokens.unwrap_or(4_096),
        "system": system,
        "messages": messages,
        "cache_control": {
            "type": "ephemeral"
        },
        "stream": stream
    });

    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }

    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }

    if let (Some(effort), Some(ReasoningAdapter::AnthropicMessages)) =
        (reasoning_effort, reasoning_adapter)
    {
        crate::reasoning::apply_anthropic_messages(&mut body, effort, reasoning_extra);
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

    Ok((body, headers))
}

pub(crate) async fn decode_completion(response: Response) -> Result<ChatCompletion, ModelError> {
    let value: Value = response.json().await?;

    if let Some(error) = crate::client::upstream_error(&value) {
        return Err(error);
    }

    let payload: AnthropicResponse = serde_json::from_value(value)?;
    let mut text = String::new();
    let mut calls = Vec::new();

    for block in payload.content {
        match block.kind.as_str() {
            "text" => text.push_str(
                block
                    .text
                    .as_deref()
                    .unwrap_or_default(),
            ),
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

        match event.kind.as_str() {
            "message_start" => {
                if let Some(usage) = event
                    .message
                    .and_then(|message| message.usage)
                {
                    events.push(StreamEvent::Completed {
                        finish_reason: None,
                        usage: Some(usage.into()),
                    });
                }
            }

            "content_block_start" => {
                if let Some(block) = event.content_block {
                    match block.kind.as_str() {
                        "tool_use" => events.push(StreamEvent::ToolCallDelta(ToolCallDelta {
                            metadata: None,
                            index: event.index.unwrap_or_default(),
                            id: block.id,
                            name: block.name,
                            arguments: block
                                .input
                                .filter(|v| {
                                    v.as_object()
                                        .is_some_and(|m| !m.is_empty())
                                })
                                .map(|v| v.to_string())
                                .unwrap_or_default(),
                        })),

                        "text" => {
                            if let Some(text) = block.text.filter(|s| !s.is_empty()) {
                                events.push(StreamEvent::TextDelta(text));
                            }
                        }

                        "thinking" => {
                            if let Some(text) = block.thinking.filter(|s| !s.is_empty()) {
                                events.push(StreamEvent::ReasoningDelta(text));
                            }
                        }

                        _ => {}
                    }
                }
            }

            "content_block_delta" => {
                if let Some(delta) = event.delta {
                    match delta.kind.as_deref() {
                        Some("thinking_delta") => {
                            if let Some(text) = delta.thinking {
                                events.push(StreamEvent::ReasoningDelta(text));
                            }
                        }

                        Some("text_delta") => {
                            if let Some(text) = delta.text {
                                events.push(StreamEvent::TextDelta(text));
                            }
                        }

                        Some("input_json_delta") => {
                            if let Some(json) = delta.partial_json {
                                events.push(StreamEvent::ToolCallDelta(ToolCallDelta {
                                    metadata: None,
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
                        usage: event
                            .usage
                            .or(delta.usage)
                            .map(Into::into),
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
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
    thinking: Option<String>,
    id: Option<String>,
    name: Option<String>,
    input: Option<Value>,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

impl From<AnthropicUsage> for Usage {
    fn from(value: AnthropicUsage) -> Self {
        Self {
            // Anthropic counts uncached input separately from cache reads and
            // writes. Context occupancy is their sum, not just billed new input.
            prompt_tokens: if value.input_tokens.is_some()
                || value.cache_read_input_tokens.is_some()
                || value
                    .cache_creation_input_tokens
                    .is_some()
            {
                Some(
                    value
                        .input_tokens
                        .unwrap_or(0)
                        .saturating_add(
                            value
                                .cache_read_input_tokens
                                .unwrap_or(0),
                        )
                        .saturating_add(
                            value
                                .cache_creation_input_tokens
                                .unwrap_or(0),
                        ),
                )
            } else {
                None
            },
            completion_tokens: value.output_tokens,
            total_tokens: None,
            cached_input_tokens: value.cache_read_input_tokens,
            cache_write_input_tokens: value.cache_creation_input_tokens,
        }
    }
}

#[derive(Deserialize)]
struct AnthropicEvent {
    #[serde(rename = "type")]
    kind: String,
    index: Option<usize>,
    content_block: Option<AnthropicBlock>,
    delta: Option<AnthropicDelta>,
    usage: Option<AnthropicUsage>,
    message: Option<AnthropicMessageStart>,
}

#[derive(Deserialize)]
struct AnthropicMessageStart {
    usage: Option<AnthropicUsage>,
}

#[derive(Deserialize)]
struct AnthropicDelta {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
    thinking: Option<String>,
    partial_json: Option<String>,
    stop_reason: Option<String>,
    usage: Option<AnthropicUsage>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_max_effort_with_the_configured_thinking_mode() {
        for extra in [
            json!({}),
            json!({
                "thinking": {
                    "type": "enabled"
                }
            }),
        ] {
            let mut request = ChatRequest::new(vec![ChatMessage::user("think")]);
            request.reasoning_effort = Some(ReasoningEffort::Max);

            let (body, _) = encode(
                "claude-test",
                request,
                Some(ReasoningAdapter::AnthropicMessages),
                &extra,
                true,
            )
            .unwrap();

            assert_eq!(body["output_config"]["effort"], "max");
            assert_eq!(
                body["thinking"]["type"],
                extra["thinking"]["type"]
                    .as_str()
                    .unwrap_or("adaptive")
            );
        }
    }

    #[test]
    fn includes_adaptive_thinking_effort_when_configured() {
        let mut request = ChatRequest::new(vec![ChatMessage::user("think")]);
        request.reasoning_effort = Some(ReasoningEffort::High);
        let (body, _) = encode(
            "claude-test",
            request,
            Some(ReasoningAdapter::AnthropicMessages),
            &json!({}),
            true,
        )
        .unwrap();

        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"]["effort"], "high");
    }

    #[test]
    fn model_profile_extra_can_override_the_adaptive_thinking_switch() {
        let mut request = ChatRequest::new(vec![ChatMessage::user("think")]);
        request.reasoning_effort = Some(ReasoningEffort::High);
        let (body, _) = encode(
            "gateway-name",
            request,
            Some(ReasoningAdapter::AnthropicMessages),
            &json!({
                "thinking": {
                    "type": "enabled"
                }
            }),
            true,
        )
        .unwrap();

        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["output_config"]["effort"], "high");
    }

    #[test]
    fn model_profile_extra_merges_nested_provider_options_without_losing_effort() {
        let mut request = ChatRequest::new(vec![ChatMessage::user("think")]);
        request.reasoning_effort = Some(ReasoningEffort::High);
        let (body, _) = encode(
            "gateway-name",
            request,
            Some(ReasoningAdapter::AnthropicMessages),
            &json!({
                "output_config": {
                    "provider_option": true
                }
            }),
            true,
        )
        .unwrap();

        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["output_config"]["provider_option"], true);
    }

    #[test]
    fn batches_all_tool_results_in_the_immediately_following_user_message() {
        let (body, _) = encode(
            "test",
            ChatRequest::new(vec![
                ChatMessage::user("read two files"),
                ChatMessage::assistant_tool_calls(vec![
                    ToolCall::function(
                        "a",
                        "read_file",
                        json!({
                            "path": "a"
                        }),
                    ),
                    ToolCall::function(
                        "b",
                        "read_file",
                        json!({
                            "path": "b"
                        }),
                    ),
                ]),
                ChatMessage::tool("a", "first"),
                ChatMessage::tool("b", "tool error: not found"),
                ChatMessage::assistant("done"),
                ChatMessage::user("another request"),
                ChatMessage::assistant_tool_calls(vec![ToolCall::function(
                    "c",
                    "read_file",
                    json!({
                        "path": "c"
                    }),
                )]),
                ChatMessage::tool("c", "third"),
            ]),
            None,
            &json!({}),
            true,
        )
        .unwrap();
        let messages = body["messages"].as_array().unwrap();

        assert_eq!(messages.len(), 7);
        assert_eq!(messages[2]["role"], "user");

        let results = messages[2]["content"]
            .as_array()
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["tool_use_id"], "a");
        assert_eq!(results[1]["tool_use_id"], "b");
        assert_eq!(results[1]["content"], "tool error: not found");
        assert_eq!(messages[3]["role"], "assistant");
        assert_eq!(messages[6]["content"][0]["tool_use_id"], "c");
    }

    #[test]
    fn missing_historical_image_keeps_text_and_allows_the_next_turn() {
        let root = tempfile::tempdir().unwrap();
        let (body, _) = encode(
            "vision-model",
            ChatRequest::new(vec![
                ChatMessage::user_with_attachments(
                    "previous question",
                    vec![crate::types::Attachment::Image {
                        name: "gone.png".into(),
                        mime_type: "image/png".into(),
                        path: root.path().join("gone.png"),
                        lease: None,
                    }],
                ),
                ChatMessage::user("continue"),
            ]),
            None,
            &json!({}),
            true,
        )
        .unwrap();
        assert_eq!(
            body["messages"][0]["content"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(body["messages"][1]["role"], "user");
        assert!(!body.to_string().contains("gone.png"));
    }

    #[test]
    fn encodes_local_images_as_anthropic_source_blocks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pixel.png");
        std::fs::write(&path, b"image-bytes").unwrap();
        let (body, _) = encode(
            "vision-model",
            ChatRequest::new(vec![ChatMessage::user_with_attachments(
                "inspect",
                vec![crate::types::Attachment::Image {
                    lease: None,
                    name: "pixel.png".into(),
                    mime_type: "image/png".into(),
                    path,
                }],
            )]),
            None,
            &json!({}),
            true,
        )
        .unwrap();

        assert_eq!(body["messages"][0]["content"][1]["type"], "image");
        assert_eq!(
            body["messages"][0]["content"][1]["source"]["media_type"],
            "image/png"
        );
    }

    #[test]
    fn loaded_image_follows_tool_result_in_the_same_user_message() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("scene.png");
        std::fs::write(&path, b"image-bytes").unwrap();
        let (body, _) = encode(
            "vision-model",
            ChatRequest::new(vec![
                ChatMessage::assistant_tool_calls(vec![ToolCall::function(
                    "image-call",
                    "read_image",
                    json!({
                        "path": "scene.png"
                    }),
                )]),
                ChatMessage::tool("image-call", r#"{"loaded":true}"#),
                ChatMessage::user_with_attachments(
                    "Analyze scene.png",
                    vec![crate::types::Attachment::Image {
                        lease: None,
                        name: "scene.png".into(),
                        mime_type: "image/png".into(),
                        path,
                    }],
                ),
            ]),
            None,
            &json!({}),
            true,
        )
        .unwrap();
        let messages = body["messages"].as_array().unwrap();

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1]["content"][0]["type"], "tool_result");
        assert_eq!(messages[1]["content"][1]["type"], "text");
        assert_eq!(messages[1]["content"][2]["type"], "image");
    }
}

#[cfg(test)]
mod cache_usage_tests {
    use super::*;
    #[test]
    fn context_usage_includes_cache_reads_and_writes_but_missing_input_stays_unknown() {
        let usage: Usage = serde_json::from_value::<AnthropicUsage>(serde_json::json!({
            "input_tokens": 12,
            "cache_read_input_tokens": 1_000,
            "cache_creation_input_tokens": 200,
            "output_tokens": 9
        }))
        .unwrap()
        .into();

        assert_eq!(usage.prompt_tokens, Some(1_212));
        assert_eq!(usage.cached_input_tokens, Some(1_000));
        assert_eq!(usage.cache_write_input_tokens, Some(200));

        let tail: Usage = serde_json::from_value::<AnthropicUsage>(serde_json::json!({
            "output_tokens": 10
        }))
        .unwrap()
        .into();

        assert_eq!(tail.prompt_tokens, None);
    }
}
