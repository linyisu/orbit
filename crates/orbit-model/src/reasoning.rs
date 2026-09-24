//! Provider-specific reasoning controls, selected by model capability profiles.

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
        (target, value) => *target = value.clone(),
    }
}

pub(crate) fn apply_openai_chat(body: &mut Value, effort: ReasoningEffort, extra: &Value) {
    body["reasoning_effort"] = json!(effort);
    merge_extra(body, extra);
    if effort == ReasoningEffort::None {
        if body.get("enable_thinking").is_some() {
            body["enable_thinking"] = json!(false);
        }
        if let Some(kwargs) = body
            .get_mut("chat_template_kwargs")
            .and_then(Value::as_object_mut)
            && kwargs.contains_key("enable_thinking")
        {
            kwargs.insert("enable_thinking".to_owned(), json!(false));
        }
    }
}

pub(crate) fn apply_openai_responses(body: &mut Value, effort: ReasoningEffort, extra: &Value) {
    body["reasoning"] = json!({ "effort": effort });
    merge_extra(body, extra);
}

pub(crate) fn apply_anthropic_messages(body: &mut Value, effort: ReasoningEffort, extra: &Value) {
    if effort == ReasoningEffort::None {
        body["thinking"] = json!({ "type": "disabled" });
        merge_extra(body, extra);
        body.as_object_mut()
            .expect("request body is an object")
            .remove("output_config");
        body["thinking"] = json!({ "type": "disabled" });
    } else {
        body["thinking"] = json!({ "type": "adaptive" });
        body["output_config"] = json!({ "effort": effort });
        merge_extra(body, extra);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Attachment, ChatMessage, ChatRequest, ReasoningAdapter};
    use sha2::{Digest, Sha256};

    #[test]
    fn none_overrides_provider_thinking_defaults() {
        let mut chat = json!({});
        apply_openai_chat(
            &mut chat,
            ReasoningEffort::None,
            &json!({ "enable_thinking": true }),
        );
        assert_eq!(chat["reasoning_effort"], "none");
        assert_eq!(chat["enable_thinking"], false);

        let mut anthropic = json!({});
        apply_anthropic_messages(
            &mut anthropic,
            ReasoningEffort::None,
            &json!({ "thinking": { "type": "enabled" } }),
        );
        assert_eq!(anthropic["thinking"]["type"], "disabled");
        assert!(anthropic.get("output_config").is_none());
    }

    #[test]
    fn request_encoders_apply_their_configured_reasoning_adapter() {
        let mut request = ChatRequest::new(vec![ChatMessage::user("hello")]);
        request.reasoning_effort = Some(ReasoningEffort::High);

        let (chat, _) = crate::openai::chat_completions::encode(
            "model",
            request.clone(),
            Some(ReasoningAdapter::OpenaiChat),
            &json!({ "chat_template_kwargs": { "enable_thinking": true } }),
            false,
        )
        .unwrap();
        assert_eq!(chat["reasoning_effort"], "high");
        assert_eq!(chat["chat_template_kwargs"]["enable_thinking"], true);

        let (responses, _) = crate::openai::responses::encode(
            "model",
            request.clone(),
            Some(ReasoningAdapter::OpenaiResponses),
            &json!({}),
            false,
        )
        .unwrap();
        assert_eq!(responses["reasoning"]["effort"], "high");

        let (anthropic, _) = crate::anthropic::messages::encode(
            "model",
            request,
            Some(ReasoningAdapter::AnthropicMessages),
            &json!({}),
            false,
        )
        .unwrap();
        assert_eq!(anthropic["thinking"]["type"], "adaptive");
        assert_eq!(anthropic["output_config"]["effort"], "high");
    }

    #[test]
    fn image_blocks_use_each_provider_wire_shape() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("pixel.png");
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            1,
            1,
            image::Rgba([1, 2, 3, 255]),
        ))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
        let bytes = png.into_inner();
        std::fs::write(&path, &bytes).unwrap();
        let sha256 = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let user = ChatMessage::user_with_attachments(
            "What is this?",
            vec![Attachment::Image {
                name: "pixel.png".into(),
                mime_type: "image/png".into(),
                path,
                sha256,
            }],
        );
        let request = ChatRequest::new(vec![user]);

        let (chat, _) = crate::openai::chat_completions::encode(
            "model",
            request.clone(),
            None,
            &json!({}),
            false,
        )
        .unwrap();
        let (responses, _) =
            crate::openai::responses::encode("model", request.clone(), None, &json!({}), false)
                .unwrap();
        let (anthropic, _) =
            crate::anthropic::messages::encode("model", request, None, &json!({}), false).unwrap();

        assert!(
            chat["messages"][0]["content"][1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert_eq!(responses["input"][0]["content"][1]["type"], "input_image");
        assert_eq!(
            anthropic["messages"][0]["content"][1]["source"]["media_type"],
            "image/png"
        );
    }
}
