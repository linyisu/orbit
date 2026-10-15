//! Public serving metadata supplements known choices without granting account access.
use super::{MIN_CONTEXT_TOKENS, ModelProfile, ReasoningProfile};
use crate::{
    client::ApiFormat,
    types::{ReasoningAdapter, ReasoningEffort},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::LazyLock};

/// Pinned capability metadata remains a fallback, never an account model directory.
pub fn bundled() -> &'static Catalog {
    static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
        serde_json::from_str(include_str!("public_metadata.json"))
            .expect("validated model metadata snapshot")
    });
    &CATALOG
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Catalog {
    pub source: Value,
    pub providers: BTreeMap<String, Provider>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Provider {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<String>,
    pub models: BTreeMap<String, Spec>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct Spec {
    #[serde(default)]
    pub limit: Limits,
    #[serde(default)]
    pub modalities: Modalities,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_options: Vec<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct Limits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct Modalities {
    #[serde(default)]
    pub input: Vec<String>,
}

/// Decode provider specifications while retaining their source provenance.
pub fn normalize(payload: Value, source: Value) -> Result<Catalog> {
    let entries = payload
        .as_object()
        .context("model metadata is not a provider catalog")?;
    let mut providers = BTreeMap::new();
    for (id, entry) in entries {
        let Some(models) = entry["models"].as_object() else {
            continue;
        };
        let models = models
            .iter()
            .filter_map(|(id, spec)| {
                if !super::valid_model_id(id) {
                    return None;
                }
                serde_json::from_value(spec.clone())
                    .ok()
                    .map(|spec| (id.clone(), spec))
            })
            .collect::<BTreeMap<_, Spec>>();
        if !models.is_empty() {
            providers.insert(
                id.clone(),
                Provider {
                    api: entry["api"].as_str().map(str::to_owned),
                    models,
                },
            );
        }
    }
    ensure!(
        !providers.is_empty(),
        "model metadata contains no usable providers"
    );
    Ok(Catalog { source, providers })
}

pub fn supports(format: ApiFormat) -> bool {
    matches!(
        format,
        ApiFormat::OpenaiChatCompletions
            | ApiFormat::OpenaiResponses
            | ApiFormat::ChatgptCodex
            | ApiFormat::AnthropicMessages
    )
}

fn endpoint(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let path = url.path().trim_end_matches('/');
    let path = path.strip_suffix("/v1").unwrap_or(path);
    Some(format!("{}{path}", url.origin().ascii_serialization()))
}

impl Catalog {
    /// Fill missing capabilities only when the endpoint and model ID match unambiguous metadata.
    pub fn supplement(&self, format: ApiFormat, base: &str, models: &mut [ModelProfile]) {
        if !supports(format) {
            return;
        }
        let Some(base) = endpoint(base) else {
            return;
        };
        let catalogs = self
            .providers
            .iter()
            .filter(|(id, entry)| {
                let api = entry
                    .api
                    .as_deref()
                    .or(match id.as_str() {
                        "openai" => Some("https://api.openai.com/v1"),
                        "anthropic" => Some("https://api.anthropic.com/v1"),
                        _ => None,
                    });
                api.and_then(endpoint).as_ref() == Some(&base)
            })
            .collect::<Vec<_>>();
        if catalogs.is_empty() {
            return;
        }
        for model in models {
            let mut specs = catalogs
                .iter()
                .filter_map(|(id, catalog)| {
                    catalog
                        .models
                        .get(&model.id)
                        .map(|spec| (id.as_str(), spec))
                });
            let Some((id, spec)) = specs.next() else {
                continue;
            };
            if specs.any(|(_, other)| other != spec) {
                continue;
            }
            spec.supplement(model, id, format);
        }
    }
}

impl Spec {
    /// Supplement missing fields after explicit profiles and live serving metadata are applied.
    /// Callers must first match the endpoint and exact model ID.
    pub fn supplement(&self, model: &mut ModelProfile, provider: &str, format: ApiFormat) {
        model.context_tokens = model.context_tokens.or(self
            .limit
            .context
            .filter(|n| *n >= MIN_CONTEXT_TOKENS));
        model.input_context_tokens = model
            .input_context_tokens
            .or(self.limit.input.filter(|n| {
                *n >= MIN_CONTEXT_TOKENS
                    && model
                        .context_tokens
                        .is_none_or(|context| *n <= context)
            }));
        model.max_output_tokens = model
            .max_output_tokens
            .or(self.limit.output.filter(|n| *n > 0));
        if model.supports_vision.is_none() && !self.modalities.input.is_empty() {
            model.supports_vision = Some(
                self.modalities
                    .input
                    .iter()
                    .any(|kind| kind == "image"),
            );
        }
        if model.reasoning.is_none() {
            let adapter = match (provider, format) {
                ("openai", ApiFormat::OpenaiChatCompletions) => Some(ReasoningAdapter::OpenaiChat),
                ("openai", ApiFormat::OpenaiResponses | ApiFormat::ChatgptCodex) => {
                    Some(ReasoningAdapter::OpenaiResponses)
                }
                ("anthropic", ApiFormat::AnthropicMessages) => {
                    Some(ReasoningAdapter::AnthropicMessages)
                }
                _ => None,
            };
            if let Some(adapter) = adapter {
                let efforts = self
                    .reasoning_options
                    .iter()
                    .filter(|option| option["type"] == "effort")
                    .filter_map(|option| option["values"].as_array())
                    .flatten()
                    .filter_map(|value| {
                        serde_json::from_value::<ReasoningEffort>(value.clone()).ok()
                    })
                    .collect::<Vec<_>>();
                if let Some(default) = efforts
                    .iter()
                    .find(|e| **e == ReasoningEffort::High)
                    .or_else(|| efforts.last())
                    .copied()
                {
                    model.reasoning = Some(ReasoningProfile {
                        adapter,
                        efforts,
                        default,
                        extra: toml::Table::new(),
                    });
                }
            }
        }
    }
}
