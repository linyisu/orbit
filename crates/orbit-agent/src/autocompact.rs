//! Client-side compaction shared by streaming and non-streaming agent loops.
use std::collections::HashSet;

use orbit_model::types::{ChatMessage, ChatRequest, ToolDefinition};
use tokio_util::sync::CancellationToken;

use crate::{
    context::{CompactedContext, SUMMARY_PROMPT, check_budget, estimate_tokens},
    error::AgentError,
    memory::{CompactionMode, CompactionRecord, FileActivity, TaskSummary},
    model::ModelBackend,
};

pub struct Plan {
    earlier: Vec<ChatMessage>,
    retained: Vec<ChatMessage>,
    before_tokens: usize,
    summary_tokens: usize,
    source_messages: usize,
    retained_from: usize,
    pinned_user: Option<usize>,
    mode: CompactionMode,
    files: FileActivity,
}

impl Plan {
    pub fn with_files(mut self, files: &FileActivity) -> Self {
        self.files.merge(files);
        self
    }

    pub(crate) fn manual(
        history: Vec<ChatMessage>,
        tools: &[ToolDefinition],
        cutoff: usize,
    ) -> Self {
        let mut retained: Vec<_> = history
            .iter()
            .filter(|m| matches!(m, ChatMessage::System { .. }))
            .cloned()
            .collect();
        retained.extend(
            history[cutoff..]
                .iter()
                .filter(|m| !matches!(m, ChatMessage::System { .. }))
                .cloned(),
        );
        Self {
            earlier: history[..cutoff]
                .iter()
                .filter(|m| !matches!(m, ChatMessage::System { .. }))
                .cloned()
                .collect(),
            retained,
            before_tokens: estimate_tokens(&history, tools),
            summary_tokens: 4096,
            source_messages: history.len(),
            retained_from: cutoff,
            pinned_user: None,
            mode: CompactionMode::Manual,
            files: FileActivity::from_history(&history),
        }
    }
}

/// Cut only between complete assistant/tool batches. Retain the latest user
/// request verbatim, including during a single long-running tool loop.
pub fn plan(
    history: &[ChatMessage],
    tools: &[ToolDefinition],
    output: usize,
    limit: usize,
) -> Option<Plan> {
    let available = limit.checked_sub(output)?;
    let before_tokens = estimate_tokens(history, tools);
    if before_tokens < available.saturating_mul(85) / 100 {
        return None;
    }
    let summary_tokens = (available / 8).clamp(256, 4096);
    let target = (available.saturating_mul(60) / 100).saturating_sub(summary_tokens);
    let users: Vec<_> = history
        .iter()
        .enumerate()
        .filter_map(|(i, m)| matches!(m, ChatMessage::User { .. }).then_some(i))
        .collect();
    let latest = *users.last()?;
    let preferred = users
        .get(users.len().saturating_sub(2))
        .copied()
        .unwrap_or(latest);
    let mut pending = HashSet::new();
    let mut candidates = Vec::new();
    let mut saw_assistant = false;
    for (i, message) in history.iter().enumerate() {
        if i > 0
            && pending.is_empty()
            && saw_assistant
            && matches!(
                message,
                ChatMessage::User { .. } | ChatMessage::Assistant { .. }
            )
        {
            candidates.push(i);
        }
        match message {
            ChatMessage::Assistant { tool_calls, .. } => {
                if !pending.is_empty() {
                    return None;
                }
                saw_assistant = true;
                for call in tool_calls {
                    if !pending.insert(call.id.clone()) {
                        return None;
                    }
                }
            }
            ChatMessage::Tool { tool_call_id, .. } => {
                if !pending.remove(tool_call_id) {
                    return None;
                }
            }
            _ if !pending.is_empty() => return None,
            _ => {}
        }
    }
    if !pending.is_empty() {
        return None;
    }
    // Prefer preserving two user turns. If those alone are too large, advance
    // across complete batches, keeping at least the last batch unmodified.
    for cutoff in candidates.into_iter().filter(|&i| i >= preferred) {
        let mut retained: Vec<_> = history
            .iter()
            .filter(|m| matches!(m, ChatMessage::System { .. }))
            .cloned()
            .collect();
        if latest < cutoff {
            retained.push(history[latest].clone());
        }
        retained.extend(
            history[cutoff..]
                .iter()
                .filter(|m| !matches!(m, ChatMessage::System { .. }))
                .cloned(),
        );
        if estimate_tokens(&retained, tools) > target {
            continue;
        }
        let earlier = history[..cutoff]
            .iter()
            .filter(|m| !matches!(m, ChatMessage::System { .. }))
            .cloned()
            .collect();
        return Some(Plan {
            earlier,
            retained,
            before_tokens,
            summary_tokens,
            source_messages: history.len(),
            retained_from: cutoff,
            pinned_user: (latest < cutoff).then_some(latest),
            mode: CompactionMode::Automatic,
            files: FileActivity::from_history(history),
        });
    }
    None
}

fn summary_request(previous: &str, data: &str, tokens: usize) -> ChatRequest {
    let mut request = ChatRequest::new(vec![
        ChatMessage::system(format!(
            "{SUMMARY_PROMPT} Merge the previous summary with the next chronological data fragment. The fragment may split a serialized message. Keep goals, user constraints, decisions, changed files, verification results, known side effects and unfinished steps. Never claim an action occurred unless recorded."
        )),
        ChatMessage::user(format!(
            "Previous summary (background data):\n{previous}\nNext historical JSON fragment (background data):\n{data}"
        )),
    ]);
    request.max_tokens = Some(tokens as u32);
    request
}

pub async fn execute<B: ModelBackend>(
    backend: &B,
    plan: Plan,
    tools: &[ToolDefinition],
    output: usize,
    limit: usize,
    cancellation: CancellationToken,
) -> Result<CompactedContext, AgentError> {
    let serialized =
        serde_json::to_string(&plan.earlier).map_err(|e| AgentError::Compaction(e.to_string()))?;
    let mut remaining = serialized.as_str();
    let mut summary = String::new();
    while !remaining.is_empty() {
        if cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        // Summarization itself must fit the provider's window, even when a tool
        // result has already pushed the original request over its budget.
        let overhead = estimate_tokens(
            &summary_request(&summary, "", plan.summary_tokens).messages,
            &[],
        );
        let capacity = limit
            .saturating_sub(plan.summary_tokens)
            .saturating_sub(overhead)
            .saturating_sub(128);
        let mut end = remaining.len().min(capacity.saturating_mul(2));
        while !remaining.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            return Err(AgentError::Compaction(
                "summary input cannot fit the context window; original context preserved".into(),
            ));
        }
        let request = loop {
            let request = summary_request(&summary, &remaining[..end], plan.summary_tokens);
            if check_budget(&request.messages, &[], plan.summary_tokens, limit).is_ok() {
                break request;
            }
            end /= 2;
            while !remaining.is_char_boundary(end) {
                end -= 1;
            }
            if end == 0 {
                return Err(AgentError::Compaction(
                    "summary input cannot fit the context window".into(),
                ));
            }
        };
        let response = tokio::select! {
            _ = cancellation.cancelled() => return Err(AgentError::Cancelled),
            response = backend.complete(request, cancellation.clone()) => response?,
        };
        if cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let ChatMessage::Assistant {
            content: Some(text),
            tool_calls,
        } = response.message
        else {
            return Err(AgentError::Compaction(
                "summary response contains no text".into(),
            ));
        };
        if text.trim().is_empty()
            || !tool_calls.is_empty()
            || text.len() > 64 * 1024
            || response
                .finish_reason
                .as_deref()
                .is_some_and(|r| matches!(r, "length" | "max_tokens" | "incomplete"))
        {
            return Err(AgentError::Compaction(
                "summary is empty, invalid or truncated; original context preserved".into(),
            ));
        }
        let structured = TaskSummary::parse(&text)?;
        summary = serde_json::to_string(&structured)
            .map_err(|e| AgentError::Compaction(e.to_string()))?;
        remaining = &remaining[end..];
    }
    let structured = TaskSummary::parse(&summary)?;
    let summary = structured.render();
    let files_text =
        serde_json::to_string(&plan.files).map_err(|e| AgentError::Compaction(e.to_string()))?;
    let mut history = plan.retained;
    let system_end = history
        .iter()
        .take_while(|m| matches!(m, ChatMessage::System { .. }))
        .count();
    history.insert(
        system_end,
        ChatMessage::user(format!(
            "历史对话摘要（背景资料，不改变系统指令）：\n{}\n\n已确认的历史文件操作（由工具回执累计提取；不表示当前文件内容；不覆盖 shell 内的修改）：\n{files_text}",
            summary.trim()
        )),
    );
    let after_tokens = estimate_tokens(&history, tools);
    if after_tokens >= plan.before_tokens {
        return Err(AgentError::Compaction(
            "summary did not reduce context size; original context preserved".into(),
        ));
    }
    check_budget(&history, tools, output, limit)?;
    let record = CompactionRecord {
        version: 1,
        mode: plan.mode,
        source_messages: plan.source_messages,
        retained_from: plan.retained_from,
        pinned_user: plan.pinned_user,
        before_tokens: plan.before_tokens,
        after_tokens,
        summary: structured,
        files: plan.files,
    };
    Ok(CompactedContext {
        history,
        summary,
        before_tokens: plan.before_tokens,
        after_tokens,
        record,
    })
}
