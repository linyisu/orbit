use anyhow::Result;
use orbit_agent::{
    agent::{Agent, AgentConfig, AgentResult},
    error::AgentError,
    events::AgentEvent,
    memory::CompactionStrategy,
    model::ModelClientBackend,
    tool::ToolRegistry,
};
use orbit_model::{
    client::ModelClient,
    types::{Attachment, ChatMessage, ChatRequest},
};
use orbit_store::types::Run;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use crate::session::SessionState;

pub type RunTask = JoinHandle<Result<AgentResult, AgentError>>;

pub struct StartedRun {
    pub receiver: mpsc::Receiver<AgentEvent>,
    pub inbox: orbit_agent::inbox::AgentInbox,
    pub cancellation: CancellationToken,
    pub task: RunTask,
    pub context_used_tokens: usize,
}

pub struct RunOptions {
    pub model_name: String,
    pub generate_title: bool,
    pub max_output_tokens: u32,
    pub config: AgentConfig,
    pub calibration: orbit_agent::calibration::TokenCalibration,
    pub compaction: orbit_agent::compaction::CompactionRuntime,
}

#[allow(clippy::too_many_arguments)]
pub async fn start(
    session: &mut SessionState,
    prompt: String,
    attachments: Vec<Attachment>,
    compacting: bool,
    history: &[ChatMessage],
    model: &ModelClient,
    tools: &ToolRegistry,
    options: RunOptions,
) -> Result<StartedRun> {
    let max_output_tokens = options.max_output_tokens;
    let mut messages = if compacting {
        compaction_source_history(session, history).await?
    } else {
        history.to_vec()
    };
    if options.generate_title {
        session
            .begin_with_title(prompt.clone(), options.model_name.clone(), model)
            .await?;
    } else {
        session
            .begin(prompt.clone(), options.model_name.clone())
            .await?;
    }
    if !compacting {
        messages.push(ChatMessage::user_with_attachments(prompt, attachments));
    }
    let context_used_tokens = options
        .calibration
        .estimate(orbit_agent::context::estimate_tokens(
            &messages,
            &tools.definitions(),
        ));
    let mut request = ChatRequest::new(messages);
    request.max_tokens = Some(max_output_tokens);
    request.reasoning_effort = model.config().reasoning_effort;
    let (sender, receiver) = mpsc::channel(256);
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    let tools = tools.clone();
    let backend = ModelClientBackend::new(model.clone());
    let files = orbit_agent::memory::FileActivity::from_events(
        session.snapshot.runs.iter().flat_map(|run| &run.events),
    );
    let config = AgentConfig {
        context_tokens: options.config.context_tokens,
        max_turns: options.config.max_turns,
        max_tool_calls: options.config.max_tool_calls,
        auto_compact: options.config.auto_compact,
    };
    let archive = std::sync::Arc::new(crate::history::SessionArchive::new(
        session.store.clone(),
        session.id().into(),
    ));
    let local_limit = options
        .calibration
        .local_limit(options.config.context_tokens, max_output_tokens as usize);
    let inbox = orbit_agent::inbox::AgentInbox::default();
    if !compacting {
        for message in session.store.pending_agent_messages(session.id()).await? {
            inbox.push(message).await?;
        }
    }
    let mut agent = Agent::new(backend.clone(), tools.clone(), config)
        .with_inbox(inbox.clone())
        .with_calibration(options.model_name.clone(), options.calibration)
        .with_compaction(options.compaction.clone())
        .with_file_activity(files.clone())
        .with_context_archive(archive.clone());

    if !compacting {
        agent = agent.with_execution_journal(session.execution_journal()?);
    }

    let summary_runtime = options
        .compaction
        .for_input_window(local_limit.saturating_sub(max_output_tokens as usize));
    let task = tokio::spawn(async move {
        if compacting {
            let compacted = orbit_agent::context::compact_configured(
                &backend,
                request.messages,
                &tools.definitions(),
                local_limit,
                max_output_tokens as usize,
                token,
                &files,
                Some(archive.as_ref()),
                &summary_runtime,
            )
            .await?;

            orbit_agent::context::check_budget(
                &compacted.history,
                &tools.definitions(),
                max_output_tokens as usize,
                local_limit,
            )?;
            let fallback = compacted.record.fallback;
            let fallback_reason = compacted.record.fallback_reason.clone();
            let before_tokens = compacted.before_tokens;
            let after_tokens = compacted.after_tokens;
            let strategy = compacted.record.strategy.clone();
            sender
                .send(AgentEvent::CompactionRecorded {
                    record: compacted.record,
                })
                .await
                .map_err(|_| AgentError::EventChannelClosed)?;
            sender
                .send(AgentEvent::ContextCompacted {
                    strategy,
                    before_tokens,
                    after_tokens,
                    fallback,
                    fallback_reason,
                })
                .await
                .map_err(|_| AgentError::EventChannelClosed)?;
            return Ok(AgentResult {
                // The compacted history is already durable. The nonempty
                // sentinel keeps the common run-finalization path from
                // mistaking this maintenance command for an empty model reply.
                final_message: ChatMessage::assistant("context compacted"),
                history: compacted.history,
                turns: 1,
                tool_calls: 0,
            });
        }

        agent.run_with_agent_events(request, token, sender).await
    });

    Ok(StartedRun {
        inbox,
        context_used_tokens,
        receiver,
        cancellation,
        task,
    })
}

// A failed model summary may have committed a retrieval-backed projection. If
// the user retries immediately, summarize the immutable source snapshot rather
// than trying to compress the already small projection a second time.
fn retry_archive_id(runs: &[Run], history: &[ChatMessage]) -> Option<i64> {
    let last_completed = runs.iter().rev().find(|run| run.status == "completed")?;

    if last_completed.prompt != "/compact" {
        return None;
    }

    let reference = last_completed.events.iter().find_map(|event| match event {
        AgentEvent::CompactionRecorded { record }
            if record.strategy == CompactionStrategy::ArchiveProjection =>
        {
            record.archive.as_deref()
        }

        _ => None,
    })?;
    let id = reference
        .strip_prefix("read_history archive_id=")?
        .split(';')
        .next()?
        .parse::<i64>()
        .ok()
        .filter(|id| *id > 0)?;
    let marker = format!("read_history archive_id={id};");

    history
        .iter()
        .any(|message| {
            matches!(message, ChatMessage::User { content, .. }
            if content.starts_with("BEGIN_UNTRUSTED_COMPACTION_RECORD\n")
                && content.contains(&marker))
        })
        .then_some(id)
}

async fn compaction_source_history(
    session: &SessionState,
    history: &[ChatMessage],
) -> Result<Vec<ChatMessage>> {
    let Some(archive_id) = retry_archive_id(&session.snapshot.runs, history) else {
        return Ok(history.to_vec());
    };

    let source = session
        .store
        .load_context_archive(session.id(), archive_id)
        .await?;
    // Restore conversation data, but keep current instructions and runtime
    // state: an archived permission or task projection may be obsolete.
    let mut restored = history
        .iter()
        .filter(|message| matches!(message, ChatMessage::System { .. }))
        .cloned()
        .collect::<Vec<_>>();

    restored.extend(source.into_iter().filter(|message| {
        !matches!(
            message,
            ChatMessage::System { .. } | ChatMessage::Runtime { .. }
        )
    }));
    restored.extend(orbit_model::runtime_context::latest(history));

    Ok(restored)
}

pub fn outcome(
    result: std::result::Result<Result<AgentResult, AgentError>, tokio::task::JoinError>,
    cancellation: Option<&CancellationToken>,
) -> (&'static str, AgentEvent, Option<Vec<ChatMessage>>) {
    match result {
        Ok(Ok(result)) if matches!(&result.final_message, ChatMessage::Assistant { content, tool_calls } if content.as_deref().unwrap_or_default().trim().is_empty() && tool_calls.is_empty()) => {
            (
                "failed",
                AgentEvent::Failed {
                    error: "模型已结束响应，但未返回回答或工具调用；请检查服务端返回内容。".into(),
                },
                None,
            )
        }

        Ok(Ok(result)) => (
            "completed",
            AgentEvent::Completed {
                turns: result.turns,
                tool_calls: result.tool_calls,
            },
            Some(result.history),
        ),

        Ok(Err(_)) if cancellation.is_some_and(CancellationToken::is_cancelled) => {
            ("cancelled", AgentEvent::Cancelled, None)
        }
        Ok(Err(AgentError::Cancelled)) => ("cancelled", AgentEvent::Cancelled, None),
        Ok(Err(AgentError::CompactionFailed { kind, .. })) => (
            "failed",
            AgentEvent::Failed {
                error: format!("上下文整理未完成：{}。原上下文已保留。", kind.label()),
            },
            None,
        ),

        Ok(Err(AgentError::Compaction(_))) => (
            "failed",
            AgentEvent::Failed {
                error: "上下文整理未完成，原上下文已保留。".into(),
            },
            None,
        ),

        Ok(Err(error)) => (
            "failed",
            AgentEvent::Failed {
                error: error.to_string(),
            },
            None,
        ),

        Err(error) => (
            "failed",
            AgentEvent::Failed {
                error: format!("后台任务中断：{error}"),
            },
            None,
        ),
    }
}

#[cfg(test)]
mod tests;
