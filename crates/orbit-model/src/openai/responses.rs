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
    reasoning_adapter: Option<crate::types::ReasoningAdapter>,
    reasoning_extra: &Value,
    stream: bool,
) -> Result<(Value, HeaderMap), ModelError> {
    let reasoning_effort = request.reasoning_effort;
    let instructions = request
        .messages
        .iter()
        .filter_map(|message| match message {
            ChatMessage::System { content } => Some(content.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut input = Vec::new();

    for message in request.messages {
        match message {
            ChatMessage::Runtime { kind, content } => {
                input.push(json!({ "role": "user", "content": [{ "type": "input_text", "text": crate::runtime_context::render(kind, &content) }] }));
            }
            ChatMessage::System { .. } => {}
            ChatMessage::User {
                content,
                attachments,
            } => {
                let mut blocks = vec![
                    json!({ "type": "input_text", "text": if content.is_empty() { "[图片]" } else { &content } }),
                ];

                for attachment in attachments {
                    let Some((mime_type, data)) = attachment.image_if_available()? else {
                        continue;
                    };
                    blocks.push(json!({ "type": "input_image", "image_url": format!("data:{mime_type};base64,{data}") }));
                }

                input.push(json!({ "role": "user", "content": blocks }));
            }

            ChatMessage::Assistant {
                content,
                tool_calls,
            } => {
                if let Some(content) = content {
                    input.push(json!({ "role": "assistant", "content": [{ "type": "output_text", "text": content }] }));
                }

                for call in tool_calls {
                    input.push(json!({ "type": "function_call", "call_id": call.id, "name": call.function.name, "arguments": call.function.arguments }));
                }
            }

            ChatMessage::Tool {
                tool_call_id,
                content,
            } => {
                input.push(json!({ "type": "function_call_output", "call_id": tool_call_id, "output": content }));
            }
        }
    }

    let tools = request
        .tools
        .into_iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.function.name,
                "description": tool.function.description,
                "parameters": tool.function.parameters
            })
        })
        .collect::<Vec<_>>();

    let mut body =
        json!({ "model": model, "instructions": instructions, "input": input, "stream": stream });

    if !tools.is_empty() {
        body["tools"] = json!(tools);
    }

    if let Some(temperature) = request.temperature {
        body["temperature"] = json!(temperature);
    }

    if let Some(max_tokens) = request.max_tokens {
        body["max_output_tokens"] = json!(max_tokens);
    }

    if let (Some(effort), Some(crate::types::ReasoningAdapter::OpenaiResponses)) =
        (reasoning_effort, reasoning_adapter)
    {
        crate::reasoning::apply_openai_responses(&mut body, effort, reasoning_extra);
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

    let payload: ResponsesResponse = serde_json::from_value(value)?;
    let (text, calls) = output_parts(payload.output_text.as_deref(), &payload.output);

    Ok(ChatCompletion {
        id: payload.id,
        model: payload.model,
        message: ChatMessage::Assistant {
            content: (!text.is_empty()).then_some(text),
            tool_calls: calls,
        },
        finish_reason: stop_reason(
            payload.status.as_deref(),
            payload.incomplete_details.as_ref(),
        ),
        usage: payload.usage.map(Into::into),
    })
}

pub(crate) fn stream(
    response: Response,
    timeout: Duration,
    cancellation: CancellationToken,
) -> ModelStream {
    sse_stream(response, timeout, cancellation, |data| {
        let event: ResponsesEvent = serde_json::from_str(data)?;
        let mut events = Vec::new();

        match event.kind.as_str() {
            "response.output_text.delta" => {
                if let Some(delta) = event.delta {
                    events.push(StreamEvent::TextDelta(delta));
                }
            }

            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                if let Some(delta) = event.delta {
                    events.push(StreamEvent::ReasoningDelta(delta));
                }
            }

            "response.output_item.added" => {
                if let Some(item) = event.item.filter(|item| item.kind == "function_call") {
                    events.push(StreamEvent::ToolCallDelta(ToolCallDelta {
                        metadata: None,
                        index: event.output_index.unwrap_or_default(),
                        id: item.call_id.or(item.id),
                        name: item.name,
                        arguments: item.arguments.unwrap_or_default(),
                    }));
                }
            }

            "response.function_call_arguments.delta" => {
                events.push(StreamEvent::ToolCallDelta(ToolCallDelta {
                    metadata: None,
                    index: event.output_index.unwrap_or_default(),
                    // The item ID differs from the function call ID used by tool results.
                    id: None,
                    name: None,
                    arguments: event.delta.unwrap_or_default(),
                }));
            }

            "response.completed" | "response.incomplete" => {
                if let Some(response) = event.response {
                    events.push(StreamEvent::Completed {
                        finish_reason: stop_reason(
                            response.status.as_deref(),
                            response.incomplete_details.as_ref(),
                        ),
                        usage: response.usage.map(Into::into),
                    });
                }
            }

            _ => {}
        }

        Ok(events)
    })
}

fn output_parts(output_text: Option<&str>, output: &[ResponseOutput]) -> (String, Vec<ToolCall>) {
    let text = output_text
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            output
                .iter()
                .filter(|item| item.kind == "message")
                .flat_map(|item| item.content.as_deref().unwrap_or_default())
                .filter_map(|part| part.text.as_deref())
                .collect::<Vec<_>>()
                .join("")
        });

    let calls = output
        .iter()
        .filter(|item| item.kind == "function_call")
        .filter_map(|item| {
            Some(ToolCall::function(
                item.call_id.clone().or(item.id.clone())?,
                item.name.clone()?,
                serde_json::from_str(item.arguments.as_deref().unwrap_or("{}"))
                    .unwrap_or_else(|_| json!({})),
            ))
        })
        .collect();

    (text, calls)
}

#[cfg(test)]
mod input_tests {
    use super::*;
    use crate::types::Attachment;

    #[test]
    fn includes_reasoning_effort_when_configured() {
        let mut request = ChatRequest::new(vec![ChatMessage::user("think")]);
        request.reasoning_effort = Some(crate::types::ReasoningEffort::High);
        let (body, _) = encode(
            "gpt-test",
            request,
            Some(crate::types::ReasoningAdapter::OpenaiResponses),
            &json!({}),
            true,
        )
        .unwrap();

        assert_eq!(body["reasoning"]["effort"], "high");
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
        assert_eq!(body["input"][0]["content"].as_array().unwrap().len(), 1);
        assert_eq!(body["input"][1]["role"], "user");
        assert!(!body.to_string().contains("gone.png"));
    }

    #[test]
    fn encodes_local_images_as_responses_input_blocks() {
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

        assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
        assert!(
            body["input"][0]["content"][1]["image_url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
    }
}

fn stop_reason(status: Option<&str>, details: Option<&IncompleteDetails>) -> Option<String> {
    details
        .and_then(|details| details.reason.clone())
        .or_else(|| (status == Some("incomplete")).then(|| "max_tokens".to_owned()))
        .or_else(|| (status == Some("completed")).then(|| "stop".to_owned()))
}

#[derive(Deserialize)]
struct ResponsesResponse {
    id: Option<String>,
    model: Option<String>,
    output_text: Option<String>,
    #[serde(default)]
    output: Vec<ResponseOutput>,
    status: Option<String>,
    incomplete_details: Option<IncompleteDetails>,
    usage: Option<ResponsesUsage>,
}

#[derive(Deserialize)]
struct ResponseOutput {
    #[serde(rename = "type")]
    kind: String,
    id: Option<String>,
    call_id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
    content: Option<Vec<ResponsePart>>,
}

#[derive(Deserialize)]
struct ResponsePart {
    text: Option<String>,
}

#[derive(Deserialize)]
struct IncompleteDetails {
    reason: Option<String>,
}

#[derive(Deserialize)]
struct ResponsesUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    total_tokens: Option<u64>,
    input_tokens_details: Option<crate::types::TokenDetails>,
}

impl From<ResponsesUsage> for Usage {
    fn from(value: ResponsesUsage) -> Self {
        Self {
            prompt_tokens: value.input_tokens,
            completion_tokens: value.output_tokens,
            total_tokens: value.total_tokens,
            cached_input_tokens: value
                .input_tokens_details
                .and_then(|details| details.cached_tokens),
            cache_write_input_tokens: None,
        }
    }
}

#[derive(Deserialize)]
struct ResponsesEvent {
    #[serde(rename = "type")]
    kind: String,
    delta: Option<String>,
    output_index: Option<usize>,
    item: Option<ResponseOutput>,
    response: Option<ResponsesResponse>,
}
