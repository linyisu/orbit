//! Deterministic, offline fixtures. Tests declare only the properties they exercise.
use crate::{
    config::{
        FileConfig, ModelProfile, ModelSelection, ProviderConfig, ProviderSpec, ReasoningProfile,
    },
    models::ModelOption,
};
use anyhow::Result;
use orbit_model::{client::ApiFormat, types::ReasoningEffort};
use std::{collections::BTreeMap, path::Path};

pub struct TestModel(ModelProfile);
impl TestModel {
    pub fn new(id: impl Into<String>) -> Self {
        Self(ModelProfile::new(id))
    }
    pub fn vision(mut self, value: bool) -> Self {
        self.0.supports_vision = Some(value);
        self
    }
    pub fn context(mut self, value: usize) -> Self {
        self.0.context_tokens = Some(value);
        self
    }
    pub fn input_context(mut self, value: usize) -> Self {
        self.0.input_context_tokens = Some(value);
        self
    }
    pub fn output(mut self, value: u32) -> Self {
        self.0.max_output_tokens = Some(value);
        self
    }
    pub fn reasoning(
        mut self,
        format: ApiFormat,
        efforts: &[ReasoningEffort],
        default: ReasoningEffort,
    ) -> Self {
        self.0.reasoning = Some(ReasoningProfile {
            adapter: crate::config::reasoning_adapter(format),
            efforts: efforts.to_vec(),
            default,
            extra: toml::Table::new(),
        });
        self
    }
    pub fn build(self) -> ModelProfile {
        self.0
    }
    pub fn option(self, provider: impl Into<String>) -> ModelOption {
        TestConfig::new()
            .provider(TestProvider::new(provider).models([self]))
            .build()
            .expect("valid test model")
            .model_options()
            .unwrap()
            .remove(0)
    }
}
impl From<&str> for TestModel {
    fn from(id: &str) -> Self {
        Self::new(id)
    }
}
impl From<ModelProfile> for TestModel {
    fn from(model: ModelProfile) -> Self {
        Self(model)
    }
}

pub struct TestProvider {
    pub name: String,
    pub spec: ProviderSpec,
}
impl TestProvider {
    pub fn new(name: impl Into<String>) -> Self {
        let mut spec = ProviderSpec::new(
            ApiFormat::OpenaiChatCompletions,
            "http://127.0.0.1:1/v1",
            ModelSelection::Explicit(vec!["test".into()]),
        );
        spec.auto_refresh_models = false;
        Self {
            name: name.into(),
            spec,
        }
    }
    pub fn responses(name: impl Into<String>) -> Self {
        Self::new(name).format(ApiFormat::OpenaiResponses)
    }
    pub fn format(mut self, value: ApiFormat) -> Self {
        self.spec.api_format = value;
        self
    }
    pub fn endpoint(mut self, value: impl Into<String>) -> Self {
        self.spec.base_url = value.into();
        self
    }
    pub fn key(mut self, value: impl Into<String>) -> Self {
        self.spec.api_key = value.into();
        self
    }
    pub fn credential(mut self, value: impl Into<String>) -> Self {
        self.spec.credential = Some(value.into());
        self
    }
    pub fn models(mut self, models: impl IntoIterator<Item = impl Into<TestModel>>) -> Self {
        self.spec.set_models(
            &models
                .into_iter()
                .map(|m| m.into().build())
                .collect::<Vec<_>>(),
        );
        self
    }
    pub fn automatic(mut self) -> Self {
        self.spec.models = ModelSelection::auto();
        self
    }
    pub fn refresh(mut self, value: bool) -> Self {
        self.spec.auto_refresh_models = value;
        self
    }
    pub fn build(self) -> ProviderConfig {
        self.spec.into()
    }
}

pub struct TestConfig {
    pub config: FileConfig,
}
impl Default for TestConfig {
    fn default() -> Self {
        Self::new()
    }
}
impl TestConfig {
    pub fn new() -> Self {
        Self {
            config: FileConfig {
                auth_directory: None,
                context_tokens: 128000,
                auto_compact: true,
                max_output_tokens: 32000,
                max_turns: 0,
                max_tool_calls: 2000,
                disabled_capabilities: Vec::new(),
                disabled_tools: Vec::new(),
                command: Default::default(),
                subagents: Default::default(),
                compaction: Default::default(),
                web: Default::default(),
                mcp: Default::default(),
                permissions: Default::default(),
                tui: Default::default(),
                providers: BTreeMap::new(),
            },
        }
    }
    pub fn provider(mut self, provider: TestProvider) -> Self {
        self.config
            .providers
            .insert(provider.name, provider.spec.into());
        self
    }
    pub fn auth(mut self, path: &Path) -> Self {
        self.config.auth_directory = Some(path.into());
        self
    }
    pub fn build(self) -> Result<FileConfig> {
        self.config.validate()?;
        Ok(self.config)
    }
    pub fn providers_toml(&self) -> Result<String> {
        let mut document = toml_edit::DocumentMut::new();
        for (name, provider) in &self.config.providers {
            document["providers"][name] =
                crate::config::connections::document::provider(&provider.spec)?;
        }
        Ok(document.to_string())
    }
    pub fn to_toml(&self) -> Result<String> {
        Ok(toml::to_string(&self.config)?)
    }
    pub fn write(&self, path: impl AsRef<Path>) -> Result<()> {
        std::fs::write(path, self.to_toml()?)?;
        Ok(())
    }
}
