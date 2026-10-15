//! Provider catalogs and capability decoding. No configuration files or credential storage.
mod antigravity;
mod api;
mod chatgpt;
mod deepseek;
mod google;
pub mod public_metadata;
mod types;

use crate::client::ApiFormat;
use anyhow::{Result, ensure};
pub use deepseek::{default_reasoning, default_vision};
use serde_json::{Value, json};
pub use types::{ModelProfile, ReasoningProfile, validate_reasoning_extra};

/// Minimum usable model context accepted by the request projection.
pub const MIN_CONTEXT_TOKENS: usize = 8_192;

/// Maximum UTF-8 byte length accepted for a provider model identifier.
pub const MAX_MODEL_ID_BYTES: usize = 256;

/// Accept bounded opaque IDs without control characters; provider naming remains authoritative.
pub fn valid_model_id(id: &str) -> bool {
    !id.trim().is_empty() && id.len() <= MAX_MODEL_ID_BYTES && !id.chars().any(char::is_control)
}

/// Whether omissions in a response are evidence that a model has been removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CatalogCoverage {
    Complete,
    Partial,
}

/// A decoded provider response, retaining the authority of its availability evidence.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ModelCatalog {
    pub models: Vec<ModelProfile>,
    pub unavailable: Vec<String>,
    pub coverage: CatalogCoverage,
}

impl ModelCatalog {
    /// Complete directories replace prior availability; partial observations only update known IDs.
    pub fn resolve(&self, mut previous: Vec<ModelProfile>) -> Vec<ModelProfile> {
        if self.coverage == CatalogCoverage::Complete {
            return self.models.clone();
        }
        for model in &self.models {
            if let Some(existing) = previous
                .iter_mut()
                .find(|entry| entry.id == model.id)
            {
                *existing = model.clone();
            } else {
                previous.push(model.clone());
            }
        }
        previous
    }

    /// Validate persisted normalized records before they become model choices.
    pub fn validate(&self, format: ApiFormat) -> Result<()> {
        let mut ids = std::collections::HashSet::new();
        for model in &self.models {
            ensure!(ids.insert(&model.id), "模型目录包含重复 ID：{}", model.id);
            ensure!(valid_model_id(&model.id), "模型 ID 无效");
            ensure!(
                model
                    .context_tokens
                    .is_none_or(|n| n >= MIN_CONTEXT_TOKENS)
                    && model
                        .input_context_tokens
                        .is_none_or(|n| n >= MIN_CONTEXT_TOKENS)
                    && model.max_output_tokens != Some(0),
                "模型容量无效：{}",
                model.id
            );
            if let Some(reasoning) = &model.reasoning {
                ensure!(
                    reasoning.adapter == format.reasoning_adapter()
                        && reasoning
                            .efforts
                            .contains(&reasoning.default),
                    "模型思考能力无效：{}",
                    model.id
                );
                validate_reasoning_extra(&model.id, format, &reasoning.extra)?;
            }
        }
        Ok(())
    }
}

/// Decode only the provider wire protocol. Never add bundled model IDs to availability.
pub fn parse(format: ApiFormat, payload: &Value) -> Result<ModelCatalog> {
    let (models, unavailable, coverage) = match format {
        ApiFormat::OpenaiChatCompletions
        | ApiFormat::OpenaiResponses
        | ApiFormat::AnthropicMessages => (
            api::parse(format, payload)?,
            Vec::new(),
            CatalogCoverage::Complete,
        ),
        ApiFormat::ChatgptCodex => (
            chatgpt::parse(payload)?,
            Vec::new(),
            CatalogCoverage::Complete,
        ),
        ApiFormat::GoogleCodeAssist => {
            if let Some(buckets) = payload["buckets"].as_array() {
                (
                    google::parse(buckets)?,
                    Vec::new(),
                    CatalogCoverage::Partial,
                )
            } else {
                let (models, unavailable) = antigravity::normalize(payload)?;
                (models, unavailable, CatalogCoverage::Complete)
            }
        }
    };
    let catalog = ModelCatalog {
        models,
        unavailable,
        coverage,
    };
    catalog.validate(format)?;
    Ok(catalog)
}

/// Compatibility projection for consumers that only display a decoded catalog.
pub fn normalize(format: ApiFormat, payload: &Value) -> Result<(Vec<ModelProfile>, Vec<String>)> {
    let catalog = parse(format, payload)?;
    Ok((catalog.models, catalog.unavailable))
}

fn profile(
    id: &str,
    context: Option<usize>,
    vision: Option<bool>,
    reasoning: Option<Value>,
) -> Result<ModelProfile> {
    ensure!(valid_model_id(id), "无效模型 ID");
    ensure!(
        context.is_none_or(|context| context >= MIN_CONTEXT_TOKENS),
        "模型上下文容量过小：{id}"
    );
    Ok(serde_json::from_value(json!({
        "id": id,
        "context_tokens": context,
        "supports_vision": vision,
        "reasoning": reasoning
    }))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_observations_preserve_known_models_but_complete_empty_catalogs_clear_them() {
        let previous = vec![
            ModelProfile::new("generated-first"),
            ModelProfile::new("generated-updated"),
        ];
        let mut updated = ModelProfile::new("generated-updated");
        updated.context_tokens = Some(128_000);
        let mut catalog = ModelCatalog {
            models: vec![updated, ModelProfile::new("generated-next")],
            unavailable: Vec::new(),
            coverage: CatalogCoverage::Partial,
        };
        let resolved = catalog.resolve(previous);
        assert_eq!(
            resolved
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["generated-first", "generated-updated", "generated-next"]
        );
        assert_eq!(resolved[1].context_tokens, Some(128_000));
        catalog.models.clear();
        assert_eq!(catalog.resolve(resolved.clone()).len(), 3);
        catalog.coverage = CatalogCoverage::Complete;
        assert!(catalog.resolve(resolved).is_empty());
    }
}
