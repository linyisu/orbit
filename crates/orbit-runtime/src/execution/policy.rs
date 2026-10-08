use super::resources::ResourceLimits;
use std::collections::BTreeMap;

/// Native process configuration. Tool approval is evaluated before execution.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecutionPolicy {
    pub resources: ResourceLimits,
    pub env_overrides: BTreeMap<String, String>,
}
