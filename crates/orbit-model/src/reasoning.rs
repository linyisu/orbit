//! Provider capability adapters for reasoning controls.
//!
//! Keep compatibility choices here rather than inferring them from a model ID
//! inside individual request encoders. A gateway can rename a model while its
//! provider-level protocol remains stable.

use serde_json::{Value, json};

use crate::types::ReasoningEffort;

fn merge_extra(body: &mut Value, extra: &Value) {
    match (body, extra) {
        (Value::Object(body), Value::Object(extra)) => {
            for (key, value) in extra {
                match body.get_mut(key) {
                    Some(existing) => merge_extra(existing, value),
                    None => {
                        body.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (body, extra) => *body = extra.clone(),
    }
}

pub(crate) fn apply_openai_chat(body: &mut Value, effort: ReasoningEffort, extra: &Value) {
    if effort == ReasoningEffort::None {
        merge_extra(body, extra);

        if body.get("enable_thinking").is_some() {
            body["enable_thinking"] = json!(false);
        }

        if let Some(kwargs) = body
            .get_mut("chat_template_kwargs")
            .and_then(Value::as_object_mut)
            && kwargs.contains_key("enable_thinking")
        {
            kwargs.insert("enable_thinking".into(), json!(false));
        }

        body["reasoning_effort"] = json!("none");
    } else {
        body["reasoning_effort"] = json!(effort);
        merge_extra(body, extra);
    }
}

pub(crate) fn apply_openai_responses(body: &mut Value, effort: ReasoningEffort, extra: &Value) {
    body["reasoning"] = json!({ "effort": effort });
    merge_extra(body, extra);
}

pub(crate) fn apply_anthropic_messages(body: &mut Value, effort: ReasoningEffort, extra: &Value) {
    if effort == ReasoningEffort::None {
        merge_extra(body, extra);
        body["thinking"] = json!({ "type": "disabled" });

        if let Some(obj) = body.as_object_mut() {
            obj.remove("output_config");
        }
    } else {
        body["thinking"] = json!({ "type": "adaptive" });
        body["output_config"] = json!({ "effort": effort });
        merge_extra(body, extra);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_openai_chat_none_disables_thinking() {
        let mut body = json!({});
        apply_openai_chat(
            &mut body,
            ReasoningEffort::None,
            &json!({ "enable_thinking": true }),
        );

        assert_eq!(body["reasoning_effort"], "none");
        assert_eq!(body["enable_thinking"], false);
    }

    #[test]
    fn test_apply_openai_responses_none() {
        let mut body = json!({});
        apply_openai_responses(&mut body, ReasoningEffort::None, &json!({}));

        assert_eq!(body["reasoning"]["effort"], "none");
    }

    #[test]
    fn test_apply_anthropic_messages_none_disables_thinking() {
        let mut body = json!({});
        apply_anthropic_messages(
            &mut body,
            ReasoningEffort::None,
            &json!({
                "thinking": { "type": "enabled" }
            }),
        );

        assert_eq!(body["thinking"]["type"], "disabled");
        assert!(body.get("output_config").is_none());
    }
}
