//! Application configuration, validation and runtime projection.

pub mod connections;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use orbit_agent::{agent::AgentConfig, model::ModelClientBackend};
use orbit_model::{
    client::{ApiFormat, ModelClient},
    types::{ModelConfig, ReasoningAdapter, ReasoningEffort},
};
use serde::Deserialize;

use crate::models as catalog;

pub const MIN_CONTEXT_TOKENS: usize = 8192;

#[derive(Debug, Deserialize)]
pub struct FileConfig {
    #[serde(skip)]
    pub auth_directory: Option<PathBuf>,
    #[serde(default = "default_context_tokens")]
    pub context_tokens: usize,
    #[serde(default = "default_auto_compact")]
    pub auto_compact: bool,
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u32,
    #[serde(default = "default_max_turns")]
    pub max_turns: usize,
    #[serde(default = "default_max_tool_calls")]
    pub max_tool_calls: usize,
    #[serde(default)]
    pub command: orbit_tools::run_command::CommandLimits,
    #[serde(default)]
    pub subagents: crate::subagents::SubagentConfig,
    #[serde(default)]
    pub compaction: orbit_agent::compaction::CompactionSettings,
    #[serde(default)]
    pub web: orbit_tools::web_fetch::WebConfig,
    #[serde(default)]
    pub mcp: orbit_tools::mcp::McpConfig,
    #[serde(default)]
    pub permissions: orbit_runtime::policy::WorkspacePermissions,
    #[serde(default)]
    pub tui: TuiConfig,
    pub providers: BTreeMap<String, ProviderConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct TuiConfig {
    pub cursor_trail: bool,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self { cursor_trail: true }
    }
}

#[derive(Clone, Deserialize)]
pub struct ProviderConfig {
    #[serde(default = "default_api_format")]
    pub api_format: ApiFormat,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub credential: Option<String>,
    pub models: Vec<ModelProfile>,
    // Explicit profiles remain authoritative across repeated catalog refreshes.
    #[serde(skip)]
    pub configured_models: Option<Vec<ModelProfile>>,
    #[serde(default)]
    pub managed_models: bool,
    #[serde(default = "default_auto_refresh_models")]
    pub auto_refresh_models: bool,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    /// Number of retries after a failed request that has not produced headers.
    #[serde(default = "orbit_model::types::default_max_retries")]
    pub max_retries: u8,
}

impl std::fmt::Debug for ProviderConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderConfig")
            .field("api_format", &self.api_format)
            .field("base_url", &self.base_url)
            .field("api_key", &"[redacted]")
            .field("credential", &self.credential)
            .field("models", &self.models)
            .field("timeout_seconds", &self.timeout_seconds)
            .field("max_retries", &self.max_retries)
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct ModelProfile {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Overrides the top-level context window for this model only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<usize>,
    /// Full usable input window for providers that allow output outside this
    /// input limit. When omitted, `context_tokens` keeps its total-budget
    /// meaning and Orbit reserves the configured output upper bound from it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_context_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningProfile>,
    #[serde(default, alias = "vision")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct ReasoningProfile {
    pub adapter: ReasoningAdapter,
    pub efforts: Vec<ReasoningEffort>,
    pub default: ReasoningEffort,
    #[serde(default)]
    pub extra: toml::Table,
}

fn default_max_output_tokens() -> u32 {
    32_000
}

fn default_auto_compact() -> bool {
    AgentConfig::default().auto_compact
}

fn default_auto_refresh_models() -> bool {
    true
}

fn default_context_tokens() -> usize {
    AgentConfig::default().context_tokens
}

fn default_max_turns() -> usize {
    AgentConfig::default().max_turns
}

fn default_max_tool_calls() -> usize {
    AgentConfig::default().max_tool_calls
}

pub fn validate_reasoning_extra(
    model: &str,
    api_format: ApiFormat,
    extra: &toml::Table,
) -> Result<()> {
    // `extra` is for provider extensions. These fields are owned by Orbit's
    // request encoder; accepting them here would silently replace the model,
    // transport shape, output budget, or tool contract.
    for key in [
        "model",
        "messages",
        "input",
        "stream",
        "tools",
        "tool_choice",
        "max_tokens",
        "max_output_tokens",
        "temperature",
    ] {
        if extra.contains_key(key) {
            bail!(
                "model {model:?}: reasoning.extra may not override the standard request field {key:?}"
            );
        }
    }

    let conflicts_effort = match api_format {
        ApiFormat::OpenaiChatCompletions => extra.contains_key("reasoning_effort"),
        ApiFormat::OpenaiResponses | ApiFormat::ChatgptCodex => extra
            .get("reasoning")
            .and_then(toml::Value::as_table)
            .is_some_and(|reasoning| reasoning.contains_key("effort")),

        ApiFormat::GoogleCodeAssist => !extra.is_empty(),
        ApiFormat::AnthropicMessages => extra
            .get("output_config")
            .and_then(toml::Value::as_table)
            .is_some_and(|output| output.contains_key("effort")),
    };

    if conflicts_effort {
        bail!("model {model:?}: reasoning.extra may not override the configured reasoning effort");
    }

    Ok(())
}

fn default_timeout_seconds() -> u64 {
    600
}

fn default_api_format() -> ApiFormat {
    ApiFormat::OpenaiChatCompletions
}

pub fn antigravity_profiles() -> Result<Vec<ModelProfile>> {
    orbit_model::account_catalog::antigravity_models()
        .iter()
        .map(|(id, model)| {
            let reasoning = model.thinking.as_ref().map(|thinking| {
                serde_json::json!({
                    "adapter": "google",
                    "efforts": thinking["efforts"],
                    "default": "high"
                })
            });

            Ok(serde_json::from_value(serde_json::json!({
                "id": id,
                "context_tokens": model.context_window,
                "supports_vision": model.input.iter().any(|kind| kind == "image"),
                "reasoning": reasoning
            }))?)
        })
        .collect()
}

impl FileConfig {
    pub fn merge_account_models(&mut self) -> Result<()> {
        for provider in self.providers.values_mut() {
            provider
                .configured_models
                .get_or_insert_with(|| provider.models.clone());
            if provider.managed_models
                || provider.api_format != ApiFormat::GoogleCodeAssist
                || provider.credential.is_none()
                || provider.base_url.trim_end_matches('/')
                    != "https://daily-cloudcode-pa.googleapis.com"
            {
                continue;
            }

            for model in antigravity_profiles()? {
                if !provider
                    .models
                    .iter()
                    .any(|existing| existing.id == model.id)
                {
                    provider.models.push(model);
                }
            }
        }

        Ok(())
    }

    pub fn load(path: &Path, auth_directory: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse(&content, auth_directory)
    }

    pub fn parse(content: &str, auth_directory: &Path) -> Result<Self> {
        let mut config: Self = toml::from_str(content).context("failed to parse configuration")?;

        config.auth_directory = Some(auth_directory.to_path_buf());
        config.merge_account_models()?;
        config.subagents.validate()?;

        if config.context_tokens < MIN_CONTEXT_TOKENS {
            bail!("context_tokens must be at least {MIN_CONTEXT_TOKENS}");
        }
        if config.max_output_tokens == 0
            || config.max_output_tokens as usize >= config.context_tokens
        {
            bail!("max_output_tokens must be positive and smaller than context_tokens");
        }

        for (name, provider) in &config.providers {
            if name.trim().is_empty()
                || provider.base_url.trim().is_empty()
                || (!provider.api_key.trim().is_empty() && provider.credential.is_some())
                || provider
                    .credential
                    .as_ref()
                    .is_some_and(|name| name.trim().is_empty())
                || provider.models.is_empty()
                || provider.timeout_seconds == 0
                || provider.max_retries > 5
                || provider
                    .models
                    .iter()
                    .any(|model| model.id.trim().is_empty())
            {
                bail!(
                    "provider {name:?} must set nonempty base_url, models, at most one of api_key or credential, a positive timeout_seconds, and max_retries from 0 to 5"
                );
            }
        }

        config.model_options()?;

        for (name, provider) in &config.providers {
            let mut models = std::collections::BTreeSet::new();

            for model in &provider.models {
                let output = model.max_output_tokens.unwrap_or(config.max_output_tokens);
                anyhow::ensure!(
                    output > 0,
                    "model {:?}: max_output_tokens must be positive",
                    model.id
                );
                if !models.insert(&model.id) {
                    bail!("provider {name:?} contains duplicate model {:?}", model.id);
                }

                if let Some(reasoning) = &model.reasoning {
                    let expected = match provider.api_format {
                        ApiFormat::OpenaiChatCompletions => ReasoningAdapter::OpenaiChat,
                        ApiFormat::OpenaiResponses | ApiFormat::ChatgptCodex => {
                            ReasoningAdapter::OpenaiResponses
                        }
                        ApiFormat::GoogleCodeAssist => ReasoningAdapter::Google,
                        ApiFormat::AnthropicMessages => ReasoningAdapter::AnthropicMessages,
                    };

                    if reasoning.adapter != expected {
                        bail!(
                            "model {:?}: reasoning adapter must match its provider API format",
                            model.id
                        );
                    }

                    if reasoning.efforts.is_empty()
                        || !reasoning.efforts.contains(&reasoning.default)
                    {
                        bail!(
                            "model {:?}: reasoning efforts must include its default",
                            model.id
                        );
                    }

                    for (index, effort) in reasoning.efforts.iter().enumerate() {
                        if reasoning.efforts[..index].contains(effort) {
                            bail!(
                                "model {:?}: reasoning effort {effort:?} is repeated",
                                model.id
                            );
                        }
                    }

                    validate_reasoning_extra(&model.id, provider.api_format, &reasoning.extra)?;
                }

                let input_context_tokens = model.input_context_tokens.unwrap_or_else(|| {
                    model
                        .context_tokens
                        .unwrap_or(config.context_tokens)
                        .saturating_sub(output as usize)
                });

                if input_context_tokens < MIN_CONTEXT_TOKENS {
                    bail!(
                        "model {:?}: input context window must be at least {MIN_CONTEXT_TOKENS} tokens",
                        model.id
                    );
                }
            }
        }

        config.compaction.validate().map_err(anyhow::Error::msg)?;

        if let Some(name) = &config.compaction.model {
            anyhow::ensure!(
                config
                    .model_options()?
                    .iter()
                    .any(|m| &m.qualified_name() == name),
                "compaction.model must name a configured provider::model"
            );
        }

        config.command.validate().map_err(anyhow::Error::msg)?;
        config.mcp.validate()?;

        Ok(config)
    }

    pub fn compaction_runtime(&self) -> Result<orbit_agent::compaction::CompactionRuntime> {
        let mut runtime = orbit_agent::compaction::CompactionRuntime {
            settings: self.compaction.clone(),
            backend: None,
        };

        if let Some(name) = &self.compaction.model {
            let choice = self
                .model_options()?
                .into_iter()
                .find(|m| &m.qualified_name() == name)
                .ok_or_else(|| {
                    anyhow::anyhow!("compaction.model must name a configured provider::model")
                })?;
            let entry = self.providers[&choice.provider]
                .models
                .iter()
                .find(|m| m.id == choice.model)
                .unwrap();
            let input = entry.input_context_tokens.unwrap_or_else(|| {
                entry
                    .context_tokens
                    .unwrap_or(self.context_tokens)
                    .saturating_sub(runtime.settings.max_output_tokens)
            });

            runtime.settings.max_input_tokens = runtime.settings.max_input_tokens.min(input);
            runtime.backend = Some(std::sync::Arc::new(ModelClientBackend::new(
                ModelClient::new(choice.config)?,
            )));
        }

        Ok(runtime)
    }

    pub fn agent_config(&self) -> AgentConfig {
        AgentConfig {
            context_tokens: self.context_tokens,
            auto_compact: self.auto_compact,
            max_turns: self.max_turns,
            max_tool_calls: self.max_tool_calls,
        }
    }

    pub fn model_options(&self) -> Result<Vec<catalog::ModelOption>> {
        let mut options = Vec::new();

        for (name, provider) in &self.providers {
            for model in &provider.models {
                let requested_output = provider
                    .configured_models
                    .as_ref()
                    .map_or(model.max_output_tokens, |models| {
                        models
                            .iter()
                            .find(|entry| entry.id == model.id)
                            .and_then(|entry| entry.max_output_tokens)
                    })
                    .unwrap_or(self.max_output_tokens);
                let max_output_tokens =
                    requested_output.min(model.max_output_tokens.unwrap_or(u32::MAX));
                let input_context_tokens = model.input_context_tokens.unwrap_or_else(|| {
                    model
                        .context_tokens
                        .unwrap_or(self.context_tokens)
                        .saturating_sub(max_output_tokens as usize)
                });
                let context_tokens = model.input_context_tokens.map_or_else(
                    || model.context_tokens.unwrap_or(self.context_tokens),
                    |input| input.saturating_add(max_output_tokens as usize),
                );
                let default_reasoning = crate::account_models::default_reasoning(
                    provider.api_format,
                    &provider.base_url,
                    &model.id,
                );
                let reasoning = model.reasoning.as_ref().or(default_reasoning.as_ref());
                let supports_vision = model.supports_vision.unwrap_or_else(|| {
                    crate::account_models::default_vision(
                        provider.api_format,
                        &provider.base_url,
                        &model.id,
                    )
                });

                options.push(catalog::ModelOption {
                    max_output_tokens,
                    provider: name.clone(),
                    model: model.id.clone(),
                    context_tokens,
                    input_context_tokens,
                    reasoning_efforts: reasoning
                        .map_or_else(Vec::new, |profile| profile.efforts.clone()),
                    supports_vision,
                    config: ModelConfig {
                        api_format: provider.api_format,
                        base_url: provider.base_url.clone(),
                        api_key: provider
                            .credential
                            .is_none()
                            .then(|| provider.api_key.clone())
                            .filter(|key| !key.is_empty()),
                        credential: provider
                            .credential
                            .as_ref()
                            .map(|name| {
                                Ok::<_, anyhow::Error>(orbit_auth::CredentialRef {
                                    name: name.clone(),
                                    directory: self.auth_directory.clone().context(
                                        "credential directory was not supplied by the host",
                                    )?,
                                })
                            })
                            .transpose()?,
                        model: model.id.clone(),
                        timeout: Duration::from_secs(provider.timeout_seconds),
                        max_retries: provider.max_retries,
                        reasoning_effort: reasoning.map(|profile| profile.default),
                        reasoning_adapter: reasoning.map(|profile| profile.adapter),
                        reasoning_extra: reasoning
                            .map(|profile| serde_json::to_value(&profile.extra))
                            .transpose()?
                            .unwrap_or_else(|| serde_json::json!({})),
                        supports_vision,
                    },
                });
            }
        }

        Ok(options)
    }

    pub fn select_model(&self, requested: &str) -> Result<catalog::ModelOption> {
        let requested = requested.trim();

        if requested.is_empty() {
            bail!("model must not be empty");
        }

        let options = self.model_options()?;

        if requested.contains("::") {
            return options
                .into_iter()
                .find(|option| option.qualified_name() == requested)
                .ok_or_else(|| anyhow::anyhow!("model {requested:?} is not configured"));
        }

        let matches = options
            .into_iter()
            .filter(|option| option.model == requested)
            .collect::<Vec<_>>();

        match matches.as_slice() {
            [option] => Ok(option.clone()),
            [] => bail!("model {requested:?} is not configured"),
            _ => bail!(
                "model {requested:?} is configured by multiple providers; use provider::model"
            ),
        }
    }

    pub fn agent_config_for(&self, context_tokens: usize) -> AgentConfig {
        AgentConfig {
            context_tokens,
            auto_compact: self.auto_compact,
            max_turns: self.max_turns,
            max_tool_calls: self.max_tool_calls,
        }
    }
}

#[cfg(test)]
mod tests;
