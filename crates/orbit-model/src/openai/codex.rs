use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::{Value, json};

use crate::{
    error::ModelError,
    types::{ChatRequest, ReasoningAdapter},
};

pub(crate) fn encode(
    model: &str,
    request: ChatRequest,
    adapter: Option<ReasoningAdapter>,
    extra: &Value,
) -> Result<(Value, HeaderMap), ModelError> {
    let (mut body, mut headers) = super::responses::encode(model, request, adapter, extra, true)?;
    // Subscription Responses is SSE-only and rejects API sampling/output limits.
    let object = body.as_object_mut().expect("Responses body is an object");
    object.remove("max_output_tokens");
    object.remove("temperature");
    body["store"] = json!(false);
    body["stream"] = json!(true);
    if body.get("reasoning").is_some() {
        body["reasoning"]["summary"] = json!("auto");
    }
    headers.insert(
        "openai-beta",
        HeaderValue::from_static("responses=experimental"),
    );
    headers.insert(
        "version",
        HeaderValue::from_static(crate::discovery::CODEX_CLIENT_VERSION),
    );
    Ok((body, headers))
}
