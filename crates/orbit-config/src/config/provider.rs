//! Persisted declarations are distinct from resolved, refreshable model metadata.
use super::{ModelProfile, ReasoningProfile};
use anyhow::{Result, ensure};
use orbit_model::{
    client::ApiFormat,
    types::{ReasoningAdapter, ReasoningEffort},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ops::{Deref, DerefMut},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ModelSelection {
    Explicit(Vec<String>),
    Automatic(Automatic),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Automatic {
    #[serde(rename = "auto")]
    Auto,
}

impl ModelSelection {
    pub fn auto() -> Self {
        Self::Automatic(Automatic::Auto)
    }

    pub fn is_auto(&self) -> bool {
        matches!(self, Self::Automatic(_))
    }
}

#[derive(Clone, Default, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_context_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningOverride>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasoningOverride {
    pub efforts: Vec<ReasoningEffort>,
    pub default: ReasoningEffort,
    #[serde(default, skip_serializing_if = "toml::Table::is_empty")]
    pub extra: toml::Table,
}

impl ModelOverrides {
    pub fn from_profile(model: &ModelProfile) -> Self {
        Self {
            max_output_tokens: model.max_output_tokens,
            context_tokens: model.context_tokens,
            input_context_tokens: model.input_context_tokens,
            supports_vision: model.supports_vision,
            reasoning: model
                .reasoning
                .as_ref()
                .map(|r| ReasoningOverride {
                    efforts: r.efforts.clone(),
                    default: r.default,
                    extra: r.extra.clone(),
                }),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.max_output_tokens.is_none()
            && self.context_tokens.is_none()
            && self.input_context_tokens.is_none()
            && self.supports_vision.is_none()
            && self.reasoning.is_none()
    }

    pub fn apply(&self, model: &mut ModelProfile, format: ApiFormat) {
        model.max_output_tokens = self
            .max_output_tokens
            .or(model.max_output_tokens);
        model.context_tokens = self
            .context_tokens
            .or(model.context_tokens);
        model.input_context_tokens = self
            .input_context_tokens
            .or(model.input_context_tokens);
        model.supports_vision = self
            .supports_vision
            .or(model.supports_vision);
        if let Some(reasoning) = &self.reasoning {
            model.reasoning = Some(ReasoningProfile {
                adapter: adapter(format),
                efforts: reasoning.efforts.clone(),
                default: reasoning.default,
                extra: reasoning.extra.clone(),
            });
        }
    }
}

pub fn adapter(format: ApiFormat) -> ReasoningAdapter {
    match format {
        ApiFormat::OpenaiChatCompletions => ReasoningAdapter::OpenaiChat,
        ApiFormat::OpenaiResponses | ApiFormat::ChatgptCodex => ReasoningAdapter::OpenaiResponses,
        ApiFormat::AnthropicMessages => ReasoningAdapter::AnthropicMessages,
        ApiFormat::GoogleCodeAssist => ReasoningAdapter::Google,
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSpec {
    #[serde(default = "super::default_api_format")]
    pub api_format: ApiFormat,
    pub base_url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
    pub models: ModelSelection,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_overrides: BTreeMap<String, ModelOverrides>,
    #[serde(default = "super::default_auto_refresh_models")]
    pub auto_refresh_models: bool,
    #[serde(default = "super::default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "orbit_model::types::default_max_retries")]
    pub max_retries: u8,
}

impl std::fmt::Debug for ProviderSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderSpec")
            .field("api_format", &self.api_format)
            .field("base_url", &self.base_url)
            .field("api_key", &"[redacted]")
            .field("models", &self.models)
            .finish_non_exhaustive()
    }
}

impl ProviderSpec {
    pub fn new(format: ApiFormat, endpoint: impl Into<String>, models: ModelSelection) -> Self {
        Self {
            api_format: format,
            base_url: endpoint.into(),
            models,
            api_key: String::new(),
            credential: None,
            model_overrides: BTreeMap::new(),
            auto_refresh_models: true,
            timeout_seconds: super::default_timeout_seconds(),
            max_retries: orbit_model::types::default_max_retries(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if let ModelSelection::Explicit(ids) = &self.models {
            let unique = ids
                .iter()
                .collect::<std::collections::BTreeSet<_>>();
            ensure!(unique.len() == ids.len(), "duplicate model ID in provider");
            ensure!(
                ids.iter()
                    .all(|id| !id.trim().is_empty()
                        && id.len() <= 256
                        && !id.chars().any(char::is_control)),
                "models must contain nonempty IDs"
            );
            for id in self.model_overrides.keys() {
                ensure!(
                    ids.contains(id),
                    "model override {id:?} is absent from models"
                );
            }
        }
        for (id, settings) in &self.model_overrides {
            ensure!(!id.trim().is_empty(), "model override ID must not be empty");
            ensure!(
                settings.max_output_tokens != Some(0),
                "model {id:?}: output limit must be positive"
            );
            ensure!(
                settings
                    .context_tokens
                    .is_none_or(|n| n >= super::MIN_CONTEXT_TOKENS)
                    && settings
                        .input_context_tokens
                        .is_none_or(|n| n >= super::MIN_CONTEXT_TOKENS),
                "model {id:?}: context window too small"
            );
            if let Some(r) = &settings.reasoning {
                ensure!(
                    !r.efforts.is_empty() && r.efforts.contains(&r.default),
                    "model {id:?}: reasoning efforts must include its default"
                );
                ensure!(
                    r.efforts
                        .iter()
                        .enumerate()
                        .all(|(i, e)| !r.efforts[..i].contains(e)),
                    "model {id:?}: repeated reasoning effort"
                );
                super::validate_reasoning_extra(id, self.api_format, &r.extra)?;
            }
        }
        Ok(())
    }

    pub fn set_models(&mut self, models: &[ModelProfile]) {
        self.models = ModelSelection::Explicit(
            models
                .iter()
                .map(|m| m.id.clone())
                .collect(),
        );
        self.model_overrides = models
            .iter()
            .filter_map(|m| {
                let settings = ModelOverrides::from_profile(m);
                (!settings.is_empty()).then(|| (m.id.clone(), settings))
            })
            .collect();
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(from = "ProviderSpec")]
pub struct ProviderConfig {
    pub spec: ProviderSpec,
    /// Resolved projection; never serialize this into the user's configuration.
    pub models: Vec<ModelProfile>,
    pub(crate) discovered: Option<Vec<ModelProfile>>,
}

impl Deref for ProviderConfig {
    type Target = ProviderSpec;
    fn deref(&self) -> &ProviderSpec {
        &self.spec
    }
}

impl DerefMut for ProviderConfig {
    fn deref_mut(&mut self) -> &mut ProviderSpec {
        &mut self.spec
    }
}

impl From<ProviderSpec> for ProviderConfig {
    fn from(spec: ProviderSpec) -> Self {
        let mut result = Self {
            spec,
            models: Vec::new(),
            discovered: None,
        };
        result.resolve();
        result
    }
}

impl ProviderConfig {
    pub fn resolve(&mut self) {
        let base = self
            .discovered
            .clone()
            .unwrap_or_else(|| self.bootstrap());
        self.models = match &self.spec.models {
            ModelSelection::Explicit(ids) => ids
                .iter()
                .map(|id| {
                    base.iter()
                        .find(|m| &m.id == id)
                        .cloned()
                        .unwrap_or_else(|| ModelProfile::new(id))
                })
                .collect(),
            ModelSelection::Automatic(_) => base,
        };
        for model in &mut self.models {
            if let Some(settings) = self.spec.model_overrides.get(&model.id) {
                settings.apply(model, self.spec.api_format);
            }
        }
    }

    pub(crate) fn bootstrap(&self) -> Vec<ModelProfile> {
        match (self.api_format, self.base_url.trim_end_matches('/')) {
            (ApiFormat::GoogleCodeAssist, "https://daily-cloudcode-pa.googleapis.com") => {
                super::antigravity_profiles().unwrap_or_default()
            }
            (ApiFormat::ChatgptCodex, "https://chatgpt.com/backend-api/codex")
            | (ApiFormat::GoogleCodeAssist, "https://cloudcode-pa.googleapis.com") => {
                let name = if self.api_format == ApiFormat::ChatgptCodex {
                    "chatgpt"
                } else {
                    "google"
                };
                #[derive(Deserialize)]
                struct Accounts {
                    google: Vec<ModelProfile>,
                    chatgpt: Vec<ModelProfile>,
                }
                let catalog: Accounts =
                    serde_json::from_str(include_str!("../models/account_models/bootstrap.json"))
                        .expect("validated account bootstrap catalog");
                if name == "chatgpt" {
                    catalog.chatgpt
                } else {
                    catalog.google
                }
            }
            _ => Vec::new(),
        }
    }
}

impl ModelProfile {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            max_output_tokens: None,
            context_tokens: None,
            input_context_tokens: None,
            reasoning: None,
            supports_vision: None,
        }
    }
}

impl Serialize for ProviderConfig {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.spec.serialize(serializer)
    }
}
