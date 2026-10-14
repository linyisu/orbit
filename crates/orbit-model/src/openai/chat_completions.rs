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
        ChatCompletion, ChatMessage, ChatRequest, ModelStream, ReasoningAdapter, StreamEvent,
        ToolCall, ToolCallDelta, Usage,
    },
};

pub(crate) fn encode(
    model: &str,
    request: ChatRequest,
    reasoning_adapter: Option<ReasoningAdapter>,
    reasoning_extra: &Value,
    stream: bool,
) -> Result<(Value, HeaderMap), ModelError> {
    let reasoning_effort = request.reasoning_effort;
    let mut messages = Vec::new();

    for message in request.messages {
        match message {
            ChatMessage::Runtime { kind, content } => {
                messages.push(json!({
                    "role": "user",
                    "content": crate::runtime_context::render(kind, &content)
                }));
            }
            ChatMessage::System { content } => {
                messages.push(json!({
                    "role": "system",
                    "content": content
                }));
            }
            ChatMessage::User {
                content,
                attachments,
            } => {
                if attachments.is_empty() {
                    messages.push(json!({
                        "role": "user",
                        "content": content
                    }));
                } else {
                    let mut blocks = vec![json!({
                        "type": "text",
                        "text": if content.is_empty() {
                            "[图片]"
                        } else {
                            &content
                        }
                    })];

                    for attachment in attachments {
                        let Some((mime_type, data)) = attachment.image_if_available()? else {
                            continue;
                        };
                        blocks.push(json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{mime_type};base64,{data}")
                            }
                        }));
                    }

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
                let calls = tool_calls
                    .into_iter()
                    .map(|mut call| {
                        call.metadata = None;
                        call
                    })
                    .collect::<Vec<_>>();
                let mut message = json!({
                    "role": "assistant",
                    "content": content
                });
                if !calls.is_empty() {
                    message["tool_calls"] = json!(calls);
                }
                messages.push(message);
            }

            ChatMessage::Tool {
                tool_call_id,
                content,
            } => messages.push(json!({
                "role": "tool",
                "tool_call_id": tool_call_id,
                "content": content
            })),
        }
    }

    let mut body = json!({
        "model": model,
        "messages": messages,
        "stream": stream
    });

    if !request.tools.is_empty() {
        body["tools"] = json!(request.tools);
    }

    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }

    if let Some(max_tokens) = request.max_tokens {
        body["max_tokens"] = json!(max_tokens);
    }

    if stream {
        body["stream_options"] = json!({
            "include_usage": true
        });
    }

    if let (Some(effort), Some(ReasoningAdapter::OpenaiChat)) =
        (reasoning_effort, reasoning_adapter)
    {
        crate::reasoning::apply_openai_chat(&mut body, effort, reasoning_extra);
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
    let choice = payload
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| {
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
            if let Some(text) = choice
                .delta
                .content
                .filter(|text| !text.is_empty())
            {
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
                    metadata: None,
                    index: call.index.unwrap_or_default(),
                    id: call.id,
                    name: function
                        .as_ref()
                        .and_then(|value| value.name.clone()),
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

// Compatible gateways may encode absent optional arrays as explicit null.
// Keep malformed non-array values and required completion choices as errors.
fn null_as_empty<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(deserializer)?.unwrap_or_default())
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
    #[serde(default, deserialize_with = "null_as_empty")]
    tool_calls: Vec<ToolCall>,
}

#[derive(Deserialize)]
struct WireStreamResponse {
    #[serde(default, deserialize_with = "null_as_empty")]
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
    #[serde(default, deserialize_with = "null_as_empty")]
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
    fn optional_wire_arrays_accept_missing_null_and_empty_but_reject_wrong_types() {
        for fields in [
            json!({}),
            json!({
                "tool_calls": null
            }),
            json!({
                "tool_calls": []
            }),
        ] {
            let mut message = json!({
                "content": "hello"
            });
            message
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            let parsed: WireMessage = serde_json::from_value(message.clone()).unwrap();
            assert!(parsed.tool_calls.is_empty());
            let parsed: WireDelta = serde_json::from_value(message).unwrap();
            assert!(parsed.tool_calls.is_empty());
        }

        for invalid in [json!({}), json!("oops"), json!(1)] {
            assert!(
                serde_json::from_value::<WireDelta>(json!({
                    "tool_calls": invalid
                }))
                .is_err()
            );
        }
        let usage: WireStreamResponse = serde_json::from_value(json!({
            "choices": null
        }))
        .unwrap();
        assert!(usage.choices.is_empty());
        assert!(
            serde_json::from_value::<WireCompletionResponse>(json!({
                "choices": null
            }))
            .is_err()
        );
    }

    #[test]
    fn replay_omits_empty_tool_calls_but_preserves_actual_calls() {
        for stream in [false, true] {
            let (body, _) = encode(
                "deepseek-chat",
                ChatRequest::new(vec![
                    ChatMessage::user("hello"),
                    ChatMessage::assistant("hello back"),
                    ChatMessage::assistant_tool_calls(vec![ToolCall::function(
                        "call-1",
                        "web_search",
                        json!({
                            "query": "rust"
                        }),
                    )]),
                ]),
                None,
                &json!({}),
                stream,
            )
            .unwrap();

            assert_eq!(body["messages"][1]["content"], "hello back");
            assert!(
                body["messages"][1]
                    .get("tool_calls")
                    .is_none()
            );
            let calls = body["messages"][2]["tool_calls"]
                .as_array()
                .unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0]["id"], "call-1");
            assert_eq!(calls[0]["function"]["name"], "web_search");
        }
    }

    #[test]
    fn includes_reasoning_effort_when_configured() {
        let mut request = ChatRequest::new(vec![ChatMessage::user("think")]);
        request.reasoning_effort = Some(crate::types::ReasoningEffort::High);
        let (body, _) = encode(
            "gpt-test",
            request,
            Some(ReasoningAdapter::OpenaiChat),
            &json!({}),
            true,
        )
        .unwrap();

        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn model_profile_extra_can_enable_a_provider_specific_thinking_switch() {
        let mut request = ChatRequest::new(vec![ChatMessage::user("think")]);
        request.reasoning_effort = Some(crate::types::ReasoningEffort::High);
        let (body, _) = encode(
            "renamed-by-gateway",
            request,
            Some(ReasoningAdapter::OpenaiChat),
            &json!({
                "enable_thinking": true
            }),
            true,
        )
        .unwrap();

        assert_eq!(body["enable_thinking"], true);
    }

    #[test]
    fn model_profile_extra_can_supply_another_compatible_thinking_switch() {
        let mut request = ChatRequest::new(vec![ChatMessage::user("think")]);
        request.reasoning_effort = Some(crate::types::ReasoningEffort::High);
        let (body, _) = encode(
            "gateway-name",
            request,
            Some(ReasoningAdapter::OpenaiChat),
            &json!({
                "thinking": {
                    "type": "enabled"
                }
            }),
            true,
        )
        .unwrap();

        assert_eq!(body["thinking"]["type"], "enabled");
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
    fn encodes_local_images_as_chat_content_blocks() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pixel.png");
        std::fs::write(&path, b"image-bytes").unwrap();
        let (body, _) = encode(
            "vision-model",
            ChatRequest::new(vec![ChatMessage::user_with_attachments(
                "inspect",
                vec![Attachment::Image {
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

        assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
        assert!(
            body["messages"][0]["content"][1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
    }
}
