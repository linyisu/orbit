use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
};

use futures_util::StreamExt;
use orbit_model::types::{
    ChatCompletion, ChatMessage, ChatRequest, FunctionCall, StreamEvent, ToolCall, ToolCallDelta,
    Usage,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{error::AgentError, events::AgentEvent, model::ModelBackend, tool::ToolRegistry};

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub max_turns: usize,
    pub max_tool_calls: usize,
    pub context_tokens: usize,
    pub auto_compact: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            // Large enough for sustained repository work, while still placing
            // a firm bound on a malformed model/tool loop.
            max_turns: 75,
            max_tool_calls: 500,
            context_tokens: 128_000,
            auto_compact: true,
        }
    }
}

#[derive(Debug)]
pub struct AgentResult {
    pub final_message: ChatMessage,
    pub history: Vec<ChatMessage>,
    pub turns: usize,
    pub tool_calls: usize,
}

#[derive(Default)]
struct ContextThresholds {
    prune_after: usize,
    compact_after: usize,
}

pub struct Agent<B> {
    backend: B,
    tools: ToolRegistry,
    config: AgentConfig,
    files: crate::memory::FileActivity,
    archive: Option<std::sync::Arc<dyn crate::archive::ContextArchive>>,
    calibration: crate::calibration::TokenCalibration,
    observation_profile: String,
    compaction: crate::compaction::CompactionRuntime,
    journal: Option<std::sync::Arc<dyn crate::checkpoint::ExecutionJournal>>,
}

impl<B> Agent<B>
where
    B: ModelBackend,
{
    pub fn new(backend: B, mut tools: ToolRegistry, config: AgentConfig) -> Self {
        if !backend.supports_vision() {
            tools.remove("read_image");
        }

        Self {
            backend,
            tools,
            config,
            files: Default::default(),
            archive: None,
            journal: None,
            calibration: Default::default(),
            observation_profile: String::new(),
            compaction: Default::default(),
        }
    }

    pub fn with_compaction(mut self, runtime: crate::compaction::CompactionRuntime) -> Self {
        self.compaction = runtime;
        self
    }

    pub fn with_calibration(
        mut self,
        profile: String,
        calibration: crate::calibration::TokenCalibration,
    ) -> Self {
        self.observation_profile = profile;
        self.calibration = calibration;
        self
    }

    pub fn with_file_activity(mut self, files: crate::memory::FileActivity) -> Self {
        self.files = files;
        self
    }

    pub fn with_execution_journal(
        mut self,
        journal: std::sync::Arc<dyn crate::checkpoint::ExecutionJournal>,
    ) -> Self {
        self.journal = Some(journal);
        self
    }

    async fn checkpoint(
        &self,
        history: &[ChatMessage],
        turns: usize,
        tool_calls: usize,
        in_flight: Option<String>,
        attachments: &[orbit_model::types::Attachment],
        event: Option<AgentEvent>,
    ) -> Result<(), AgentError> {
        if let Some(journal) = &self.journal {
            journal
                .save(
                    crate::checkpoint::ExecutionCheckpoint {
                        history: history.to_vec(),
                        turns,
                        tool_calls,
                        in_flight,
                        attachments: attachments.to_vec(),
                    },
                    event,
                )
                .await?;
        }

        Ok(())
    }

    pub fn with_context_archive(
        mut self,
        archive: std::sync::Arc<dyn crate::archive::ContextArchive>,
    ) -> Self {
        self.archive = Some(archive);
        self
    }

    /// Run the one Agent execution loop while delivering every lifecycle event
    /// to an adapter. `run`, `run_with_events`, and the TUI channel API all
    /// delegate here so they cannot develop different tool or recovery rules.
    pub async fn run(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> Result<AgentResult, AgentError> {
        self.run_core(request, cancellation, |_| {
            std::future::ready(Ok::<(), AgentError>(()))
        })
        .await
    }

    /// Compatibility adapter for consumers interested only in raw model
    /// streaming events. Agent lifecycle events still drive the shared core.
    pub async fn run_with_events<F>(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
        on_event: F,
    ) -> Result<AgentResult, AgentError>
    where
        F: Fn(StreamEvent) + Send + Sync,
    {
        self.run_core(request, cancellation, |event| {
            if let AgentEvent::Model(event) = event {
                on_event(event);
            }

            std::future::ready(Ok::<(), AgentError>(()))
        })
        .await
    }

    /// Run the Agent with the canonical lifecycle event stream used by the TUI
    /// and CLI. A closed receiver aborts with `EventChannelClosed`, preserving
    /// the previous channel contract.
    pub async fn run_with_agent_events(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
        sender: mpsc::Sender<AgentEvent>,
    ) -> Result<AgentResult, AgentError> {
        self.run_core(request, cancellation, move |event| {
            let sender = sender.clone();
            async move { send_agent_event(&sender, event).await }
        })
        .await
    }

    async fn run_core<F, Fut>(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
        mut emit: F,
    ) -> Result<AgentResult, AgentError>
    where
        F: FnMut(AgentEvent) -> Fut,
        Fut: Future<Output = Result<(), AgentError>>,
    {
        let mut pending_events = 0usize;
        let mut flushed_at = std::time::Instant::now();
        let mut journaled_emit = |event: AgentEvent| {
            let journal = self.journal.clone();

            if let Some(journal) = &journal {
                journal.record(event.clone());
            }

            pending_events += 1;
            let flush = pending_events >= 256
                || flushed_at.elapsed() >= std::time::Duration::from_millis(250);

            if flush {
                pending_events = 0;
                flushed_at = std::time::Instant::now();
            }

            let emitted = emit(event);

            async move {
                if flush && let Some(journal) = journal {
                    journal.flush().await?;
                }

                emitted.await
            }
        };
        let result = self
            .execute_core(request, cancellation.clone(), &mut journaled_emit)
            .await;
        let terminal = match &result {
            Ok(agent_result) => AgentEvent::Completed {
                turns: agent_result.turns,
                tool_calls: agent_result.tool_calls,
            },
            Err(AgentError::Cancelled) => AgentEvent::Cancelled,
            Err(error) => AgentEvent::Failed {
                error: error.to_string(),
            },
        };

        let emitted = journaled_emit(terminal).await;

        if let Some(journal) = &self.journal {
            journal.flush().await?;
        }

        emitted?;

        result
    }

    async fn execute_core<F, Fut>(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
        emit: &mut F,
    ) -> Result<AgentResult, AgentError>
    where
        F: FnMut(AgentEvent) -> Fut,
        Fut: Future<Output = Result<(), AgentError>>,
    {
        if request.messages.is_empty() {
            return Err(AgentError::InvalidInitialRequest);
        }

        let mut history = request.messages;

        // The host builds a request from its previous projection plus the new
        // user message. Instructions injected by the host are request context,
        // not canonical conversation messages.
        if let Some(journal) = &self.journal
            && let Some(message @ ChatMessage::User { .. }) = history.last()
        {
            journal.record_message(message.clone());
        }

        let mut files = self.files.clone();
        files.merge(&crate::memory::FileActivity::from_history(&history));
        let mut turns = 0;
        let mut tool_calls = 0;
        let mut context_thresholds = ContextThresholds::default();
        let mut calibration = self.calibration.clone();
        let mut recovery_limit = None;
        let mut force_compaction = false;
        self.checkpoint(&history, turns, tool_calls, None, &[], None)
            .await?;

        loop {
            if cancellation.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            if self.config.max_turns > 0 && turns >= self.config.max_turns {
                return Err(AgentError::MaxTurns);
            }

            let output = request.max_tokens.unwrap_or(4096) as usize;
            let local_limit = recovery_limit
                .unwrap_or(usize::MAX)
                .min(calibration.local_limit(self.config.context_tokens, output));
            let prepared = self
                .prepare_context(
                    &mut history,
                    (output, local_limit, force_compaction),
                    &mut context_thresholds,
                    &files,
                    cancellation.clone(),
                    emit,
                )
                .await
                .map_err(|error| match error {
                    AgentError::ContextBudget { estimated, .. } => AgentError::ContextBudget {
                        estimated: calibration
                            .estimate(estimated.saturating_sub(output))
                            .saturating_add(output),
                        limit: self.config.context_tokens,
                    },
                    other => other,
                });

            // A successful compaction may be followed immediately by cancellation
            // or a closed UI channel. Persist its effective history together with
            // the queued compaction events before returning that terminal error.
            if prepared.is_err() {
                self.checkpoint(&history, turns, tool_calls, None, &[], None)
                    .await?;
            }

            prepared?;
            force_compaction = false;
            turns += 1;
            self.checkpoint(&history, turns, tool_calls, None, &[], None)
                .await?;
            emit(AgentEvent::TurnStarted { turn: turns }).await?;

            let model_request = ChatRequest {
                messages: model_messages(&history, self.backend.supports_vision()),
                tools: self.tools.definitions(),
                temperature: request.temperature,
                max_tokens: request.max_tokens,
                reasoning_effort: request.reasoning_effort,
            };
            let estimated_input =
                crate::context::estimate_tokens(&model_request.messages, &model_request.tools);
            emit(AgentEvent::ContextUsage { estimated_input }).await?;
            let started = std::time::Instant::now();
            let response = self
                .collect_stream(model_request, cancellation.clone(), emit)
                .await;
            let usage = response.as_ref().ok().and_then(|(c, _)| c.usage.clone());
            calibration.observe(
                estimated_input,
                usage.as_ref().and_then(|u| u.prompt_tokens),
            );
            emit(AgentEvent::RequestMetrics {
                profile: self.observation_profile.clone(),
                retries: response.as_ref().ok().and_then(|(_, retries)| *retries),
                estimated_input,
                usage,
                elapsed_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            })
            .await?;

            if self.config.auto_compact
                && recovery_limit.is_none()
                && matches!(&response, Err(AgentError::Model(error)) if error.is_context_limit())
            {
                // No tool has started for this rejected request. Retry this
                // request once after a verified reduction, not the whole run.
                recovery_limit = Some(
                    local_limit
                        .saturating_sub(output)
                        .min(estimated_input.saturating_mul(3) / 4)
                        .saturating_add(output),
                );
                force_compaction = true;
                turns = turns.saturating_sub(1);
                continue;
            }

            let (completion, _) = response?;
            check_finish_reason(completion.finish_reason.as_deref())?;
            let assistant = completion.message.clone();
            let calls = match &assistant {
                ChatMessage::Assistant { tool_calls, .. } => tool_calls.clone(),
                _ => Vec::new(),
            };

            validate_tool_ids(&calls)?;

            if self.config.max_tool_calls > 0
                && tool_calls.saturating_add(calls.len()) > self.config.max_tool_calls
            {
                return Err(AgentError::MaxToolCalls);
            }

            history.push(assistant.clone());

            if let Some(journal) = &self.journal {
                journal.record_message(assistant);
            }

            self.checkpoint(&history, turns, tool_calls, None, &[], None)
                .await?;

            if calls.is_empty() {
                return Ok(AgentResult {
                    final_message: completion.message,
                    history,
                    turns,
                    tool_calls,
                });
            }

            let mut loaded_attachments = Vec::new();

            for call in calls {
                if cancellation.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }

                tool_calls += 1;
                let event = AgentEvent::ToolStarted { call: call.clone() };
                self.checkpoint(
                    &history,
                    turns,
                    tool_calls,
                    Some(call.id.clone()),
                    &loaded_attachments,
                    Some(event.clone()),
                )
                .await?;
                emit(event).await?;

                if call.function.name == "read_image" && !self.backend.supports_vision() {
                    let message = "current model does not support image input".to_string();
                    let event = AgentEvent::ToolFailed {
                        call: call.clone(),
                        error: message.clone(),
                    };
                    let result = ChatMessage::tool(call.id, format!("tool error: {message}"));
                    history.push(result.clone());

                    if let Some(journal) = &self.journal {
                        journal.record_message(result);
                    }

                    self.checkpoint(
                        &history,
                        turns,
                        tool_calls,
                        None,
                        &loaded_attachments,
                        Some(event.clone()),
                    )
                    .await?;
                    emit(event).await?;
                    continue;
                }

                match self.tools.execute(call.clone(), cancellation.clone()).await {
                    Ok(output) => {
                        files.observe(&call, &output.content);

                        if let Some(evidence) = output.verification {
                            emit(AgentEvent::VerificationRecorded {
                                call_id: call.id.clone(),
                                evidence,
                            })
                            .await?;
                        }

                        let event = AgentEvent::ToolFinished {
                            call: call.clone(),
                            content: output.content.clone(),
                            changes: output.changes.clone(),
                        };
                        let result = ChatMessage::tool(call.id, output.content);
                        history.push(result.clone());

                        if let Some(journal) = &self.journal {
                            journal.record_message(result);
                        }

                        loaded_attachments.extend(output.attachments);
                        self.checkpoint(
                            &history,
                            turns,
                            tool_calls,
                            None,
                            &loaded_attachments,
                            Some(event.clone()),
                        )
                        .await?;
                        emit(event).await?;
                    }

                    Err(error) => {
                        let message = error.to_string();
                        let event = AgentEvent::ToolFailed {
                            call: call.clone(),
                            error: message.clone(),
                        };
                        let result = ChatMessage::tool(call.id, format!("tool error: {message}"));
                        history.push(result.clone());

                        if let Some(journal) = &self.journal {
                            journal.record_message(result);
                        }

                        self.checkpoint(
                            &history,
                            turns,
                            tool_calls,
                            None,
                            &loaded_attachments,
                            Some(event.clone()),
                        )
                        .await?;
                        emit(event).await?;
                    }
                }
            }

            let before_attachments = history.len();
            append_loaded_attachments(&mut history, loaded_attachments);

            if let Some(journal) = &self.journal {
                for message in &history[before_attachments..] {
                    journal.record_message(message.clone());
                }
            }

            self.checkpoint(&history, turns, tool_calls, None, &[], None)
                .await?;
        }
    }

    async fn prepare_context<F, Fut>(
        &self,
        history: &mut Vec<ChatMessage>,
        (output, local_limit, force): (usize, usize, bool),
        thresholds: &mut ContextThresholds,
        files: &crate::memory::FileActivity,
        cancellation: CancellationToken,
        emit: &mut F,
    ) -> Result<(), AgentError>
    where
        F: FnMut(AgentEvent) -> Fut,
        Fut: Future<Output = Result<(), AgentError>>,
    {
        let tools = self.tools.definitions();
        let mut estimated = crate::context::estimate_tokens(history, &tools);

        if !force
            && self.config.auto_compact
            && estimated >= thresholds.prune_after
            && let Some(archive) = self.archive.as_deref()
            && let Some(plan) = crate::autocompact::plan_pruning(
                history,
                &tools,
                output,
                local_limit,
                &self.compaction.settings,
            )
        {
            match crate::autocompact::execute_pruning_configured(
                plan,
                &tools,
                output,
                local_limit,
                cancellation.clone(),
                archive,
            )
            .await
            {
                Ok(Some(result)) => {
                    emit(AgentEvent::CompactionRecorded {
                        record: result.record,
                    })
                    .await?;
                    *history = result.history;
                    estimated = result.after_tokens;
                    thresholds.prune_after = estimated.saturating_add(local_limit / 10);
                    emit(AgentEvent::ContextCompacted {
                        strategy: crate::memory::CompactionStrategy::ToolPruning,
                        before_tokens: result.before_tokens,
                        after_tokens: result.after_tokens,
                        fallback: false,
                        fallback_reason: None,
                    })
                    .await?;
                }
                Ok(None) => thresholds.prune_after = estimated.saturating_add(local_limit / 10),
                Err(AgentError::Cancelled) => return Err(AgentError::Cancelled),
                Err(error) => {
                    thresholds.prune_after = estimated.saturating_add(local_limit / 10);
                    emit(AgentEvent::CompactionFailed {
                        kind: error.compaction_kind(),
                        error: error.to_string(),
                    })
                    .await?;
                }
            }
        }

        if force && self.archive.is_none() {
            return Err(AgentError::CompactionFailed {
                kind: crate::compaction::CompactionFailureKind::Archive,
                message: "context archive is required before compacting history".into(),
            });
        }

        let plan = if force {
            Some(crate::autocompact::plan_configured(history, &tools, output, local_limit, crate::memory::CompactionMode::Manual, &self.compaction.settings)
                .ok_or_else(|| AgentError::CompactionFailed {kind:crate::compaction::CompactionFailureKind::InputBudget,
                    message:"no safe compaction boundary fits the rejected request; original context preserved".into()})?)
        } else if self.config.auto_compact
            && self.archive.is_some()
            && estimated >= thresholds.compact_after
        {
            crate::autocompact::plan_configured(
                history,
                &tools,
                output,
                local_limit,
                crate::memory::CompactionMode::Automatic,
                &self.compaction.settings,
            )
        } else {
            None
        };

        if let Some(plan) = plan {
            emit(AgentEvent::CompactionStarted).await?;
            let runtime = self
                .compaction
                .for_input_window(local_limit.saturating_sub(output));
            let event = match crate::autocompact::execute_configured(
                runtime.backend.as_deref().unwrap_or(&self.backend),
                plan.with_files(files),
                &tools,
                output,
                local_limit,
                cancellation.clone(),
                self.archive.as_deref(),
                &runtime.settings,
            )
            .await
            {
                Ok(result) => {
                    let fallback = result.record.fallback;
                    let fallback_reason = result.record.fallback_reason.clone();
                    let strategy = result.record.strategy.clone();
                    emit(AgentEvent::CompactionRecorded {
                        record: result.record,
                    })
                    .await?;
                    let event = AgentEvent::ContextCompacted {
                        strategy,
                        before_tokens: result.before_tokens,
                        after_tokens: result.after_tokens,
                        fallback,
                        fallback_reason,
                    };
                    *history = result.history;
                    thresholds.compact_after = result
                        .after_tokens
                        .saturating_add(local_limit.saturating_sub(output) / 10);

                    event
                }

                Err(AgentError::Cancelled) => return Err(AgentError::Cancelled),

                Err(error) => {
                    // A failed early attempt must not cause a summary request on
                    // every tool call. Retry only after substantial input growth.
                    thresholds.compact_after =
                        estimated.saturating_add(local_limit.saturating_sub(output) / 10);
                    let event = AgentEvent::CompactionFailed {
                        kind: error.compaction_kind(),
                        error: error.to_string(),
                    };

                    if force {
                        emit(event).await?;
                        return Err(error);
                    }

                    event
                }
            };

            emit(event).await?;
        }

        if cancellation.is_cancelled() {
            return Err(AgentError::Cancelled);
        }

        crate::context::check_budget(history, &tools, output, local_limit)
    }

    async fn collect_stream<F, Fut>(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
        emit: &mut F,
    ) -> Result<(ChatCompletion, Option<u32>), AgentError>
    where
        F: FnMut(AgentEvent) -> Fut,
        Fut: Future<Output = Result<(), AgentError>>,
    {
        let fallback_request = request.clone();
        let mut reasoning_start = std::time::Instant::now();
        let mut reasoning_active = false;
        let (mut stream, retries) = self
            .backend
            .stream_with_retries(request, cancellation.clone())
            .await?;
        let mut text = String::new();
        let mut calls = BTreeMap::new();
        let mut finish_reason = None;
        let mut usage = None;

        while let Some(event) = stream.next().await {
            let event = event.map_err(AgentError::Model)?;
            match &event {
                StreamEvent::ReasoningDelta(text) if !text.is_empty() => {
                    if !reasoning_active {
                        reasoning_start = std::time::Instant::now();
                        reasoning_active = true;
                    }
                }
                StreamEvent::ReasoningDelta(_) => {}
                StreamEvent::TextDelta(text) if text.is_empty() => {}
                _ => {
                    if reasoning_active {
                        emit(AgentEvent::ReasoningFinished {
                            elapsed_ms: reasoning_start
                                .elapsed()
                                .as_millis()
                                .min(u128::from(u64::MAX))
                                as u64,
                        })
                        .await?;
                        reasoning_active = false;
                    }
                    reasoning_start = std::time::Instant::now();
                }
            }
            emit(AgentEvent::Model(event.clone())).await?;

            match event {
                StreamEvent::TextDelta(delta) => text.push_str(&delta),
                StreamEvent::ReasoningDelta(_) => {}
                StreamEvent::ToolCallDelta(delta) => merge_tool_call(&mut calls, delta),
                StreamEvent::Completed {
                    finish_reason: reason,
                    usage: event_usage,
                } => {
                    // OpenAI-compatible streams commonly send a final usage-only
                    // chunk after the choice carrying `finish_reason`. Keep the
                    // terminal reason while accepting the later usage payload.
                    if reason.is_some() {
                        finish_reason = reason;
                    }

                    merge_usage(&mut usage, event_usage);
                }
            }
        }

        if text.is_empty() && calls.is_empty() && finish_reason.is_none() {
            let completion = self
                .backend
                .complete(fallback_request, cancellation)
                .await?;

            if let ChatMessage::Assistant {
                content,
                tool_calls,
            } = &completion.message
            {
                if let Some(content) = content {
                    emit(AgentEvent::Model(StreamEvent::TextDelta(content.clone()))).await?;
                }

                for (index, call) in tool_calls.iter().enumerate() {
                    emit(AgentEvent::Model(StreamEvent::ToolCallDelta(
                        ToolCallDelta {
                            metadata: call.metadata.clone(),
                            index,
                            id: Some(call.id.clone()),
                            name: Some(call.function.name.clone()),
                            arguments: call.function.arguments.clone(),
                        },
                    )))
                    .await?;
                }

                emit(AgentEvent::Model(StreamEvent::Completed {
                    finish_reason: completion.finish_reason.clone(),
                    usage: completion.usage.clone(),
                }))
                .await?;
            }

            return Ok((completion, None));
        }

        Ok((
            ChatCompletion {
                id: None,
                model: None,
                message: ChatMessage::Assistant {
                    content: (!text.is_empty()).then_some(text),
                    tool_calls: calls.into_values().collect(),
                },
                finish_reason,
                usage,
            },
            retries,
        ))
    }
}

/// Keep persisted history lossless, but bound binary payloads assembled for one
/// provider request. Older images remain represented by their name in text.
/// If the model does not support native vision input, all image attachments are
/// omitted from the request payload and represented as text notes.
pub fn model_messages(history: &[ChatMessage], supports_vision: bool) -> Vec<ChatMessage> {
    if !supports_vision {
        let mut messages = history.to_vec();

        for message in &mut messages {
            let ChatMessage::User {
                content,
                attachments,
            } = message
            else {
                continue;
            };
            if attachments.is_empty() {
                continue;
            }

            let omitted = attachments
                .iter()
                .map(|attachment| attachment.name())
                .collect::<Vec<_>>()
                .join(", ");
            content.push_str(&format!(
                "\n\n[Image omitted: current model does not support image input: {omitted}]"
            ));
            attachments.clear();
        }

        return messages;
    }

    const MAX_IMAGES: usize = 4;

    let mut remaining = MAX_IMAGES;
    let mut messages = history.to_vec();

    for message in messages.iter_mut().rev() {
        let ChatMessage::User {
            content,
            attachments,
        } = message
        else {
            continue;
        };
        if attachments.len() <= remaining {
            remaining -= attachments.len();
            continue;
        }

        let split = attachments.len() - remaining;
        let omitted = attachments[..split]
            .iter()
            .map(|attachment| attachment.name())
            .collect::<Vec<_>>()
            .join(", ");
        content.push_str(&format!(
            "\n\n[Earlier image attachment omitted from this request to limit context size: {omitted}]"
        ));
        attachments.drain(..split);
        remaining = 0;
    }

    messages
}

fn append_loaded_attachments(
    history: &mut Vec<ChatMessage>,
    attachments: Vec<orbit_model::types::Attachment>,
) {
    if attachments.is_empty() {
        return;
    }

    let names = attachments
        .iter()
        .map(|attachment| attachment.name())
        .collect::<Vec<_>>()
        .join(", ");
    history.push(ChatMessage::user_with_attachments(
        format!(
            "The read_image tool loaded the following local image into this model request: {names}. Analyze the actual image content directly."
        ),
        attachments,
    ));
}

fn check_finish_reason(reason: Option<&str>) -> Result<(), AgentError> {
    if reason.is_some_and(|r| matches!(r, "max_tokens" | "length" | "incomplete")) {
        return Err(AgentError::Model(orbit_model::error::ModelError::Protocol(
            format!(
                "模型输出达到上限或未完成（stop_reason={}）。思考也可能占用输出预算；请提高 config.toml 的 max_output_tokens。未执行本条响应中的工具，也未自动重试。",
                reason.unwrap()
            ),
        )));
    }

    Ok(())
}

fn merge_usage(current: &mut Option<Usage>, update: Option<Usage>) {
    let Some(update) = update else {
        return;
    };

    if let Some(current) = current {
        current.prompt_tokens = update.prompt_tokens.or(current.prompt_tokens);
        current.completion_tokens = update.completion_tokens.or(current.completion_tokens);
        current.total_tokens = update.total_tokens.or(current.total_tokens);
    } else {
        *current = Some(update);
    }
}

fn validate_tool_ids(calls: &[ToolCall]) -> Result<(), AgentError> {
    let mut ids = HashSet::new();

    for call in calls {
        if call.id.trim().is_empty() || !ids.insert(&call.id) {
            return Err(AgentError::Model(orbit_model::error::ModelError::Protocol(
                "model returned empty or duplicate tool call IDs; no tools in this response were executed".into(),
            )));
        }
    }

    Ok(())
}

fn merge_tool_call(calls: &mut BTreeMap<usize, ToolCall>, delta: ToolCallDelta) {
    // Providers index all output blocks, including text and reasoning. Only
    // materialize actual tool blocks; never pad sparse indices with fake calls.
    let call = calls.entry(delta.index).or_insert_with(|| ToolCall {
        metadata: None,
        id: String::new(),
        kind: "function".to_owned(),
        function: FunctionCall {
            name: String::new(),
            arguments: String::new(),
        },
    });

    if let Some(id) = delta.id {
        call.id = id;
    }

    if let Some(name) = delta.name {
        call.function.name.push_str(&name);
    }

    if let Some(metadata) = delta.metadata {
        call.metadata = Some(metadata);
    }

    call.function.arguments.push_str(&delta.arguments);
}

async fn send_agent_event(
    sender: &mpsc::Sender<AgentEvent>,
    event: AgentEvent,
) -> Result<(), AgentError> {
    sender
        .send(event)
        .await
        .map_err(|_| AgentError::EventChannelClosed)
}

impl From<ChatCompletion> for AgentResult {
    fn from(completion: ChatCompletion) -> Self {
        Self {
            final_message: completion.message.clone(),
            history: vec![completion.message],
            turns: 1,
            tool_calls: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::MockModel,
        tool::{ClosureTool, Tool, ToolError, ToolFuture, ToolOutput},
    };
    use orbit_model::types::{Attachment, FunctionCall, ToolCall, Usage};
    use serde_json::json;

    fn assistant_text(text: &str) -> ChatCompletion {
        ChatCompletion {
            id: None,
            model: Some("mock".to_owned()),
            message: ChatMessage::assistant(text),
            finish_reason: Some("stop".to_owned()),
            usage: None,
        }
    }

    #[tokio::test]
    async fn calibrated_budget_stops_request_and_reports_real_configured_limit() {
        let backend = MockModel::new(vec![]);
        let mut calibration = crate::calibration::TokenCalibration::default();
        calibration.observe(1, Some(10000));
        let agent = Agent::new(
            backend.clone(),
            ToolRegistry::new(),
            AgentConfig {
                context_tokens: 8192,
                auto_compact: false,
                ..Default::default()
            },
        )
        .with_calibration("provider::model".into(), calibration);
        let result = agent.run(request(), CancellationToken::new()).await;

        assert!(
            matches!(result,Err(AgentError::ContextBudget {estimated,limit:8192}) if estimated>8192)
        );
        assert!(backend.requests().is_empty());
    }

    struct FailingJournal {
        checkpoints: std::sync::Mutex<Vec<crate::checkpoint::ExecutionCheckpoint>>,
        fail_start: bool,
        fail_receipt: bool,
    }

    #[tokio::test]
    async fn compaction_without_an_archive_keeps_the_original_history() {
        let agent = Agent::new(
            MockModel::new(vec![]),
            ToolRegistry::new(),
            AgentConfig {
                context_tokens: 8192,
                ..Default::default()
            },
        );
        let original = vec![
            ChatMessage::system("rules"),
            ChatMessage::user("preserve this request"),
            ChatMessage::assistant("previous work"),
        ];
        let mut automatic = original.clone();
        let mut thresholds = ContextThresholds {
            prune_after: 0,
            compact_after: 0,
        };

        agent
            .prepare_context(
                &mut automatic,
                (128, 8192, false),
                &mut thresholds,
                &Default::default(),
                CancellationToken::new(),
                &mut |_| std::future::ready(Ok(())),
            )
            .await
            .unwrap();

        let mut manual = original.clone();
        let error = agent
            .prepare_context(
                &mut manual,
                (128, 8192, true),
                &mut thresholds,
                &Default::default(),
                CancellationToken::new(),
                &mut |_| std::future::ready(Ok(())),
            )
            .await
            .unwrap_err();

        assert_eq!(automatic, original);
        assert_eq!(manual, original);
        assert!(matches!(
            error,
            AgentError::CompactionFailed {
                kind: crate::compaction::CompactionFailureKind::Archive,
                ..
            }
        ));
    }
    impl crate::checkpoint::ExecutionJournal for FailingJournal {
        fn record(&self, _: AgentEvent) {}

        fn flush(&self) -> crate::checkpoint::CheckpointFuture<'_> {
            Box::pin(async { Ok(()) })
        }

        fn save<'a>(
            &'a self,
            checkpoint: crate::checkpoint::ExecutionCheckpoint,
            event: Option<AgentEvent>,
        ) -> crate::checkpoint::CheckpointFuture<'a> {
            Box::pin(async move {
                if (self.fail_start && matches!(event, Some(AgentEvent::ToolStarted { .. })))
                    || (self.fail_receipt && matches!(event, Some(AgentEvent::ToolFinished { .. })))
                {
                    Err(AgentError::Checkpoint("disk unavailable".into()))
                } else {
                    self.checkpoints.lock().unwrap().push(checkpoint);
                    Ok(())
                }
            })
        }
    }

    #[tokio::test]
    async fn cancellation_after_compaction_saves_the_published_context() {
        use std::sync::Arc;

        struct TestArchive;

        impl crate::archive::ContextArchive for TestArchive {
            fn save<'a>(
                &'a self,
                _messages: &'a [ChatMessage],
            ) -> crate::archive::ArchiveFuture<'a> {
                Box::pin(async { Ok("archive://test".into()) })
            }
        }

        let response = assistant_text(
            &serde_json::json!({
                "goal": "fix parser",
                "constraints": ["preserve API"],
                "decisions": [],
                "completed": [],
                "remaining": ["test empty input"],
                "verification": [],
                "uncertainty": []
            })
            .to_string(),
        );
        let history = vec![
            ChatMessage::system("coding rules"),
            ChatMessage::user("fix parser, preserve API"),
            ChatMessage::assistant("investigation ".repeat(4000)),
            ChatMessage::user("continue"),
        ];
        let journal = Arc::new(FailingJournal {
            fail_start: false,
            fail_receipt: false,
            checkpoints: Default::default(),
        });
        let agent = Agent::new(
            MockModel::new(vec![response; 16]),
            ToolRegistry::new(),
            AgentConfig {
                context_tokens: 16000,
                ..Default::default()
            },
        )
        .with_execution_journal(journal.clone())
        .with_context_archive(Arc::new(TestArchive));
        let token = CancellationToken::new();
        let result = agent
            .run_core(ChatRequest::new(history.clone()), token.clone(), |event| {
                if matches!(event, AgentEvent::ContextCompacted { .. }) {
                    token.cancel();
                }

                std::future::ready(Ok(()))
            })
            .await;

        assert!(matches!(result, Err(AgentError::Cancelled)));

        let checkpoints = journal.checkpoints.lock().unwrap();
        let restored = &checkpoints.last().unwrap().history;

        assert!(
            crate::context::estimate_tokens(restored, &[])
                < crate::context::estimate_tokens(&history, &[]) / 2
        );
        assert!(
            serde_json::to_string(restored)
                .unwrap()
                .contains("preserve API")
        );
    }

    #[tokio::test]
    async fn durability_failure_stops_before_next_side_effect_on_all_adapters() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        for fail_start in [true, false] {
            for adapter in 0..3 {
                let count = Arc::new(AtomicUsize::new(0));
                let effects = count.clone();
                let mut tools = ToolRegistry::new();
                tools.register(ClosureTool::new("write", "", json!({}), move |_, _| {
                    effects.fetch_add(1, Ordering::SeqCst);
                    async { Ok("saved".into()) }
                }));
                let mut completion = assistant_tool_call("a", "write", "{}");
                completion.message = ChatMessage::assistant_tool_calls(vec![
                    ToolCall::function("a", "write", json!({})),
                    ToolCall::function("b", "write", json!({})),
                ]);
                let agent = Agent::new(
                    MockModel::new(vec![completion]),
                    tools,
                    AgentConfig::default(),
                )
                .with_execution_journal(Arc::new(FailingJournal {
                    fail_start,
                    fail_receipt: !fail_start,
                    checkpoints: Default::default(),
                }));
                let result = match adapter {
                    0 => agent.run(request(), CancellationToken::new()).await,
                    1 => {
                        agent
                            .run_with_events(request(), CancellationToken::new(), |_| {})
                            .await
                    }
                    _ => {
                        let (tx, _rx) = mpsc::channel(128);
                        agent
                            .run_with_agent_events(request(), CancellationToken::new(), tx)
                            .await
                    }
                };

                assert!(matches!(result, Err(AgentError::Checkpoint(_))));
                assert_eq!(count.load(Ordering::SeqCst), usize::from(!fail_start));
            }
        }
    }

    #[test]
    fn provider_payload_keeps_only_the_four_most_recent_images() {
        let history = (0..6)
            .map(|index| {
                ChatMessage::user_with_attachments(
                    format!("prompt {index}"),
                    vec![Attachment::Image {
                        name: format!("{index}.png"),
                        mime_type: "image/png".into(),
                        path: format!("/stored/{index}.png").into(),
                    }],
                )
            })
            .collect::<Vec<_>>();
        let payload = model_messages(&history, true);
        let image_count = payload
            .iter()
            .map(|message| match message {
                ChatMessage::User { attachments, .. } => attachments.len(),
                _ => 0,
            })
            .sum::<usize>();

        assert_eq!(image_count, 4);
        assert!(matches!(
            &payload[0],
            ChatMessage::User { content, attachments }
                if attachments.is_empty() && content.contains("0.png")
        ));
        assert_eq!(
            history[0],
            ChatMessage::user_with_attachments(
                "prompt 0",
                vec![Attachment::Image {
                    name: "0.png".into(),
                    mime_type: "image/png".into(),
                    path: "/stored/0.png".into(),
                }]
            )
        );
    }

    #[test]
    fn provider_payload_omits_all_images_for_text_only_model() {
        let history = vec![ChatMessage::user_with_attachments(
            "check this scene",
            vec![Attachment::Image {
                name: "scene.png".into(),
                mime_type: "image/png".into(),
                path: "/stored/scene.png".into(),
            }],
        )];
        let payload = model_messages(&history, false);

        assert_eq!(payload.len(), 1);

        let ChatMessage::User {
            content,
            attachments,
        } = &payload[0]
        else {
            panic!("expected user message");
        };

        assert!(attachments.is_empty());
        assert!(content.contains("check this scene"));
        assert!(
            content
                .contains("[Image omitted: current model does not support image input: scene.png]")
        );
    }

    fn assistant_tool_call(id: &str, name: &str, arguments: &str) -> ChatCompletion {
        ChatCompletion {
            id: None,
            model: Some("mock".to_owned()),
            message: ChatMessage::Assistant {
                content: None,
                tool_calls: vec![ToolCall {
                    metadata: None,
                    id: id.to_owned(),
                    kind: "function".to_owned(),
                    function: FunctionCall {
                        name: name.to_owned(),
                        arguments: arguments.to_owned(),
                    },
                }],
            },
            finish_reason: Some("tool_calls".to_owned()),
            usage: None,
        }
    }

    fn request() -> ChatRequest {
        ChatRequest::new(vec![ChatMessage::user("hello")])
    }

    struct ImageTool;

    impl Tool for ImageTool {
        fn definition(&self) -> orbit_model::types::ToolDefinition {
            orbit_model::types::ToolDefinition::function("read_image", "", json!({}))
        }

        fn execute<'a>(
            &'a self,
            _call: ToolCall,
            _cancellation: CancellationToken,
        ) -> ToolFuture<'a> {
            Box::pin(async {
                Ok(ToolOutput::with_attachments(
                    r#"{"loaded":true}"#,
                    vec![Attachment::Image {
                        name: "scene.png".into(),
                        mime_type: "image/png".into(),
                        path: "/global/scene.png".into(),
                    }],
                ))
            })
        }
    }

    #[tokio::test]
    async fn image_tool_output_is_added_to_the_next_model_request() {
        let backend = MockModel::new(vec![
            assistant_tool_call("image-call", "read_image", "{}"),
            assistant_text("I can see it"),
        ])
        .with_vision(true);
        let mut tools = ToolRegistry::new();
        tools.register(ImageTool);
        let agent = Agent::new(backend.clone(), tools, AgentConfig::default());
        let result = agent
            .run(request(), CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.turns, 2);

        let requests = backend.requests();

        assert_eq!(requests.len(), 2);
        assert!(matches!(
            requests[1].messages.last(),
            Some(ChatMessage::User { content, attachments })
                if content.contains("scene.png") && attachments.len() == 1
        ));
        assert!(matches!(
            &requests[1].messages[2],
            ChatMessage::Tool { tool_call_id, .. } if tool_call_id == "image-call"
        ));
    }

    #[tokio::test]
    async fn scenario_1_text_only_model_removes_read_image_and_fails_fast_on_invocation() {
        let backend = MockModel::new(vec![
            assistant_tool_call("image-call", "read_image", "{}"),
            assistant_text("I see it failed"),
        ])
        .with_vision(false);
        let mut tools = ToolRegistry::new();
        tools.register(ImageTool);
        let agent = Agent::new(backend.clone(), tools, AgentConfig::default());

        // Verify tool definition does not expose read_image
        assert!(!agent.tools.contains("read_image"));
        assert!(
            !agent
                .tools
                .definitions()
                .iter()
                .any(|d| d.function.name == "read_image")
        );

        let result = agent
            .run(request(), CancellationToken::new())
            .await
            .unwrap();

        // The assistant called read_image, which agent intercepted as unsupported, returning tool error
        assert_eq!(result.turns, 2);

        let requests = backend.requests();

        assert_eq!(requests.len(), 2);
        // The tool result in the next request should contain the informative error
        assert!(matches!(
            &requests[1].messages[2],
            ChatMessage::Tool { content, .. } if content.contains("current model does not support image input")
        ));
        // No user_with_attachments was pushed with image
        assert!(!requests[1].messages.iter().any(|m| match m {
            ChatMessage::User { attachments, .. } => !attachments.is_empty(),
            _ => false,
        }));
    }

    #[tokio::test]
    async fn scenario_2_model_failure_preserves_attachments_in_checkpoint_losslessly() {
        use std::sync::Arc;

        let journal = Arc::new(FailingJournal {
            checkpoints: std::sync::Mutex::new(Vec::new()),
            fail_start: false,
            fail_receipt: false,
        });
        let backend = MockModel::from_results(vec![
            Ok(assistant_tool_call("image-call", "read_image", "{}")),
            Err(orbit_model::error::ModelError::Protocol(
                "HTTP 500: rate limit or transient failure".into(),
            )),
        ])
        .with_vision(true);
        let mut tools = ToolRegistry::new();
        tools.register(ImageTool);
        let agent = Agent::new(backend.clone(), tools, AgentConfig::default())
            .with_execution_journal(journal.clone());
        let result = agent.run(request(), CancellationToken::new()).await;

        assert!(result.is_err());

        // Check the last saved checkpoint in journal: canonical history preserves image attachments losslessly
        let checkpoints = journal.checkpoints.lock().unwrap();
        let last_checkpoint = checkpoints.last().expect("saved checkpoint");
        let has_raw_attachments = last_checkpoint.history.iter().any(|m| match m {
            ChatMessage::User { attachments, .. } => !attachments.is_empty(),
            _ => false,
        });

        assert!(
            has_raw_attachments,
            "checkpoint must preserve raw image attachments losslessly on model failure"
        );
    }

    #[tokio::test]
    async fn scenario_3_retry_after_model_failure_retains_image_attachment() {
        use std::sync::Arc;

        let journal = Arc::new(FailingJournal {
            checkpoints: std::sync::Mutex::new(Vec::new()),
            fail_start: false,
            fail_receipt: false,
        });
        let backend = MockModel::from_results(vec![
            Ok(assistant_tool_call("image-call", "read_image", "{}")),
            Err(orbit_model::error::ModelError::Protocol(
                "HTTP 500: transient error".into(),
            )),
        ])
        .with_vision(true);
        let mut tools = ToolRegistry::new();
        tools.register(ImageTool);
        let agent = Agent::new(backend.clone(), tools.clone(), AgentConfig::default())
            .with_execution_journal(journal.clone());
        let _ = agent.run(request(), CancellationToken::new()).await;

        // Recover history from the checkpoint
        let restored_history = {
            let checkpoints = journal.checkpoints.lock().unwrap();
            checkpoints.last().unwrap().history.clone()
        };

        // Verify restored_history still has image attachment losslessly
        assert!(restored_history.iter().any(|m| match m {
            ChatMessage::User { attachments, .. } => !attachments.is_empty(),
            _ => false,
        }));

        // Retry / follow-up with working vision model succeeds and sends the image
        let follow_up_backend =
            MockModel::new(vec![assistant_text("I see the image and help you")]).with_vision(true);
        let follow_up_agent = Agent::new(follow_up_backend.clone(), tools, AgentConfig::default());
        let result = follow_up_agent
            .run(ChatRequest::new(restored_history), CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.turns, 1);

        let requests = follow_up_backend.requests();

        assert_eq!(requests.len(), 1);
        assert!(requests[0].messages.iter().any(|m| match m {
            ChatMessage::User { attachments, .. } => !attachments.is_empty(),
            _ => false,
        }));
    }

    #[tokio::test]
    async fn scenario_4_resuming_failed_conversation_with_text_only_model() {
        // Conversation has a poisoned historical image attachment (e.g. created prior to fix)
        let historical_poison = vec![
            ChatMessage::system("system"),
            ChatMessage::user("look at 1.jpg"),
            ChatMessage::user_with_attachments(
                "The read_image tool loaded the following local image into this model request: 1.jpg. Analyze the actual image content directly.",
                vec![Attachment::Image {
                    name: "1.jpg".into(),
                    mime_type: "image/jpeg".into(),
                    path: "/stored/1.jpg".into(),
                }],
            ),
            ChatMessage::user("你看，我就说你不能识图吧"),
        ];

        let text_only_backend =
            MockModel::new(vec![assistant_text("确实，当前模型不支持直接识图。")])
                .with_vision(false);
        let agent = Agent::new(
            text_only_backend.clone(),
            ToolRegistry::new(),
            AgentConfig::default(),
        );
        let result = agent
            .run(
                ChatRequest::new(historical_poison),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(result.turns, 1);

        let requests = text_only_backend.requests();

        assert_eq!(requests.len(), 1);

        // Verify that the request sent to the text-only model has 0 image attachments
        for msg in &requests[0].messages {
            if let ChatMessage::User { attachments, .. } = msg {
                assert!(
                    attachments.is_empty(),
                    "text-only model must receive 0 attachments"
                );
            }
        }

        let recovered_image_msg = requests[0].messages.iter().find(|m| match m {
            ChatMessage::User { content, .. } => content
                .contains("[Image omitted: current model does not support image input: 1.jpg]"),
            _ => false,
        });

        assert!(
            recovered_image_msg.is_some(),
            "must contain degradation text notice"
        );
    }

    #[tokio::test]
    async fn scenario_5_switching_conversation_with_historical_images_to_text_only_and_back_to_vision()
     {
        // Step 1: Run with multimodal model where read_image completes and assistant answers
        let vision_backend = MockModel::new(vec![
            assistant_tool_call("image-call", "read_image", "{}"),
            assistant_text("I see a blue square in scene.png."),
        ])
        .with_vision(true);
        let mut tools = ToolRegistry::new();
        tools.register(ImageTool);
        let agent = Agent::new(vision_backend, tools.clone(), AgentConfig::default());
        let result = agent
            .run(request(), CancellationToken::new())
            .await
            .unwrap();
        let history_after_vision = result.history;

        // Step 2: Switch to text-only model and ask a question about the image
        let mut switched_history = history_after_vision;
        switched_history.push(ChatMessage::user("What color was the square?"));
        let text_only_backend =
            MockModel::new(vec![assistant_text("The square was blue.")]).with_vision(false);
        let text_agent = Agent::new(
            text_only_backend.clone(),
            tools.clone(),
            AgentConfig::default(),
        );
        let result2 = text_agent
            .run(ChatRequest::new(switched_history), CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result2.turns, 1);

        let requests = text_only_backend.requests();

        assert_eq!(requests.len(), 1);

        // Ensure no image attachment was sent to the text-only model
        for msg in &requests[0].messages {
            if let ChatMessage::User { attachments, .. } = msg {
                assert!(attachments.is_empty());
            }
        }

        assert!(requests[0].messages.iter().any(|m| match m {
            ChatMessage::User { content, .. } =>
                content.contains(
                    "[Image omitted: current model does not support image input: scene.png]"
                ),
            _ => false,
        }));
        // Verify canonical history still retains the image attachment losslessly!
        assert!(result2.history.iter().any(|m| match m {
            ChatMessage::User { attachments, .. } => !attachments.is_empty(),
            _ => false,
        }));

        // Step 3: Switch back to vision model; original image attachment is still preserved and sent
        let mut back_to_vision_history = result2.history;
        back_to_vision_history.push(ChatMessage::user(
            "Look at the image again, what is in the corner?",
        ));
        let vision_backend_2 =
            MockModel::new(vec![assistant_text("In the corner is a circle.")]).with_vision(true);
        let vision_agent_2 = Agent::new(vision_backend_2.clone(), tools, AgentConfig::default());
        let result3 = vision_agent_2
            .run(
                ChatRequest::new(back_to_vision_history),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(result3.turns, 1);

        let requests2 = vision_backend_2.requests();

        assert_eq!(requests2.len(), 1);
        assert!(
            requests2[0].messages.iter().any(|m| match m {
                ChatMessage::User { attachments, .. } => !attachments.is_empty(),
                _ => false,
            }),
            "vision model must receive restored raw image attachment"
        );
    }

    #[tokio::test]
    async fn token_exhaustion_does_not_retry_or_execute_partial_tools() {
        for message in [
            ChatMessage::assistant(""),
            ChatMessage::assistant_tool_calls(vec![ToolCall::function(
                "partial",
                "forbidden",
                json!({}),
            )]),
        ] {
            for streaming in [false, true] {
                let backend = MockModel::new(vec![ChatCompletion {
                    id: None,
                    model: None,
                    message: message.clone(),
                    finish_reason: Some("max_tokens".into()),
                    usage: None,
                }]);
                let mut tools = ToolRegistry::new();
                tools.register(ClosureTool::new("forbidden", "", json!({}), |_, _| async {
                    panic!("truncated response must not run tools")
                }));
                let agent = Agent::new(backend.clone(), tools, AgentConfig::default());
                let mut req = request();
                req.max_tokens = Some(16384);
                let result = if streaming {
                    let (tx, _rx) = mpsc::channel(64);
                    agent
                        .run_with_agent_events(req, CancellationToken::new(), tx)
                        .await
                } else {
                    agent.run(req, CancellationToken::new()).await
                };

                let error = result.unwrap_err().to_string();

                assert!(
                    error.contains("max_tokens") && error.contains("max_output_tokens"),
                    "{error}"
                );
                assert_eq!(backend.requests().len(), 1);
                assert_eq!(backend.requests()[0].max_tokens, Some(16384));
            }
        }
    }

    struct UsageTailModel;

    impl ModelBackend for UsageTailModel {
        fn complete<'a>(
            &'a self,
            _: ChatRequest,
            _: CancellationToken,
        ) -> crate::model::CompletionFuture<'a> {
            Box::pin(async { panic!("a completed stream must not fall back to complete") })
        }

        fn stream<'a>(
            &'a self,
            _: ChatRequest,
            _: CancellationToken,
        ) -> crate::model::StreamFuture<'a> {
            Box::pin(async {
                Ok(Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta("partial".into())),
                    Ok(StreamEvent::Completed {
                        finish_reason: Some("length".into()),
                        usage: None,
                    }),
                    Ok(StreamEvent::Completed {
                        finish_reason: None,
                        usage: Some(Usage {
                            prompt_tokens: Some(10),
                            completion_tokens: Some(5),
                            total_tokens: Some(15),
                        }),
                    }),
                ])) as orbit_model::types::ModelStream)
            })
        }
    }

    #[tokio::test]
    async fn usage_only_tail_does_not_hide_a_truncated_stream_reason() {
        for with_agent_events in [false, true] {
            let agent = Agent::new(UsageTailModel, ToolRegistry::new(), AgentConfig::default());
            let error = if with_agent_events {
                let (sender, _receiver) = mpsc::channel(16);
                agent
                    .run_with_agent_events(request(), CancellationToken::new(), sender)
                    .await
                    .unwrap_err()
            } else {
                agent
                    .run_with_events(request(), CancellationToken::new(), |_| {})
                    .await
                    .unwrap_err()
            };

            assert!(error.to_string().contains("输出达到上限"));
        }
    }

    struct SparseToolModel(MockModel);

    impl ModelBackend for SparseToolModel {
        fn complete<'a>(
            &'a self,
            request: ChatRequest,
            token: CancellationToken,
        ) -> crate::model::CompletionFuture<'a> {
            self.0.complete(request, token)
        }

        fn stream<'a>(
            &'a self,
            request: ChatRequest,
            token: CancellationToken,
        ) -> crate::model::StreamFuture<'a> {
            Box::pin(async move {
                let stream = self.0.stream(request, token).await?;

                Ok(Box::pin(stream.map(|event| {
                    event.map(|event| match event {
                        StreamEvent::ToolCallDelta(mut delta) => {
                            delta.index = delta.index * 3 + 2;
                            StreamEvent::ToolCallDelta(delta)
                        }
                        other => other,
                    })
                })) as orbit_model::types::ModelStream)
            })
        }
    }

    #[tokio::test]
    async fn sparse_content_indices_execute_only_real_tools_in_both_stream_paths() {
        for channel in [false, true] {
            let calls = vec![
                ToolCall::function("first", "echo", json!({ "text": "one" })),
                ToolCall::function("second", "echo", json!({ "text": "two" })),
            ];
            let mock = MockModel::new(vec![
                ChatCompletion {
                    id: None,
                    model: None,
                    message: ChatMessage::Assistant {
                        content: Some("Reading files".into()),
                        tool_calls: calls.clone(),
                    },
                    finish_reason: Some("tool_use".into()),
                    usage: None,
                },
                assistant_text("done"),
            ]);
            let mut tools = ToolRegistry::new();
            tools.register(ClosureTool::new(
                "echo",
                "echo",
                json!({ "type": "object" }),
                |args, _| async move { Ok(args.to_string()) },
            ));
            let agent = Agent::new(SparseToolModel(mock.clone()), tools, AgentConfig::default());
            let result = if channel {
                let (tx, _rx) = mpsc::channel(64);
                agent
                    .run_with_agent_events(request(), CancellationToken::new(), tx)
                    .await
            } else {
                agent
                    .run_with_events(request(), CancellationToken::new(), |_| {})
                    .await
            }
            .unwrap();

            assert_eq!(result.tool_calls, 2);

            let sent = mock.requests();

            assert_eq!(sent.len(), 2);
            assert_eq!(sent[1].messages.len(), 4);

            let ChatMessage::Assistant { tool_calls, .. } = &sent[1].messages[1] else {
                panic!("assistant")
            };

            assert_eq!(tool_calls, &calls);
            assert!(
                matches!(&sent[1].messages[2], ChatMessage::Tool { tool_call_id, .. } if tool_call_id == "first")
            );
            assert!(
                matches!(&sent[1].messages[3], ChatMessage::Tool { tool_call_id, .. } if tool_call_id == "second")
            );
        }
    }

    #[test]
    fn tool_signature_metadata_survives_delta_merging_and_history_serialization() {
        let metadata = orbit_model::types::ToolCallMetadata {
            google: orbit_model::types::GoogleToolMetadata {
                model: "test-model".into(),
                thought_signature: Some("opaque-signature".into()),
                native_id: None,
            },
        };
        let mut calls = BTreeMap::new();

        merge_tool_call(
            &mut calls,
            ToolCallDelta {
                metadata: Some(metadata.clone()),
                index: 0,
                id: Some("call".into()),
                name: Some("read_file".into()),
                arguments: "{".into(),
            },
        );
        merge_tool_call(
            &mut calls,
            ToolCallDelta {
                metadata: None,
                index: 0,
                id: None,
                name: None,
                arguments: "}".into(),
            },
        );
        let history = ChatMessage::assistant_tool_calls(calls.into_values().collect());
        let restored: ChatMessage =
            serde_json::from_str(&serde_json::to_string(&history).unwrap()).unwrap();

        assert_eq!(restored, history);
        let ChatMessage::Assistant { tool_calls, .. } = restored else {
            panic!("assistant history expected")
        };
        assert_eq!(tool_calls[0].metadata, Some(metadata));
        assert_eq!(tool_calls[0].function.arguments, "{}");
    }

    #[test]
    fn sparse_and_large_indices_merge_argument_fragments_without_padding() {
        let mut calls = BTreeMap::new();

        for (index, id, name, arguments) in [
            (usize::MAX, Some("later"), Some("echo"), "{\"text\":"),
            (2, Some("first"), Some("echo"), "{}"),
            (usize::MAX, None, None, "\"你好\"}"),
        ] {
            merge_tool_call(
                &mut calls,
                ToolCallDelta {
                    metadata: None,
                    index,
                    id: id.map(str::to_owned),
                    name: name.map(str::to_owned),
                    arguments: arguments.into(),
                },
            );
        }

        let calls: Vec<_> = calls.into_values().collect();

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "first");
        assert_eq!(calls[1].function.arguments, "{\"text\":\"你好\"}");
    }

    #[tokio::test]
    async fn malformed_ids_stop_before_tools_or_a_followup_request() {
        for ids in [["", ""], ["same", "same"]] {
            for streaming in [false, true] {
                let mock = MockModel::new(vec![ChatCompletion {
                    id: None,
                    model: None,
                    message: ChatMessage::assistant_tool_calls(
                        ids.into_iter()
                            .map(|id| ToolCall::function(id, "fail_if_executed", json!({})))
                            .collect(),
                    ),
                    finish_reason: None,
                    usage: None,
                }]);
                let mut tools = ToolRegistry::new();
                tools.register(ClosureTool::new(
                    "fail_if_executed",
                    "",
                    json!({}),
                    |_, _| async { panic!("invalid response must not execute any tools") },
                ));
                let agent = Agent::new(mock.clone(), tools, AgentConfig::default());
                let error = if streaming {
                    let (tx, _rx) = mpsc::channel(64);
                    agent
                        .run_with_agent_events(request(), CancellationToken::new(), tx)
                        .await
                } else {
                    agent.run(request(), CancellationToken::new()).await
                }
                .unwrap_err();

                assert!(error.to_string().contains("empty or duplicate"));
                assert_eq!(mock.requests().len(), 1);
            }
        }
    }

    struct ReasoningModel;

    impl ModelBackend for ReasoningModel {
        fn complete<'a>(
            &'a self,
            _: ChatRequest,
            _: CancellationToken,
        ) -> crate::model::CompletionFuture<'a> {
            Box::pin(async { Ok(assistant_text("answer")) })
        }

        fn stream<'a>(
            &'a self,
            _: ChatRequest,
            _: CancellationToken,
        ) -> crate::model::StreamFuture<'a> {
            Box::pin(async {
                Ok(Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::ReasoningDelta("thinking".into())),
                    Ok(StreamEvent::TextDelta("answer".into())),
                    Ok(StreamEvent::Completed {
                        finish_reason: Some("stop".into()),
                        usage: None,
                    }),
                ])) as orbit_model::types::ModelStream)
            })
        }
    }

    #[tokio::test]
    async fn reasoning_is_displayed_but_not_added_to_assistant_history() {
        let agent = Agent::new(ReasoningModel, ToolRegistry::new(), AgentConfig::default());
        let (sender, mut receiver) = mpsc::channel(16);
        let result = agent
            .run_with_agent_events(request(), CancellationToken::new(), sender)
            .await
            .unwrap();

        assert_eq!(result.final_message, ChatMessage::assistant("answer"));

        let mut saw_reasoning = false;

        while let Some(event) = receiver.recv().await {
            saw_reasoning |= matches!(event, AgentEvent::Model(StreamEvent::ReasoningDelta(_)));
        }

        assert!(saw_reasoning);

        let result = agent
            .run_with_events(request(), CancellationToken::new(), |_| {})
            .await
            .unwrap();

        assert_eq!(result.final_message, ChatMessage::assistant("answer"));
    }

    struct StreamOnlyModel;

    impl ModelBackend for StreamOnlyModel {
        fn complete<'a>(
            &'a self,
            _: ChatRequest,
            _: CancellationToken,
        ) -> crate::model::CompletionFuture<'a> {
            Box::pin(async { panic!("the canonical execution loop must use stream") })
        }

        fn stream<'a>(
            &'a self,
            _: ChatRequest,
            _: CancellationToken,
        ) -> crate::model::StreamFuture<'a> {
            Box::pin(async {
                Ok(Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta("streamed answer".into())),
                    Ok(StreamEvent::Completed {
                        finish_reason: Some("stop".into()),
                        usage: None,
                    }),
                ])) as orbit_model::types::ModelStream)
            })
        }
    }

    #[tokio::test]
    async fn non_event_run_uses_the_same_streaming_core() {
        let agent = Agent::new(StreamOnlyModel, ToolRegistry::new(), AgentConfig::default());
        let result = agent
            .run(request(), CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(
            result.final_message,
            ChatMessage::assistant("streamed answer")
        );
    }

    #[tokio::test]
    async fn completes_when_model_returns_text() {
        let model = MockModel::new([assistant_text("hello back")]);
        let agent = Agent::new(model.clone(), ToolRegistry::new(), AgentConfig::default());

        let result = agent
            .run(request(), CancellationToken::new())
            .await
            .expect("agent completes");

        assert_eq!(result.final_message, ChatMessage::assistant("hello back"));
        assert_eq!(result.turns, 1);
        assert_eq!(result.tool_calls, 0);
        assert_eq!(result.history.len(), 2);
        assert_eq!(model.requests().len(), 1);
    }

    #[tokio::test]
    async fn executes_tool_and_sends_result_back_to_model() {
        let model = MockModel::new([
            assistant_tool_call("call-1", "echo", r#"{"value":"hello"}"#),
            assistant_text("tool finished"),
        ]);
        let mut tools = ToolRegistry::new();
        tools.register(ClosureTool::new(
            "echo",
            "Echo a value",
            json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                }
            }),
            |arguments, _cancellation| async move {
                Ok(arguments["value"].as_str().unwrap_or_default().to_owned())
            },
        ));
        let agent = Agent::new(model.clone(), tools, AgentConfig::default());

        let result = agent
            .run(request(), CancellationToken::new())
            .await
            .expect("agent completes");

        assert_eq!(
            result.final_message,
            ChatMessage::assistant("tool finished")
        );
        assert_eq!(result.turns, 2);
        assert_eq!(result.tool_calls, 1);
        assert_eq!(result.history.len(), 4);
        assert!(matches!(
            &result.history[2],
            ChatMessage::Tool {
                tool_call_id,
                content
            } if tool_call_id == "call-1" && content == "hello"
        ));

        let requests = model.requests();

        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].messages.len(), 3);
        assert_eq!(requests[1].tools.len(), 1);
    }

    #[tokio::test]
    async fn tool_failure_is_returned_to_model_as_tool_message() {
        let model = MockModel::new([
            assistant_tool_call("call-1", "missing", "{}"),
            assistant_text("I recovered"),
        ]);
        let agent = Agent::new(model, ToolRegistry::new(), AgentConfig::default());

        let result = agent
            .run(request(), CancellationToken::new())
            .await
            .expect("tool failure is recoverable");

        assert_eq!(result.final_message, ChatMessage::assistant("I recovered"));
        assert!(matches!(
            &result.history[2],
            ChatMessage::Tool { content, .. } if content.contains("unknown tool: missing")
        ));
    }

    #[tokio::test]
    async fn invalid_tool_arguments_are_returned_to_model() {
        let model = MockModel::new([
            assistant_tool_call("call-1", "echo", "not-json"),
            assistant_text("bad arguments handled"),
        ]);
        let mut tools = ToolRegistry::new();
        tools.register(ClosureTool::new(
            "echo",
            "Echo a value",
            json!({ "type": "object" }),
            |_arguments, _cancellation| async {
                Err(ToolError::ExecutionFailed("should not execute".to_owned()))
            },
        ));
        let agent = Agent::new(model, tools, AgentConfig::default());

        let result = agent
            .run(request(), CancellationToken::new())
            .await
            .expect("invalid arguments are recoverable");

        assert!(matches!(
            &result.history[2],
            ChatMessage::Tool { content, .. }
                if content.contains("invalid arguments for tool echo")
        ));
    }

    #[tokio::test]
    async fn default_budget_allows_long_tasks() {
        for streaming in [false, true] {
            let responses = (0..40)
                .map(|i| assistant_tool_call(&format!("call-{i}"), "missing", "{}"))
                .chain(std::iter::once(assistant_text("finished")));
            let agent = Agent::new(
                MockModel::new(responses),
                ToolRegistry::new(),
                AgentConfig::default(),
            );
            let result = if streaming {
                agent
                    .run_with_events(request(), CancellationToken::new(), |_| {})
                    .await
            } else {
                agent.run(request(), CancellationToken::new()).await
            }
            .unwrap();

            assert_eq!(result.turns, 41);
            assert_eq!(result.tool_calls, 40);
        }
    }

    #[tokio::test]
    async fn max_turns_stops_an_unfinished_loop() {
        let model = MockModel::new([
            assistant_tool_call("call-1", "missing", "{}"),
            assistant_tool_call("call-2", "missing", "{}"),
        ]);
        let agent = Agent::new(
            model,
            ToolRegistry::new(),
            AgentConfig {
                max_turns: 1,
                max_tool_calls: 10,
                ..AgentConfig::default()
            },
        );

        let error = agent
            .run(request(), CancellationToken::new())
            .await
            .expect_err("turn budget must stop the loop");

        assert!(matches!(error, AgentError::MaxTurns));
    }

    #[tokio::test]
    async fn max_tool_calls_stops_tool_execution() {
        let first = ToolCall {
            metadata: None,
            id: "call-1".to_owned(),
            kind: "function".to_owned(),
            function: FunctionCall {
                name: "missing".to_owned(),
                arguments: "{}".to_owned(),
            },
        };
        let second = ToolCall {
            metadata: None,
            id: "call-2".to_owned(),
            ..first.clone()
        };
        let model = MockModel::new([ChatCompletion {
            id: None,
            model: Some("mock".to_owned()),
            message: ChatMessage::Assistant {
                content: None,
                tool_calls: vec![first, second],
            },
            finish_reason: Some("tool_calls".to_owned()),
            usage: None,
        }]);
        let journal = std::sync::Arc::new(FailingJournal {
            checkpoints: Default::default(),
            fail_start: false,
            fail_receipt: false,
        });
        let agent = Agent::new(
            model,
            ToolRegistry::new(),
            AgentConfig {
                max_turns: 4,
                max_tool_calls: 1,
                ..AgentConfig::default()
            },
        )
        .with_execution_journal(journal.clone());

        let error = agent
            .run(request(), CancellationToken::new())
            .await
            .expect_err("tool budget must stop the loop");

        assert!(matches!(error, AgentError::MaxToolCalls));
        assert!(journal
            .checkpoints
            .lock()
            .unwrap()
            .iter()
            .all(|checkpoint| !checkpoint.history.iter().any(|message| {
                matches!(message, ChatMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty())
            })));
    }

    #[tokio::test]
    async fn agent_event_channel_reports_stream_and_terminal_state() {
        let model = MockModel::new([assistant_text("hello back")]);
        let agent = Agent::new(model, ToolRegistry::new(), AgentConfig::default());
        let (sender, mut receiver) = mpsc::channel(16);

        agent
            .run_with_agent_events(request(), CancellationToken::new(), sender)
            .await
            .expect("agent completes");

        let mut events = Vec::new();

        while let Some(event) = receiver.recv().await {
            events.push(event);
        }

        assert!(matches!(
            events.first(),
            Some(AgentEvent::TurnStarted { turn: 1 })
        ));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                AgentEvent::Model(StreamEvent::TextDelta(text)) if text == "hello back"
            )
        }));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::Completed { .. }))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn cancellation_is_checked_before_first_model_call() {
        let model = MockModel::new([assistant_text("never returned")]);
        let agent = Agent::new(model.clone(), ToolRegistry::new(), AgentConfig::default());
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = agent
            .run(request(), cancellation)
            .await
            .expect_err("cancelled agent must stop");

        assert!(matches!(error, AgentError::Cancelled));
        assert!(model.requests().is_empty());
    }

    #[test]
    fn usage_events_combine_input_and_output_counts() {
        let mut usage = None;
        merge_usage(
            &mut usage,
            Some(Usage {
                prompt_tokens: Some(321),
                completion_tokens: None,
                total_tokens: None,
            }),
        );
        merge_usage(
            &mut usage,
            Some(Usage {
                prompt_tokens: None,
                completion_tokens: Some(16384),
                total_tokens: None,
            }),
        );

        assert_eq!(
            usage,
            Some(Usage {
                prompt_tokens: Some(321),
                completion_tokens: Some(16384),
                total_tokens: None,
            })
        );
    }
}
