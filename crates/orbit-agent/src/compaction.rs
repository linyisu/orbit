//! Shared summary configuration for manual, automatic and recovery compaction.
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::model::ModelBackend;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CompactionSettings {
    /// Host-resolved provider::model; None follows the conversation model.
    pub model: Option<String>,
    pub max_input_tokens: usize,
    pub max_output_tokens: usize,
    pub request_timeout_seconds: u64,
    pub total_timeout_seconds: u64,
    /// Manual /compact may spend longer on a local model than automatic upkeep.
    pub manual_request_timeout_seconds: u64,
    pub manual_total_timeout_seconds: u64,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            model: None,
            max_input_tokens: 16_384,
            max_output_tokens: 4_096,
            request_timeout_seconds: 120,
            total_timeout_seconds: 360,
            manual_request_timeout_seconds: 180,
            manual_total_timeout_seconds: 3_600,
        }
    }
}

impl CompactionSettings {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1_024..=1_000_000).contains(&self.max_input_tokens)
            || !(256..=32_768).contains(&self.max_output_tokens)
        {
            return Err(
                "compaction max_input_tokens must be 1024..1000000 and max_output_tokens 256..32768",
            );
        }

        if self.request_timeout_seconds == 0
            || self.request_timeout_seconds > self.total_timeout_seconds
            || self.total_timeout_seconds > 3_600
            || self.manual_request_timeout_seconds == 0
            || self.manual_request_timeout_seconds > self.manual_total_timeout_seconds
            || self.manual_total_timeout_seconds > 3_600
        {
            return Err(
                "compaction timeouts must satisfy 1 <= request_timeout_seconds <= \
                    total_timeout_seconds <= 3600 and 1 <= manual_request_timeout_seconds <= \
                    manual_total_timeout_seconds <= 3600",
            );
        }

        if self.model.as_ref().is_some_and(|m| {
            m.split_once("::")
                .is_none_or(|(p, m)| p.is_empty() || m.is_empty())
        }) {
            return Err("compaction.model must be a configured provider::model");
        }

        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct CompactionRuntime {
    pub settings: CompactionSettings,
    pub backend: Option<Arc<dyn ModelBackend>>,
}

impl CompactionRuntime {
    pub fn for_input_window(&self, input: usize) -> Self {
        let mut runtime = self.clone();

        if runtime.backend.is_none() {
            runtime.settings.max_input_tokens = runtime
                .settings
                .max_input_tokens
                .min(input);
        }

        runtime
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionFailureKind {
    RequestTimeout,
    TotalTimeout,
    Upstream,
    InvalidSummary,
    /// Summary input or the preserved working set cannot fit its input allowance.
    InputBudget,
    InsufficientReduction,
    Archive,
}

impl CompactionFailureKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::RequestTimeout => "摘要请求超时",
            Self::TotalTimeout => "整理总时限已到",
            Self::Upstream => "摘要服务请求失败",
            Self::InvalidSummary => "摘要格式不完整",
            Self::InputBudget => "输入内容超出预算",
            Self::InsufficientReduction => "未释放足够上下文",
            Self::Archive => "历史归档失败",
        }
    }
}
