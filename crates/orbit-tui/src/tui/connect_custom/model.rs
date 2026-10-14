//! Editable model drafts, committed only with the connection.
use anyhow::Result;
use orbit_client::protocol::model::ReasoningEffort;
use orbit_config::ApiFormat;

use super::form::Field;
use crate::config::{ModelProfile, ReasoningProfile};

pub(super) fn blank(id: &str) -> ModelProfile {
    ModelProfile::new(id)
}

const EFFORTS: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

pub(super) fn fields(model: &ModelProfile) -> Vec<Field> {
    vec![
        Field::text("模型 ID", &model.id, "服务端请求使用的准确标识"),
        Field::text(
            "上下文容量",
            &model
                .context_tokens
                .map_or_else(String::new, |v| v.to_string()),
            "",
        ),
        Field::text(
            "独立输入窗口",
            &model
                .input_context_tokens
                .map_or_else(String::new, |v| v.to_string()),
            "",
        ),
        Field::text(
            "最大输出 tokens",
            &model
                .max_output_tokens
                .map_or_else(String::new, |v| v.to_string()),
            "",
        ),
        Field::choice(
            "图片输入",
            match model.supports_vision {
                Some(true) => "支持",
                Some(false) => "不支持",
                None => "继承服务端",
            },
            &["继承服务端", "不支持", "支持"],
        ),
    ]
}

pub(super) fn read(
    fields: &[Field],
    model: &ModelProfile,
    default_output: u32,
    default_context: usize,
) -> Result<ModelProfile> {
    let id = fields[0].value();
    orbit_config::config::connections::models::validate_id(id)?;
    let mut updated = model.clone();
    updated.id = id.into();
    updated = orbit_config::config::connections::models::limits(
        &updated,
        [fields[1].value(), fields[2].value(), fields[3].value()],
        Some((default_context, default_output)),
    )?;
    updated.supports_vision = match fields[4].value() {
        "支持" => Some(true),
        "不支持" => Some(false),
        _ => None,
    };
    Ok(updated)
}

pub(super) fn reasoning_fields(current: &Option<ReasoningProfile>) -> Vec<Field> {
    let mut fields = vec![Field::choice(
        "思考设置",
        if current.is_some() {
            "自定义"
        } else {
            "继承服务端"
        },
        &["继承服务端", "自定义"],
    )];

    for effort in EFFORTS {
        let enabled = current.as_ref().is_some_and(|p| {
            p.efforts.iter().any(|e| {
                serde_json::to_value(e)
                    .ok()
                    .as_ref()
                    .and_then(|v| v.as_str())
                    == Some(effort)
            })
        });

        fields.push(Field::choice(
            effort,
            if enabled { "启用" } else { "关闭" },
            &["启用", "关闭"],
        ));
    }

    let default = current
        .as_ref()
        .and_then(|p| serde_json::to_value(p.default).ok())
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();

    fields.push(Field::choice("默认强度", &default, &EFFORTS));
    fields
}

pub(super) fn read_reasoning(
    fields: &[Field],
    current: &Option<ReasoningProfile>,
    format: ApiFormat,
) -> Result<Option<ReasoningProfile>> {
    if fields[0].value() == "继承服务端" {
        return Ok(None);
    }
    let efforts = EFFORTS
        .iter()
        .enumerate()
        .filter(|(i, _)| fields[i + 1].value() == "启用")
        .map(|(_, effort)| serde_json::from_value::<ReasoningEffort>(serde_json::json!(effort)))
        .collect::<Result<Vec<_>, _>>()?;
    let default = serde_json::from_value(serde_json::json!(fields[8].value()))?;
    orbit_config::config::connections::models::reasoning(current.as_ref(), format, efforts, default)
        .map(Some)
}
