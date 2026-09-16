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

pub(crate) fn encode(model: &str, request: ChatRequest, stream: bool) -> (Value, HeaderMap) {
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
            ChatMessage::System { .. } => {}
            ChatMessage::User { content } => {
                input.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": content,
                    }],
                }));
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
            } => {
                if let Some(content) = content {
                    input.push(json!({
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": content,
                        }],
                    }));
                }

                for call in tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.function.name,
                        "arguments": call.function.arguments,
                    }));
                }
            }
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => {
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_call_id,
                    "output": content,
                }));
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
                "parameters": tool.function.parameters,
            })
        })
        .collect::<Vec<_>>();

    let body = json!({
        "model": model,
        "instructions": instructions,
        "input": input,
        "tools": tools,
        "temperature": request.temperature,
        "max_output_tokens": request.max_tokens,
        "stream": stream,
    });

    let mut headers = HeaderMap::new();
    headers.insert(
        ACCEPT,
        HeaderValue::from_static(if stream {
            "text/event-stream"
        } else {
            "application/json"
        }),
    );

    (body, headers)
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
                        index: event.output_index.unwrap_or_default(),
                        id: item.call_id.or(item.id),
                        name: item.name,
                        arguments: item.arguments.unwrap_or_default(),
                    }));
                }
            }
            "response.function_call_arguments.delta" => {
                events.push(StreamEvent::ToolCallDelta(ToolCallDelta {
                    index: event.output_index.unwrap_or_default(),
                    id: event.item_id,
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
}

impl From<ResponsesUsage> for Usage {
    fn from(value: ResponsesUsage) -> Self {
        Self {
            prompt_tokens: value.input_tokens,
            completion_tokens: value.output_tokens,
            total_tokens: value.total_tokens,
        }
    }
}

#[derive(Deserialize)]
struct ResponsesEvent {
    #[serde(rename = "type")]
    kind: String,
    delta: Option<String>,
    output_index: Option<usize>,
    item_id: Option<String>,
    item: Option<ResponseOutput>,
    response: Option<ResponsesResponse>,
}
