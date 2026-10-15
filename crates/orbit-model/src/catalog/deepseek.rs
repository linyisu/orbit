//! Official capability defaults supplement metadata; they never grant access.
use super::ReasoningProfile;
use crate::{
    client::ApiFormat,
    types::{ReasoningAdapter, ReasoningEffort},
};
/// Official capability metadata absent from the provider's ID-only model list.
/// https://api-docs.deepseek.com/guides/vision/ (checked 2026-09-30).
pub fn default_vision(format: ApiFormat, base: &str, id: &str) -> bool {
    matches!(
        format,
        ApiFormat::OpenaiChatCompletions | ApiFormat::OpenaiResponses
    ) && matches!(
        base.trim_end_matches('/'),
        "https://api.deepseek.com" | "https://api.deepseek.com/v1"
    ) && matches!(
        id,
        "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4-flash-vision-exp"
    )
}

/// Defaults for official models whose discovery response contains only IDs.
/// https://api-docs.deepseek.com/api/create-chat-completion/ (checked 2026-10-03).
pub fn default_reasoning(format: ApiFormat, base: &str, id: &str) -> Option<ReasoningProfile> {
    if !matches!(
        base.trim_end_matches('/'),
        "https://api.deepseek.com" | "https://api.deepseek.com/v1"
    ) || !matches!(id, "deepseek-flash" | "deepseek-v4-pro")
    {
        return None;
    }

    let adapter = match format {
        ApiFormat::OpenaiChatCompletions => ReasoningAdapter::OpenaiChat,
        ApiFormat::OpenaiResponses => ReasoningAdapter::OpenaiResponses,
        _ => return None,
    };
    Some(ReasoningProfile {
        adapter,
        efforts: vec![
            ReasoningEffort::None,
            ReasoningEffort::Low,
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ],
        default: ReasoningEffort::High,
        extra: toml::Table::new(),
    })
}
