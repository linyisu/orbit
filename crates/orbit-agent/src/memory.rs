//! Auditable compaction data, separate from model-authored prose.
use crate::{error::AgentError, events::AgentEvent};
use orbit_model::types::{ChatMessage, ToolCall};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileActivity {
    pub read: BTreeSet<String>,
    pub written: BTreeSet<String>,
}

impl FileActivity {
    /// Only successful built-in file-tool receipts count. A shell command or a
    /// model statement about a file is not evidence that a file was changed.
    pub fn observe(&mut self, call: &ToolCall, output: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(output) else {
            return;
        };
        let Some(path) = value.get("path").and_then(|v| v.as_str()) else {
            return;
        };
        match call.function.name.as_str() {
            "read_file"
                if value.get("content").is_some_and(|v| v.is_string())
                    && value.get("total_lines").is_some_and(|v| v.is_u64()) =>
            {
                self.read.insert(path.into());
            }
            "write_file" | "edit_file"
                if value.get("written_bytes").is_some_and(|v| v.is_u64()) =>
            {
                self.written.insert(path.into());
            }
            _ => {}
        }
    }

    pub fn merge(&mut self, other: &Self) {
        self.read.extend(other.read.iter().cloned());
        self.written.extend(other.written.iter().cloned());
    }

    pub fn from_history(history: &[ChatMessage]) -> Self {
        let mut files = Self::default();
        let mut calls = BTreeMap::new();
        for message in history {
            match message {
                ChatMessage::Assistant { tool_calls, .. } => {
                    calls.clear();
                    for call in tool_calls {
                        calls.insert(call.id.clone(), call);
                    }
                }
                ChatMessage::Tool {
                    tool_call_id,
                    content,
                } => {
                    if let Some(call) = calls.remove(tool_call_id) {
                        files.observe(call, content);
                    }
                }
                _ => {
                    calls.clear();
                }
            }
        }
        files
    }

    pub fn from_events<'a>(events: impl IntoIterator<Item = &'a AgentEvent>) -> Self {
        let mut files = Self::default();
        for event in events {
            match event {
                AgentEvent::ToolFinished { call, content, .. } => files.observe(call, content),
                AgentEvent::CompactionRecorded { record } => files.merge(&record.files),
                _ => {}
            }
        }
        files
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskSummary {
    pub goal: String,
    pub constraints: Vec<String>,
    pub decisions: Vec<String>,
    pub completed: Vec<String>,
    pub remaining: Vec<String>,
    pub verification: Vec<String>,
    pub uncertainty: Vec<String>,
}

impl TaskSummary {
    pub fn parse(text: &str) -> Result<Self, AgentError> {
        let text = text.trim();
        let text = text
            .strip_prefix("```json")
            .or_else(|| text.strip_prefix("```"))
            .and_then(|v| v.strip_suffix("```"))
            .unwrap_or(text)
            .trim();
        let summary: Self = serde_json::from_str(text).map_err(|_| AgentError::Compaction("summary must contain the required structured task fields; original context preserved".into()))?;
        if summary.goal.trim().is_empty() {
            return Err(AgentError::Compaction(
                "summary goal is empty; original context preserved".into(),
            ));
        }
        Ok(summary)
    }

    pub fn render(&self) -> String {
        let mut text = format!("目标\n{}", self.goal);
        for (title, items) in [
            ("约束", &self.constraints),
            ("决策", &self.decisions),
            ("已完成", &self.completed),
            ("待完成", &self.remaining),
            ("验证结果", &self.verification),
            ("不确定事项", &self.uncertainty),
        ] {
            text.push_str(&format!("\n\n{title}\n"));
            if items.is_empty() {
                text.push_str("未记录");
            } else {
                text.push_str(
                    &items
                        .iter()
                        .map(|item| format!("- {item}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
        }
        text
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionMode {
    Automatic,
    Manual,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStrategy {
    #[default]
    Summary,
    ToolPruning,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactionRecord {
    pub version: u32,
    pub mode: CompactionMode,
    /// Offsets are relative to this compaction's input context, not the full log.
    pub source_messages: usize,
    pub retained_from: usize,
    pub pinned_user: Option<usize>,
    pub before_tokens: usize,
    pub after_tokens: usize,
    #[serde(default)]
    pub strategy: CompactionStrategy,
    #[serde(default)]
    pub summary_requests: usize,
    #[serde(default)]
    pub summary_input_tokens: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<String>,
    /// Retained for backwards compatibility with historical records. The current
    /// engine preserves the original context on failure instead of generating
    /// deterministic local fallbacks.
    #[serde(default)]
    pub fallback: bool,
    /// Retained for backwards compatibility with older persisted records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    pub summary: TaskSummary,
    /// Cumulative, confirmed observations; not a claim about current file contents.
    pub files: FileActivity,
}
