//! Contiguous observation batches; effects remain ordered barriers.
use super::*;
use crate::tool::{ToolContext, ToolError};
use futures_util::stream::FuturesOrdered;

const MAX_PARALLEL_READS: usize = 4;

pub(super) struct Execution<'a> {
    pub history: &'a mut Vec<ChatMessage>,
    pub turns: usize,
    pub tool_calls: &'a mut usize,
    pub files: &'a mut crate::memory::FileActivity,
}

impl<B: ModelBackend> Agent<B> {
    pub(super) async fn execute_calls<F, Fut>(
        &self,
        calls: Vec<ToolCall>,
        state: Execution<'_>,
        cancellation: CancellationToken,
        emit: &mut F,
    ) -> Result<(), AgentError>
    where
        F: FnMut(AgentEvent) -> Fut,
        Fut: Future<Output = Result<(), AgentError>>,
    {
        let Execution {
            history,
            turns,
            tool_calls,
            files,
        } = state;
        let mut loaded_attachments = Vec::new();
        let mut calls = calls.into_iter().peekable();
        while let Some(first) = calls.next() {
            if cancellation.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            let parallel = self
                .tools
                .call_capabilities(&first)
                .parallel_read();
            let mut batch = vec![first];
            while parallel && batch.len() < MAX_PARALLEL_READS {
                let Some(next) = calls.next_if(|call| {
                    self.tools
                        .call_capabilities(call)
                        .parallel_read()
                }) else {
                    break;
                };
                batch.push(next);
            }

            // Persist every start before polling any execution future. Failure
            // to save a receipt must prevent the next barrier from executing.
            let mut in_flight = Vec::new();
            for call in &batch {
                *tool_calls += 1;
                in_flight.push(call.id.clone());
                let event = AgentEvent::ToolStarted { call: call.clone() };
                self.checkpoint(
                    history,
                    turns,
                    *tool_calls,
                    in_flight.clone(),
                    &loaded_attachments,
                    Some(event.clone()),
                )
                .await?;
                emit(event).await?;
            }
            let mut executions: FuturesOrdered<_> = batch
                .into_iter()
                .map(|call| {
                    let cancellation = cancellation.clone();
                    async move {
                        let output = if call.function.name == "read_image"
                            && !self.backend.supports_vision()
                        {
                            Err(ToolError::ExecutionFailed(
                                "current model does not support image input".into(),
                            ))
                        } else {
                            self.tools
                                .execute_with_context(
                                    call.clone(),
                                    cancellation,
                                    ToolContext {
                                        supports_vision: self.backend.supports_vision()
                                            && self.tools.contains("read_image"),
                                    },
                                )
                                .await
                        };
                        (call, output)
                    }
                })
                .collect();

            // FuturesOrdered polls concurrently but publishes receipts in the
            // model's original order, independent of completion timing.
            loop {
                let next = if parallel {
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return Err(AgentError::Cancelled),
                        next = executions.next() => next,
                    }
                } else {
                    executions.next().await
                };
                let Some((call, output)) = next else { break };
                in_flight.retain(|id| id != &call.id);
                let (event, content) = match output {
                    Ok(output) => {
                        files.observe(&call, &output.content);
                        if let Some(evidence) = output.verification {
                            emit(AgentEvent::VerificationRecorded {
                                call_id: call.id.clone(),
                                evidence,
                            })
                            .await?;
                        }
                        let event = match output.background_job {
                            Some(job_id) => AgentEvent::ToolBackgrounded {
                                call: call.clone(),
                                job_id,
                                content: output.content.clone(),
                            },
                            None => AgentEvent::ToolFinished {
                                call: call.clone(),
                                content: output.content.clone(),
                                changes: output.changes,
                            },
                        };
                        loaded_attachments.extend(output.attachments);
                        (event, output.content)
                    }
                    Err(error) => {
                        let message = error.to_string();
                        (
                            AgentEvent::ToolFailed {
                                call: call.clone(),
                                error: message.clone(),
                            },
                            format!("tool error: {message}"),
                        )
                    }
                };
                let result = ChatMessage::tool(call.id, content);
                history.push(result.clone());
                if let Some(journal) = &self.journal {
                    journal.record_message(result);
                }
                self.checkpoint(
                    history,
                    turns,
                    *tool_calls,
                    in_flight.clone(),
                    &loaded_attachments,
                    Some(event.clone()),
                )
                .await?;
                emit(event).await?;
            }
        }

        let before = history.len();
        append_loaded_attachments(history, loaded_attachments);
        if let Some(journal) = &self.journal {
            for message in &history[before..] {
                journal.record_message(message.clone());
            }
        }
        self.checkpoint(history, turns, *tool_calls, None, &[], None)
            .await
    }
}
