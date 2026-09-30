//! Application configuration, validation and runtime projection.
use crate::tui;
use anyhow::{Context, Result, bail};
use orbit_agent::{agent::AgentConfig, model::ModelClientBackend};
use orbit_model::{
    client::{ApiFormat, ModelClient},
    types::{ModelConfig, ReasoningAdapter, ReasoningEffort},
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Deserialize)]
pub(crate) struct FileConfig {
    #[serde(skip)]
    pub(crate) auth_directory: Option<PathBuf>,
    pub(crate) default_provider: String,
    #[serde(default = "default_context_tokens")]
    pub(crate) context_tokens: usize,
    #[serde(default = "default_auto_compact")]
    pub(crate) auto_compact: bool,
    #[serde(default = "default_max_output_tokens")]
    pub(crate) max_output_tokens: u32,
    #[serde(default = "default_max_turns")]
    pub(crate) max_turns: usize,
    #[serde(default = "default_max_tool_calls")]
    pub(crate) max_tool_calls: usize,
    #[serde(default)]
    pub(crate) command: orbit_tools::run_command::CommandLimits,
    #[serde(default)]
    pub(crate) compaction: orbit_agent::compaction::CompactionSettings,
    #[serde(default)]
    pub(crate) web: orbit_tools::web_fetch::WebConfig,
    #[serde(default)]
    pub(crate) mcp: orbit_tools::mcp::McpConfig,
    #[serde(default)]
    pub(crate) permissions: orbit_runtime::policy::WorkspacePermissions,
    pub(crate) providers: BTreeMap<String, ProviderConfig>,
}

#[derive(Clone, Deserialize)]
pub(crate) struct ProviderConfig {
    #[serde(default = "default_api_format")]
    pub(crate) api_format: ApiFormat,
    pub(crate) base_url: String,
    #[serde(default)]
    pub(crate) api_key: String,
    #[serde(default)]
    pub(crate) credential: Option<String>,
    /// The first model is selected when Orbit starts with this provider.
    pub(crate) models: Vec<ModelProfile>,
    #[serde(default)]
    pub(crate) managed_models: bool,
    #[serde(default = "default_timeout_seconds")]
    pub(crate) timeout_seconds: u64,
    /// Number of retries after a failed request that has not produced headers.
    #[serde(default = "orbit_model::types::default_max_retries")]
    pub(crate) max_retries: u8,
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
pub(crate) struct ModelProfile {
    pub(crate) id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) max_output_tokens: Option<u32>,
    /// Overrides the top-level context window for this model only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) context_tokens: Option<usize>,
    /// Full usable input window for providers that allow output outside this
    /// input limit. When omitted, `context_tokens` keeps its total-budget
    /// meaning and Orbit reserves the configured output upper bound from it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) input_context_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning: Option<ReasoningProfile>,
    #[serde(default, alias = "vision")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) supports_vision: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub(crate) struct ReasoningProfile {
    pub(crate) adapter: ReasoningAdapter,
    pub(crate) efforts: Vec<ReasoningEffort>,
    pub(crate) default: ReasoningEffort,
    #[serde(default)]
    pub(crate) extra: toml::Table,
}

fn default_max_output_tokens() -> u32 {
    32_000
}

fn default_auto_compact() -> bool {
    true
}

fn default_context_tokens() -> usize {
    128_000
}

fn default_max_turns() -> usize {
    300
}

fn default_max_tool_calls() -> usize {
    2000
}

fn validate_reasoning_extra(model: &str, api_format: ApiFormat, extra: &toml::Table) -> Result<()> {
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

pub(crate) fn antigravity_profiles() -> Result<Vec<ModelProfile>> {
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
    pub(crate) fn merge_account_models(&mut self) -> Result<()> {
        for provider in self.providers.values_mut() {
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

    pub(crate) fn load(path: &Path, auth_directory: &Path) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        Self::parse(&content, auth_directory)
    }

    pub(crate) fn parse(content: &str, auth_directory: &Path) -> Result<Self> {
        let mut config: Self = toml::from_str(content).context("failed to parse configuration")?;

        config.auth_directory = Some(auth_directory.to_path_buf());
        config.merge_account_models()?;

        if config.context_tokens < 8192 {
            bail!("context_tokens must be at least 8192");
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

        let options = config.model_options()?;

        if !options
            .iter()
            .any(|option| option.provider == config.default_provider && option.default_for_provider)
        {
            bail!(
                "default_provider {:?} does not name a configured provider",
                config.default_provider
            );
        }

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

                if input_context_tokens < 8192 {
                    bail!(
                        "model {:?}: input context window must be at least 8192 tokens",
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

    pub(crate) fn compaction_runtime(&self) -> Result<orbit_agent::compaction::CompactionRuntime> {
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

    pub(crate) fn agent_config(&self) -> AgentConfig {
        AgentConfig {
            context_tokens: self.context_tokens,
            auto_compact: self.auto_compact,
            max_turns: self.max_turns,
            max_tool_calls: self.max_tool_calls,
        }
    }

    pub(crate) fn model_options(&self) -> Result<Vec<tui::ModelOption>> {
        let mut options = Vec::new();

        for (name, provider) in &self.providers {
            for (index, model) in provider.models.iter().enumerate() {
                let max_output_tokens = model.max_output_tokens.unwrap_or(self.max_output_tokens);
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
                let reasoning = model.reasoning.as_ref();
                let supports_vision = model.supports_vision.unwrap_or_else(|| {
                    crate::account_models::default_vision(
                        provider.api_format,
                        &provider.base_url,
                        &model.id,
                    )
                });
                options.push(tui::ModelOption {
                    max_output_tokens,
                    provider: name.clone(),
                    model: model.id.clone(),
                    context_tokens,
                    input_context_tokens,
                    default_for_provider: index == 0,
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

    pub(crate) fn default_model_option(&self) -> Result<tui::ModelOption> {
        self.model_options()?
            .into_iter()
            .find(|option| option.provider == self.default_provider && option.default_for_provider)
            .ok_or_else(|| anyhow::anyhow!("default_provider is not configured"))
    }

    pub(crate) fn agent_config_for(&self, context_tokens: usize) -> AgentConfig {
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
