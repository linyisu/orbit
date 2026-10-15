use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};

use crate::{
    error::ModelError,
    types::{ChatRequest, ReasoningAdapter},
};

const TOOL_NAMESPACE: &str = "orbit";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    LegacyCodex,
    ChatgptPlan,
}

impl Route {
    pub fn for_endpoint(endpoint: &reqwest::Url) -> Result<Self, ModelError> {
        match (
            endpoint
                .origin()
                .ascii_serialization()
                .as_str(),
            endpoint.path(),
        ) {
            ("https://chatgpt.com", "/backend-api/codex/responses") => Ok(Self::LegacyCodex),
            ("https://api.openai.com", "/v1/responses") => Ok(Self::ChatgptPlan),
            _ => Err(ModelError::Protocol(
                "unsupported ChatGPT subscription endpoint".into(),
            )),
        }
    }
}

pub(crate) fn encode(
    model: &str,
    request: ChatRequest,
    adapter: Option<ReasoningAdapter>,
    extra: &Value,
    route: Route,
) -> Result<(Value, HeaderMap), ModelError> {
    let (mut body, mut headers) = super::responses::encode(model, request, adapter, extra, true)?;
    // Subscription Responses is SSE-only and rejects API sampling/output limits.
    let object = body
        .as_object_mut()
        .expect("Responses body is an object");
    object.remove("max_output_tokens");
    object.remove("temperature");
    body["store"] = json!(false);
    body["stream"] = json!(true);
    if body.get("reasoning").is_some() {
        body["reasoning"]["summary"] = json!("auto");
    }
    if route == Route::LegacyCodex {
        headers.insert(
            "openai-beta",
            HeaderValue::from_static("responses=experimental"),
        );
        headers.insert(
            "version",
            HeaderValue::from_static(crate::discovery::CODEX_CLIENT_VERSION),
        );
    } else {
        if let Some(tools) = body.get_mut("tools") {
            let functions = tools.take();
            *tools = json!([{
                "type": "namespace",
                "name": TOOL_NAMESPACE,
                "description": "Orbit workspace tools",
                "tools": functions,
            }]);
        }
        for item in body["input"]
            .as_array_mut()
            .expect("Responses input array")
        {
            if item["type"] == "function_call" {
                item["namespace"] = json!(TOOL_NAMESPACE);
            }
        }
    }
    Ok((body, headers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatMessage, ToolCall, ToolDefinition};

    #[test]
    fn plan_requests_namespace_tools_and_replay_calls_without_legacy_headers() {
        let mut request = ChatRequest::new(vec![ChatMessage::Assistant {
            content: None,
            tool_calls: vec![ToolCall::function(
                "generated-call",
                "generated-tool",
                json!({}),
            )],
        }]);
        request.tools = vec![ToolDefinition::function(
            "generated-tool",
            "Generated tool",
            json!({ "type": "object" }),
        )];
        request.max_tokens = Some(8_000);
        request.temperature = Some(0.3);
        let (body, headers) = encode(
            "generated-model",
            request.clone(),
            None,
            &json!({}),
            Route::ChatgptPlan,
        )
        .unwrap();
        assert_eq!(body["tools"][0]["type"], "namespace");
        assert_eq!(body["tools"][0]["name"], TOOL_NAMESPACE);
        assert_eq!(body["input"][0]["namespace"], TOOL_NAMESPACE);
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert!(body.get("temperature").is_none());
        assert!(body.get("max_output_tokens").is_none());
        assert!(!headers.contains_key("version"));
        assert!(!headers.contains_key("openai-beta"));
        let (legacy, headers) = encode(
            "generated-model",
            request,
            None,
            &json!({}),
            Route::LegacyCodex,
        )
        .unwrap();
        assert_eq!(legacy["tools"][0]["type"], "function");
        assert!(
            legacy["input"][0]
                .get("namespace")
                .is_none()
        );
        assert!(headers.contains_key("version"));
    }
}
