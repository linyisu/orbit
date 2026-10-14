use anyhow::{Result, ensure};
use orbit_agent::agent::AgentConfig;
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SubagentConfig {
    pub enabled: bool,
    pub max_concurrent: usize,
    pub max_turns: usize,
    pub max_tool_calls: usize,
    pub timeout_seconds: u64,
    pub explore_model: Option<String>,
    pub review_model: Option<String>,
}

impl Default for SubagentConfig {
    fn default() -> Self {
        let agent = AgentConfig::default();

        Self {
            enabled: true,
            max_concurrent: 8,
            max_turns: agent.max_turns,
            max_tool_calls: agent.max_tool_calls,
            timeout_seconds: 1_800,
            explore_model: None,
            review_model: None,
        }
    }
}

impl SubagentConfig {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=128).contains(&self.max_concurrent),
            "subagents.max_concurrent must be between 1 and 128"
        );
        ensure!(
            self.timeout_seconds <= 86_400,
            "subagents.timeout_seconds must be between 0 and 86400 (0 disables progress reminders)"
        );
        for model in [&self.explore_model, &self.review_model]
            .into_iter()
            .flatten()
        {
            ensure!(
                !model.trim().is_empty(),
                "subagent model override must not be empty"
            );
        }

        Ok(())
    }
}
