use std::{collections::HashMap, time::Duration};

use reqwest::{
    Response,
    header::{ACCEPT, HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::{client::sse_stream, error::ModelError, types::*};

pub(crate) fn encode(model: &str, request: ChatRequest) -> Result<(Value, HeaderMap), ModelError> {
    let mut system = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    let mut calls = HashMap::new();

    for message in request.messages {
        let (role, parts) = match message {
            ChatMessage::System { content } => {
                system.push(json!({ "text": content }));
                continue;
            }
            ChatMessage::User {
                content,
                attachments,
            } => {
                let mut parts = Vec::new();
                if !content.is_empty() {
                    parts.push(json!({ "text": content }));
                }
                for attachment in attachments {
                    let (mime, data) = attachment.image()?;
                    parts.push(json!({ "inlineData": { "mimeType": mime, "data": data } }));
                }
                ("user", parts)
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
            } => {
                let mut parts = Vec::new();
                if let Some(content) = content.filter(|text| !text.is_empty()) {
                    parts.push(json!({ "text": content }));
                }
                for call in tool_calls {
                    let arguments: Value = serde_json::from_str(&call.function.arguments)?;
                    let metadata = call
                        .metadata
                        .as_ref()
                        .map(|value| &value.google)
                        .filter(|value| value.model == model);
                    let native_id = metadata.and_then(|value| value.native_id.clone());
                    let mut function = json!({ "name": call.function.name, "args": arguments });
                    if let Some(id) = &native_id {
                        function["id"] = json!(id);
                    }
                    let mut part = json!({ "functionCall": function });
                    if let Some(signature) =
                        metadata.and_then(|value| value.thought_signature.as_deref())
                    {
                        part["thoughtSignature"] = json!(signature);
                    } else {
                        // Documented Gemini replay marker for imported / unsigned calls.
                        part["thoughtSignature"] = json!("skip_thought_signature_validator");
                    }
                    parts.push(part);
                    calls.insert(call.id, (call.function.name, native_id));
                }
                ("model", parts)
            }
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => {
                let (name, native_id) = calls.get(&tool_call_id).ok_or_else(|| {
                    ModelError::Protocol("Google tool result has no matching function call".into())
                })?;
                let mut response = json!({ "name": name, "response": { "output": content } });
                if let Some(id) = native_id {
                    response["id"] = json!(id);
                }
                ("user", vec![json!({ "functionResponse": response })])
            }
        };
        if parts.is_empty() {
            continue;
        }
        if let Some(last) = contents.last_mut().filter(|last| last["role"] == role) {
            last["parts"]
                .as_array_mut()
                .expect("parts array")
                .extend(parts);
        } else {
            contents.push(json!({ "role": role, "parts": parts }));
        }
    }

    let mut body = json!({ "contents": contents });
    if !system.is_empty() {
        body["systemInstruction"] = json!({ "parts": system });
    }
    if !request.tools.is_empty() {
        let declarations = request
            .tools
            .into_iter()
            .map(|tool| {
                json!({
                    "name": tool.function.name,
                    "description": tool.function.description,
                    "parametersJsonSchema": tool.function.parameters
                })
            })
            .collect::<Vec<_>>();
        body["tools"] = json!([{ "functionDeclarations": declarations }]);
    }
    let mut generation = json!({});
    if let Some(max) = request.max_tokens {
        generation["maxOutputTokens"] = json!(max);
    }
    if let Some(temperature) = request.temperature {
        generation["temperature"] = json!(temperature);
    }
    if let Some(effort) = request.reasoning_effort {
        let level = match effort {
            ReasoningEffort::Minimal => "MINIMAL",
            ReasoningEffort::Low => "LOW",
            ReasoningEffort::Medium => "MEDIUM",
            ReasoningEffort::High => "HIGH",
            ReasoningEffort::None | ReasoningEffort::Xhigh | ReasoningEffort::Max => return Err(ModelError::Protocol("Google thinkingLevel supports minimal, low, medium, or high; configure only levels supported by the model".into())),
        };
        generation["thinkingConfig"] =
            json!({ "thinkingLevel": level, "includeThoughts": effort != ReasoningEffort::None });
    }
    body["generationConfig"] = generation;
    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));

    Ok((json!({ "model": model, "request": body }), headers))
}

pub(crate) fn stream(
    response: Response,
    timeout: Duration,
    cancellation: CancellationToken,
    model: String,
) -> ModelStream {
    let mut nonce = [0; 16];
    if getrandom::fill(&mut nonce).is_err() {
        return Box::pin(futures_util::stream::once(async {
            Err(ModelError::Protocol("secure randomness unavailable".into()))
        }));
    }
    let nonce = nonce
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut next_call = 0;
    sse_stream(response, timeout, cancellation, move |data| {
        decode(data, &model, &nonce, &mut next_call)
    })
}

fn decode(
    data: &str,
    model: &str,
    nonce: &str,
    next_call: &mut usize,
) -> Result<Vec<StreamEvent>, ModelError> {
    let envelope: Value = serde_json::from_str(data)?;
    let value = envelope.get("response").unwrap_or(&envelope);
    if value["promptFeedback"].get("blockReason").is_some() {
        return Err(ModelError::Protocol("Google blocked the request".into()));
    }
    let mut events = Vec::new();
    let candidate = &value["candidates"][0];

    for part in candidate["content"]["parts"]
        .as_array()
        .into_iter()
        .flatten()
    {
        if let Some(text) = part["text"].as_str() {
            events.push(if part["thought"] == true {
                StreamEvent::ReasoningDelta(text.into())
            } else {
                StreamEvent::TextDelta(text.into())
            });
        }
        if let Some(call) = part.get("functionCall") {
            let name = call["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| {
                    ModelError::Protocol("Google returned a function call without a name".into())
                })?;
            let native_id = call["id"].as_str().map(str::to_owned);
            let id = native_id
                .clone()
                .unwrap_or_else(|| format!("google_{nonce}_{next_call}"));
            events.push(StreamEvent::ToolCallDelta(ToolCallDelta {
                index: *next_call,
                id: Some(id),
                name: Some(name.into()),
                arguments: call
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| json!({}))
                    .to_string(),
                metadata: Some(ToolCallMetadata {
                    google: GoogleToolMetadata {
                        model: model.into(),
                        thought_signature: part["thoughtSignature"].as_str().map(str::to_owned),
                        native_id,
                    },
                }),
            }));
            *next_call += 1;
        }
    }
    if let Some(reason) = candidate["finishReason"].as_str() {
        let finish = match reason {
            "STOP" if *next_call > 0 => "tool_calls",
            "STOP" => "stop",
            "MAX_TOKENS" => "length",
            other => {
                return Err(ModelError::Protocol(format!(
                    "Google generation stopped: {other}"
                )));
            }
        };
        let usage = &value["usageMetadata"];
        let output = usage["candidatesTokenCount"]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(usage["thoughtsTokenCount"].as_u64().unwrap_or(0));
        events.push(StreamEvent::Completed {
            finish_reason: Some(finish.into()),
            usage: value.get("usageMetadata").map(|_| Usage {
                prompt_tokens: usage["promptTokenCount"].as_u64(),
                completion_tokens: Some(output),
                total_tokens: usage["totalTokenCount"].as_u64(),
            }),
        });
    }
    Ok(events)
}

/// Antigravity's agent envelope is a request projection, never stored in history.
pub(crate) fn antigravity_envelope(body: &mut Value) -> Result<(), ModelError> {
    project_antigravity_model(body)?;
    let mut random = [0u8; 32];
    getrandom::fill(&mut random)
        .map_err(|_| ModelError::Protocol("secure randomness unavailable".into()))?;
    let id = |bytes: &[u8]| {
        let mut bytes: [u8; 16] = bytes.try_into().expect("sixteen bytes");
        bytes[6] = (bytes[6] & 0x0f) | 0x40;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;
        let hex = bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        format!(
            "{}-{}-{}-{}-{}",
            &hex[..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..]
        )
    };
    let agent = id(&random[..16]);
    let trajectory = id(&random[16..]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    body["userAgent"] = json!("antigravity");
    body["requestType"] = json!("agent");
    body["requestId"] = json!(format!("agent/{agent}/{now}/{trajectory}/2"));
    body["request"]["sessionId"] =
        json!(u64::from_le_bytes(random[..8].try_into().expect("eight bytes")).to_string());
    let claude = body["model"]
        .as_str()
        .is_some_and(|id| id.starts_with("claude-"));
    body["request"]["labels"] = json!({
        "trajectory_id": trajectory,
        "last_step_index": "1",
        "used_claude": if claude { "true" } else { "false" },
        "used_claude_conservative": if claude { "true" } else { "false" }
    });

    Ok(())
}

fn project_antigravity_model(body: &mut Value) -> Result<(), ModelError> {
    let id = body["model"].as_str().unwrap_or_default().to_owned();
    let Some(profile) = crate::account_catalog::antigravity_models().get(&id) else {
        return Ok(());
    };
    let effort = body["request"]["generationConfig"]["thinkingConfig"]["thinkingLevel"]
        .as_str()
        .map(str::to_ascii_lowercase);
    let route = profile
        .thinking
        .as_ref()
        .and_then(|thinking| thinking["effortRouting"][effort.as_deref().unwrap_or("off")].as_str())
        .or(profile.request_model_id.as_deref())
        .unwrap_or(&id);
    body["model"] = json!(route);

    let generation = &mut body["request"]["generationConfig"];
    generation["maxOutputTokens"] = json!(profile.max_tokens);
    if let (Some(effort), Some(thinking)) = (effort, &profile.thinking)
        && thinking["mode"] == "budget"
    {
        let budget = thinking["effortBudgets"][&effort]
            .as_u64()
            .unwrap_or(match effort.as_str() {
                "minimal" => 1024,
                "low" => 4096,
                "medium" => 8192,
                _ => 16384,
            });
        generation["thinkingConfig"] = json!({ "thinkingBudget": budget, "includeThoughts": true });
    }

    if let Some(system) = body["request"].get_mut("systemInstruction") {
        system["role"] = json!("user");
    }
    if let Some(tools) = body["request"]["tools"].as_array_mut() {
        for tool in tools {
            if let Some(declarations) = tool["functionDeclarations"].as_array_mut() {
                for declaration in declarations {
                    if let Some(schema) = declaration
                        .as_object_mut()
                        .and_then(|map| map.remove("parametersJsonSchema"))
                    {
                        declaration["parameters"] = cca_schema(schema)?;
                    }
                }
            }
        }
        body["request"]["toolConfig"] = json!({ "functionCallingConfig": { "mode": "VALIDATED" } });
    }

    Ok(())
}

// Project tool schemas only at the provider boundary. Local validation retains
// the original constraints; property names must never be treated as keywords.
fn cca_schema(schema: Value) -> Result<Value, ModelError> {
    let Some(source) = schema.as_object() else {
        return Err(ModelError::Protocol(
            "Cloud Code Assist requires object tool schemas".into(),
        ));
    };
    let mut result = serde_json::Map::new();
    for key in ["type", "description", "enum", "required", "title"] {
        if let Some(value) = source.get(key) {
            result.insert(key.into(), value.clone());
        }
    }
    if source
        .keys()
        .any(|key| matches!(key.as_str(), "$ref" | "anyOf" | "oneOf" | "allOf" | "not"))
        || source.get("type").is_some_and(Value::is_array)
    {
        return Err(ModelError::Protocol(
            "Cloud Code Assist tool schema contains an unsupported reference or union".into(),
        ));
    }

    if let Some(properties) = source.get("properties").and_then(Value::as_object) {
        let mut normalized = serde_json::Map::new();
        for (name, property) in properties {
            normalized.insert(name.clone(), cca_schema(property.clone())?);
        }
        result.insert("properties".into(), Value::Object(normalized));
    }
    if let Some(items) = source.get("items") {
        result.insert("items".into(), cca_schema(items.clone())?);
    }

    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antigravity_routes_logical_models_and_projects_thinking_and_tools() {
        for (model, effort, wire, budget) in [
            ("gemini-3.8-flash", "HIGH", "gemini-3.8-flash-high", None),
            ("gemini-3.1-pro", "LOW", "gemini-3.1-pro-low", Some(1001)),
            (
                "claude-opus-4-6",
                "HIGH",
                "claude-opus-4-6-thinking",
                Some(16384),
            ),
            ("gpt-oss-120b", "MEDIUM", "gpt-oss-120b-medium", Some(8192)),
        ] {
            let mut body = json!({
                "model": model,
                "request": {
                    "generationConfig": { "thinkingConfig": { "thinkingLevel": effort } },
                    "tools": [{ "functionDeclarations": [{
                        "name": "read_file",
                        "parametersJsonSchema": {
                            "type": "object",
                            "properties": { "minimum": { "type": "integer", "minimum": 1 } },
                            "additionalProperties": false
                        }
                    }] }]
                }
            });

            antigravity_envelope(&mut body).unwrap();

            assert_eq!(body["model"], wire);
            let thinking = &body["request"]["generationConfig"]["thinkingConfig"];
            if let Some(budget) = budget {
                assert_eq!(thinking["thinkingBudget"], budget);
                assert!(thinking.get("thinkingLevel").is_none());
            } else {
                assert_eq!(thinking["thinkingLevel"], effort);
            }
            let tool = &body["request"]["tools"][0]["functionDeclarations"][0];
            assert!(tool.get("parametersJsonSchema").is_none());
            assert_eq!(
                tool["parameters"]["properties"]["minimum"]["type"],
                "integer"
            );
            assert!(tool["parameters"].get("additionalProperties").is_none());
        }
    }

    #[test]
    fn signed_tool_calls_survive_serialization_and_replay_but_do_not_leak_to_other_protocols() {
        let call = ToolCall {
            metadata: Some(ToolCallMetadata {
                google: GoogleToolMetadata {
                    model: "gemini-test".into(),
                    thought_signature: Some("signature".into()),
                    native_id: Some("native-id".into()),
                },
            }),
            ..ToolCall::function("orbit-id", "read_file", json!({ "path": "src/main.rs" }))
        };
        let messages = vec![
            ChatMessage::user("read it"),
            ChatMessage::assistant_tool_calls(vec![call]),
            ChatMessage::tool("orbit-id", "file content"),
        ];
        let serialized = serde_json::to_string(&messages).unwrap();
        let restored = serde_json::from_str(&serialized).unwrap();

        let (body, _) = encode("gemini-test", ChatRequest::new(restored)).unwrap();

        let call = &body["request"]["contents"][1]["parts"][0];
        assert_eq!(call["thoughtSignature"], "signature");
        assert_eq!(call["functionCall"]["id"], "native-id");
        assert_eq!(
            body["request"]["contents"][2]["parts"][0]["functionResponse"]["name"],
            "read_file"
        );

        let (other, _) = crate::openai::chat_completions::encode(
            "test",
            ChatRequest::new(messages.clone()),
            None,
            &json!({}),
            false,
        )
        .unwrap();

        assert!(!other.to_string().contains("signature"));
        assert!(
            serde_json::to_string(&messages)
                .unwrap()
                .contains("signature")
        );
    }

    #[test]
    fn parallel_function_calls_have_distinct_ids_across_chunks_and_responses() {
        let payload = json!({ "response": { "candidates": [{ "content": { "parts": [{ "functionCall": { "name": "test", "args": {} } }] } }] } }).to_string();
        let mut index = 0;

        let first = decode(&payload, "model", "turn-one", &mut index).unwrap();
        let second = decode(&payload, "model", "turn-one", &mut index).unwrap();
        let third = decode(&payload, "model", "turn-two", &mut 0).unwrap();

        let id = |events: &[StreamEvent]| match &events[0] {
            StreamEvent::ToolCallDelta(delta) => delta.id.clone().unwrap(),
            _ => panic!("expected tool call"),
        };
        assert_ne!(id(&first), id(&second));
        assert_ne!(id(&first), id(&third));
    }
}
