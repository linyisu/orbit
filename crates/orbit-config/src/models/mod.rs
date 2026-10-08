pub mod account_models;
pub mod model_metadata;
pub mod updater;

#[derive(Clone, Debug)]
pub struct ModelOption {
    pub max_output_tokens: u32,
    pub provider: String,
    pub model: String,
    /// Request budget passed to the agent, including the output upper bound.
    pub context_tokens: usize,
    /// Full usable input window shown to the user and used for compaction.
    pub input_context_tokens: usize,
    pub reasoning_efforts: Vec<orbit_model::types::ReasoningEffort>,
    pub supports_vision: bool,
    pub config: orbit_model::types::ModelConfig,
}

impl ModelOption {
    pub fn qualified_name(&self) -> String {
        format!("{}::{}", self.provider, self.model)
    }
}
