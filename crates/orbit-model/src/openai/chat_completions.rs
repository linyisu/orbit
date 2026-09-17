use std::time::Duration;

use reqwest::{
    Response,
    header::{ACCEPT, HeaderMap, HeaderValue},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{
    client::sse_stream,
    error::ModelError,
    types::{
        ChatCompletion, ChatMessage, ChatRequest, ModelStream, StreamEvent, ToolCall,
        ToolCallDelta, Usage,
    },
};

pub(crate) fn encode(
    model: &str,
    request: ChatRequest,
    stream: bool,
) -> Result<(Value, HeaderMap), ModelError> {
    let reasoning_effort = request.reasoning_effort;
    let mut messages = Vec::new();
    for message in request.messages {
        match message {
            ChatMessage::System { content } => {
                messages.push(json!({"role":"system", "content":content}));
            }
            ChatMessage::User {
                content,
                attachments,
            } => {
                if attachments.is_empty() {
                    messages.push(json!({"role":"user", "content":content}));
                } else {
                    let mut blocks = vec![json!({"type":"text", "text":content})];
                    for attachment in attachments {
                        let (mime_type, data) = attachment.image()?;
                        blocks.push(json!({
                            "type":"image_url",
                            "image_url":{"url":format!("data:{mime_type};base64,{data}")}
                        }));
                    }
                    messages.push(json!({"role":"user", "content":blocks}));
                }
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
            } => messages.push(json!({
                "role":"assistant",
                "content":content,
                "tool_calls":tool_calls,
            })),
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => messages.push(json!({
                "role":"tool",
                "tool_call_id":tool_call_id,
                "content":content,
            })),
        }
    }
    let mut body = json!({
        "model": model,
        "messages": messages,
        "tools": request.tools,
        "temperature": request.temperature,
        "max_tokens": request.max_tokens,
        "stream": stream,
        "stream_options": if stream {
            json!({ "include_usage": true })
        } else {
            Value::Null
        },
    });
    if let Some(effort) = reasoning_effort {
        body["reasoning_effort"] = json!(effort);
        if model.to_ascii_lowercase().starts_with("qwen") {
            body["enable_thinking"] = json!(true);
        }
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

    Ok((body, headers))
}

pub(crate) async fn decode_completion(response: Response) -> Result<ChatCompletion, ModelError> {
    let value: Value = response.json().await?;
    if let Some(error) = crate::client::upstream_error(&value) {
        return Err(error);
    }
    let payload: WireCompletionResponse = serde_json::from_value(value)?;
    let choice = payload.choices.into_iter().next().ok_or_else(|| {
        crate::client::invalid_response("OpenAI response did not contain choices")
    })?;

    Ok(ChatCompletion {
        id: payload.id,
        model: payload.model,
        message: ChatMessage::Assistant {
            content: choice.message.content,
            tool_calls: choice.message.tool_calls,
        },
        finish_reason: choice.finish_reason,
        usage: payload.usage,
    })
}

pub(crate) fn stream(
    response: Response,
    timeout: Duration,
    cancellation: CancellationToken,
) -> ModelStream {
    sse_stream(response, timeout, cancellation, |data| {
        if data == "[DONE]" {
            return Ok(Vec::new());
        }

        let chunk: WireStreamResponse = serde_json::from_str(data)?;
        let choices_empty = chunk.choices.is_empty();
        let usage = chunk.usage.clone();
        let mut events = Vec::new();

        for choice in chunk.choices {
            if let Some(text) = choice.delta.content.filter(|text| !text.is_empty()) {
                events.push(StreamEvent::TextDelta(text));
            }

            if let Some(reasoning) = choice
                .delta
                .reasoning_content
                .filter(|text| !text.is_empty())
            {
                events.push(StreamEvent::ReasoningDelta(reasoning));
            }

            for call in choice.delta.tool_calls {
                let function = call.function;

                events.push(StreamEvent::ToolCallDelta(ToolCallDelta {
                    index: call.index.unwrap_or_default(),
                    id: call.id,
                    name: function.as_ref().and_then(|value| value.name.clone()),
                    arguments: function
                        .and_then(|value| value.arguments)
                        .unwrap_or_default(),
                }));
            }

            if choice.finish_reason.is_some() {
                events.push(StreamEvent::Completed {
                    finish_reason: choice.finish_reason,
                    usage: usage.clone(),
                });
            }
        }

        if choices_empty && usage.is_some() {
            events.push(StreamEvent::Completed {
                finish_reason: None,
                usage,
            });
        }

        Ok(events)
    })
}

#[derive(Deserialize)]
struct WireCompletionResponse {
    id: Option<String>,
    model: Option<String>,
    choices: Vec<WireChoice>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct WireChoice {
    message: WireMessage,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireMessage {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
}

#[derive(Deserialize)]
struct WireStreamResponse {
    #[serde(default)]
    choices: Vec<WireStreamChoice>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct WireStreamChoice {
    delta: WireDelta,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireDelta {
    content: Option<String>,
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<WireToolCallDelta>,
}

#[derive(Deserialize)]
struct WireToolCallDelta {
    index: Option<usize>,
    id: Option<String>,
    function: Option<WireFunctionDelta>,
}

#[derive(Deserialize)]
struct WireFunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[cfg(test)]
mod input_tests {
    use super::*;
    use crate::types::Attachment;

    #[test]
    fn includes_reasoning_effort_when_configured() {
        let mut request = ChatRequest::new(vec![ChatMessage::user("think")]);
        request.reasoning_effort = Some(crate::types::ReasoningEffort::High);
        let (body, _) = encode("gpt-test", request, true).unwrap();
        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn enables_thinking_for_qwen_reasoning_models() {
        let mut request = ChatRequest::new(vec![ChatMessage::user("think")]);
        request.reasoning_effort = Some(crate::types::ReasoningEffort::High);
        let (body, _) = encode("qwen3.8-27b", request, true).unwrap();
        assert_eq!(body["enable_thinking"], true);
    }

    #[test]
    fn encodes_local_images_as_chat_content_blocks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pixel.png");
        std::fs::write(&path, b"image-bytes").unwrap();
        let (body, _) = encode(
            "vision-model",
            ChatRequest::new(vec![ChatMessage::user_with_attachments(
                "inspect",
                vec![Attachment::Image {
                    name: "pixel.png".into(),
                    mime_type: "image/png".into(),
                    path,
                }],
            )]),
            true,
        )
        .unwrap();
        assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
        assert!(
            body["messages"][0]["content"][1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
    }
}
