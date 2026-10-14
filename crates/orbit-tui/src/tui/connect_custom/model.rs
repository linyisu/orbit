//! Editable model drafts, committed only with the connection.
use anyhow::{Result, ensure};
use orbit_model::{client::ApiFormat, types::ReasoningEffort};

use super::form::Field;
use crate::config::{ModelProfile, ReasoningProfile, reasoning_adapter as adapter};

pub(super) fn blank(id: &str) -> ModelProfile {
    ModelProfile::new(id)
}

fn number<T: std::str::FromStr>(field: &Field) -> Result<Option<T>> {
    if field.value().is_empty() {
        return Ok(None);
    }
    field
        .value()
        .parse()
        .map(Some)
        .map_err(|_| anyhow::anyhow!("{}必须为整数", field.label))
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
    let mut updated = model.clone();
    let id = fields[0].value();
    ensure!(
        !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control),
        "请填写有效的模型 ID"
    );
    updated.id = id.into();
    updated.context_tokens = number(&fields[1])?;
    updated.input_context_tokens = number(&fields[2])?;
    updated.max_output_tokens = number(&fields[3])?;
    let output = updated
        .max_output_tokens
        .unwrap_or(default_output);
    ensure!(output > 0, "输出上限必须大于 0");
    let input = updated
        .input_context_tokens
        .unwrap_or_else(|| {
            updated
                .context_tokens
                .unwrap_or(default_context)
                .saturating_sub(output as usize)
        });

    ensure!(input >= 8_192, "可用输入预算不能小于 8192 tokens");
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
    ensure!(efforts.contains(&default), "默认强度必须在已启用的强度中");
    Ok(Some(ReasoningProfile {
        adapter: adapter(format),
        efforts,
        default,
        extra: current
            .as_ref()
            .filter(|profile| profile.adapter == adapter(format))
            .map_or_else(Default::default, |profile| profile.extra.clone()),
    }))
}
