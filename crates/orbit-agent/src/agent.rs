use std::{collections::HashSet, future::Future};

use futures_util::StreamExt;
use orbit_model::{
    error::ModelError,
    types::{
        ChatCompletion, ChatMessage, ChatRequest, FunctionCall, StreamEvent, ToolCall,
        ToolCallDelta, Usage,
    },
};
use tokio_util::sync::CancellationToken;

use crate::{
    context::ContextRuntime, error::AgentError, events::AgentEvent, model::ModelBackend,
    tool::ToolRegistry,
};

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub max_turns: usize,
    pub max_tool_calls: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 8,
            max_tool_calls: 32,
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

pub struct Agent<B> {
    backend: B,
    tools: ToolRegistry,
    config: AgentConfig,
    context: Option<ContextRuntime>,
}

impl<B> Agent<B>
where
    B: ModelBackend,
{
    pub fn new(backend: B, tools: ToolRegistry, config: AgentConfig) -> Self {
        Self {
            backend,
            tools,
            config,
            context: None,
        }
    }

    pub fn with_context(mut self, context: ContextRuntime) -> Self {
        self.context = Some(context);
        self
    }

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

    pub async fn run_with_agent_events<F, Fut>(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
        emit: F,
    ) -> Result<AgentResult, AgentError>
    where
        F: FnMut(AgentEvent) -> Fut,
        Fut: Future<Output = Result<(), AgentError>>,
    {
        self.run_core(request, cancellation, emit).await
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
        let result = self.execute_core(request, cancellation, &mut emit).await;
        let terminal = match &result {
            Ok(result) => AgentEvent::Completed {
                turns: result.turns,
                tool_calls: result.tool_calls,
            },
            Err(AgentError::Cancelled | AgentError::Model(ModelError::Cancelled)) => {
                AgentEvent::Cancelled
            }
            Err(error) => AgentEvent::Failed {
                error: error.to_string(),
            },
        };

        emit(terminal).await?;
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
        let mut turns = 0usize;
        let mut tool_calls = 0usize;
        let mut context = self.context.clone();

        loop {
            if cancellation.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            if self.config.max_turns > 0 && turns >= self.config.max_turns {
                return Err(AgentError::MaxTurns);
            }

            turns += 1;
            emit(AgentEvent::TurnStarted { turn: turns }).await?;

            let mut definitions = self.tools.definitions();
            if !self.backend.supports_vision() {
                definitions.retain(|definition| definition.function.name != "read_image");
            }
            let mut projected_messages = history.clone();
            if let Some(runtime) = &mut context {
                for _ in 0..64 {
                    let step = runtime
                        .prepare_step(&self.backend, &history, &definitions, cancellation.clone())
                        .await?;
                    projected_messages = step.messages;
                    if let Some(checkpoint) = step.checkpoint {
                        emit(AgentEvent::ContextCheckpoint { checkpoint }).await?;
                    }
                    if !step.needs_more {
                        break;
                    }
                }
            }

            let model_request = ChatRequest {
                messages: model_messages(&projected_messages, self.backend.supports_vision()),
                tools: definitions,
                temperature: request.temperature,
                max_tokens: request.max_tokens,
                reasoning_effort: request.reasoning_effort,
            };
            let local_estimate =
                crate::context::estimate_tokens(&model_request.messages, &model_request.tools);
            let estimated_input_tokens = context.as_ref().map_or(local_estimate, |runtime| {
                runtime.calibrated_estimate(local_estimate)
            });
            emit(AgentEvent::ModelRequestPrepared {
                estimated_input_tokens,
            })
            .await?;
            let completion = self
                .collect_stream(model_request, cancellation.clone(), emit)
                .await?;
            let actual_input_tokens = completion
                .usage
                .as_ref()
                .and_then(|usage| usage.prompt_tokens);
            if let Some(runtime) = &mut context {
                runtime.observe_usage(local_estimate, actual_input_tokens);
            }
            emit(AgentEvent::ModelUsageObserved {
                local_estimate,
                estimated_input_tokens,
                actual_input_tokens,
            })
            .await?;
            check_finish_reason(completion.finish_reason.as_deref())?;

            let assistant = completion.message;
            let calls = match &assistant {
                ChatMessage::Assistant { tool_calls, .. } => tool_calls.clone(),
                _ => {
                    return Err(AgentError::Model(ModelError::Protocol(
                        "model returned a non-assistant message".to_owned(),
                    )));
                }
            };
            validate_tool_ids(&calls)?;
            if self.config.max_tool_calls > 0
                && tool_calls.saturating_add(calls.len()) > self.config.max_tool_calls
            {
                return Err(AgentError::MaxToolCalls);
            }

            history.push(assistant.clone());
            emit(AgentEvent::MessageCommitted {
                message: assistant.clone(),
            })
            .await?;

            if calls.is_empty() {
                return Ok(AgentResult {
                    final_message: assistant,
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
                emit(AgentEvent::ToolStarted { call: call.clone() }).await?;

                let result =
                    if call.function.name == "read_image" && !self.backend.supports_vision() {
                        Err(crate::tool::ToolError::ExecutionFailed(
                            "current model does not support image input".into(),
                        ))
                    } else {
                        self.tools
                            .execute_output(call.clone(), cancellation.clone())
                            .await
                    };
                let (content, attachments, event) = match result {
                    Ok(output) => (
                        output.content.clone(),
                        output.attachments,
                        AgentEvent::ToolFinished {
                            call: call.clone(),
                            content: output.content,
                        },
                    ),
                    Err(error) => {
                        if cancellation.is_cancelled() {
                            return Err(AgentError::Cancelled);
                        }

                        let message = error.to_string();
                        (
                            format!("tool error: {message}"),
                            Vec::new(),
                            AgentEvent::ToolFailed {
                                call: call.clone(),
                                error: message,
                            },
                        )
                    }
                };
                let message = ChatMessage::tool(call.id, content);
                history.push(message.clone());
                emit(AgentEvent::MessageCommitted { message }).await?;
                emit(event).await?;
                loaded_attachments.extend(attachments);
            }
            if !loaded_attachments.is_empty() {
                let names = loaded_attachments
                    .iter()
                    .map(|attachment| attachment.name())
                    .collect::<Vec<_>>()
                    .join(", ");
                let message = ChatMessage::user_with_attachments(
                    format!("Images loaded by read_image: {names}. Analyze their visual content."),
                    loaded_attachments,
                );
                history.push(message.clone());
                emit(AgentEvent::MessageCommitted { message }).await?;
            }
        }
    }

    async fn collect_stream<F, Fut>(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
        emit: &mut F,
    ) -> Result<ChatCompletion, AgentError>
    where
        F: FnMut(AgentEvent) -> Fut,
        Fut: Future<Output = Result<(), AgentError>>,
    {
        let fallback_request = request.clone();
        let mut stream = self.backend.stream(request, cancellation.clone()).await?;
        let mut text = String::new();
        let mut calls = Vec::new();
        let mut finish_reason = None;
        let mut usage = None;

        while let Some(event) = stream.next().await {
            let event = event.map_err(AgentError::Model)?;
            emit(AgentEvent::Model(event.clone())).await?;

            match event {
                StreamEvent::TextDelta(delta) => text.push_str(&delta),
                StreamEvent::ReasoningDelta(_) => {}
                StreamEvent::ToolCallDelta(delta) => merge_tool_call(&mut calls, delta),
                StreamEvent::Completed {
                    finish_reason: reason,
                    usage: event_usage,
                } => {
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
            emit_completion(&completion, emit).await?;
            return Ok(completion);
        }

        Ok(ChatCompletion {
            id: None,
            model: None,
            message: ChatMessage::Assistant {
                content: (!text.is_empty()).then_some(text),
                tool_calls: calls,
            },
            finish_reason,
            usage,
        })
    }
}

/// Adapt the provider request without changing persisted conversation history.
pub fn model_messages(history: &[ChatMessage], supports_vision: bool) -> Vec<ChatMessage> {
    let mut projected = history.to_vec();
    let mut remaining = if supports_vision { 4 } else { 0 };
    for message in projected.iter_mut().rev() {
        let ChatMessage::User {
            content,
            attachments,
        } = message
        else {
            continue;
        };
        let omitted = attachments.len().saturating_sub(remaining);
        for attachment in attachments.drain(..omitted) {
            let reason = if supports_vision {
                "earlier image removed from this request to limit context"
            } else {
                "current model does not support image input"
            };
            content.push_str(&format!(
                "\n[Image omitted: {reason}: {}]",
                attachment.name()
            ));
        }
        remaining = remaining.saturating_sub(attachments.len());
    }
    projected
}

async fn emit_completion<F, Fut>(
    completion: &ChatCompletion,
    emit: &mut F,
) -> Result<(), AgentError>
where
    F: FnMut(AgentEvent) -> Fut,
    Fut: Future<Output = Result<(), AgentError>>,
{
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

    Ok(())
}

fn check_finish_reason(reason: Option<&str>) -> Result<(), AgentError> {
    if let Some(reason) =
        reason.filter(|reason| matches!(*reason, "max_tokens" | "length" | "incomplete"))
    {
        return Err(AgentError::Model(ModelError::Protocol(format!(
            "model output was incomplete (finish_reason={reason})"
        ))));
    }

    Ok(())
}

fn validate_tool_ids(calls: &[ToolCall]) -> Result<(), AgentError> {
    let mut ids = HashSet::new();
    for call in calls {
        if call.id.trim().is_empty() || !ids.insert(&call.id) {
            return Err(AgentError::Model(ModelError::Protocol(
                "model returned empty or duplicate tool call IDs; no tools were executed"
                    .to_owned(),
            )));
        }
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

fn merge_tool_call(calls: &mut Vec<ToolCall>, delta: ToolCallDelta) {
    while calls.len() <= delta.index {
        calls.push(ToolCall {
            id: String::new(),
            r#type: "function".to_owned(),
            function: FunctionCall {
                name: String::new(),
                arguments: String::new(),
            },
        });
    }

    let call = &mut calls[delta.index];
    if let Some(id) = delta.id {
        call.id = id;
    }
    if let Some(name) = delta.name {
        call.function.name.push_str(&name);
    }
    call.function.arguments.push_str(&delta.arguments);
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
        model::{CompletionFuture, MockModel, StreamFuture},
        tool::{ClosureTool, Tool, ToolError, ToolFuture, ToolOutput, ToolOutputFuture},
    };
    use orbit_model::types::{Attachment, FunctionCall, ToolCall};
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

    fn assistant_tool_call(id: &str, name: &str, arguments: &str) -> ChatCompletion {
        ChatCompletion {
            id: None,
            model: Some("mock".to_owned()),
            message: ChatMessage::Assistant {
                content: None,
                tool_calls: vec![ToolCall {
                    id: id.to_owned(),
                    r#type: "function".to_owned(),
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

    struct VisionModel(MockModel);

    impl ModelBackend for VisionModel {
        fn supports_vision(&self) -> bool {
            true
        }

        fn complete<'a>(
            &'a self,
            request: ChatRequest,
            cancellation: CancellationToken,
        ) -> CompletionFuture<'a> {
            self.0.complete(request, cancellation)
        }

        fn stream<'a>(
            &'a self,
            request: ChatRequest,
            cancellation: CancellationToken,
        ) -> StreamFuture<'a> {
            self.0.stream(request, cancellation)
        }
    }

    struct ImageTool;

    impl Tool for ImageTool {
        fn definition(&self) -> orbit_model::types::ToolDefinition {
            orbit_model::types::ToolDefinition::function(
                "read_image",
                "Load an image",
                json!({ "type": "object" }),
            )
        }

        fn execute<'a>(
            &'a self,
            call: ToolCall,
            cancellation: CancellationToken,
        ) -> ToolFuture<'a> {
            Box::pin(async move {
                self.execute_output(call, cancellation)
                    .await
                    .map(|output| output.content)
            })
        }

        fn execute_output<'a>(
            &'a self,
            _call: ToolCall,
            _cancellation: CancellationToken,
        ) -> ToolOutputFuture<'a> {
            Box::pin(async {
                Ok(ToolOutput::with_attachments(
                    json!({ "loaded": true }).to_string(),
                    vec![Attachment::Image {
                        name: "scene.png".into(),
                        mime_type: "image/png".into(),
                        path: "/unused/mock.png".into(),
                        sha256: "digest".into(),
                    }],
                ))
            })
        }
    }

    #[tokio::test]
    async fn read_image_is_projected_after_tool_results_and_retained_in_history() {
        let model = MockModel::new([
            assistant_tool_call("image-1", "read_image", "{}"),
            assistant_text("I see a scene"),
        ]);
        let mut tools = ToolRegistry::new();
        tools.register(ImageTool);
        let agent = Agent::new(VisionModel(model.clone()), tools, AgentConfig::default());

        let result = agent
            .run(request(), CancellationToken::new())
            .await
            .unwrap();
        let requests = model.requests();

        assert_eq!(requests.len(), 2);
        assert!(
            requests[0]
                .tools
                .iter()
                .any(|tool| tool.function.name == "read_image")
        );
        assert!(matches!(
            &requests[1].messages[3],
            ChatMessage::User { attachments, .. } if attachments.len() == 1
        ));
        assert_eq!(
            result.history,
            requests[1]
                .messages
                .iter()
                .cloned()
                .chain([result.final_message])
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn text_only_model_never_executes_read_image() {
        let model = MockModel::new([
            assistant_tool_call("image-1", "read_image", "{}"),
            assistant_text("image unavailable"),
        ]);
        let mut tools = ToolRegistry::new();
        tools.register(ImageTool);
        let agent = Agent::new(model.clone(), tools, AgentConfig::default());

        let result = agent
            .run(request(), CancellationToken::new())
            .await
            .unwrap();

        assert!(
            !model.requests()[0]
                .tools
                .iter()
                .any(|tool| tool.function.name == "read_image")
        );
        assert!(
            matches!(&result.history[2], ChatMessage::Tool { content, .. } if content.contains("does not support image input"))
        );
        assert!(!result.history.iter().any(|message| {
            matches!(message, ChatMessage::User { attachments, .. } if !attachments.is_empty())
        }));
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
                "properties": { "value": { "type": "string" } }
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
            id: "call-1".to_owned(),
            r#type: "function".to_owned(),
            function: FunctionCall {
                name: "missing".to_owned(),
                arguments: "{}".to_owned(),
            },
        };
        let second = ToolCall {
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
        let agent = Agent::new(
            model,
            ToolRegistry::new(),
            AgentConfig {
                max_turns: 4,
                max_tool_calls: 1,
            },
        );

        let mut events = Vec::new();

        let error = agent
            .run_with_agent_events(request(), CancellationToken::new(), |event| {
                events.push(event);
                std::future::ready(Ok(()))
            })
            .await
            .expect_err("tool budget must stop the loop");

        assert!(matches!(error, AgentError::MaxToolCalls));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::ToolStarted { .. }))
        );
    }

    #[derive(Clone)]
    struct ScriptedStreamModel {
        events: Vec<StreamEvent>,
    }

    impl ModelBackend for ScriptedStreamModel {
        fn complete<'a>(
            &'a self,
            _request: ChatRequest,
            _cancellation: CancellationToken,
        ) -> crate::model::CompletionFuture<'a> {
            Box::pin(async {
                Err(ModelError::Protocol(
                    "unexpected non-streaming fallback".to_owned(),
                ))
            })
        }

        fn stream<'a>(
            &'a self,
            _request: ChatRequest,
            _cancellation: CancellationToken,
        ) -> crate::model::StreamFuture<'a> {
            let events = self.events.clone();
            Box::pin(async move {
                Ok(
                    Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
                        as orbit_model::types::ModelStream,
                )
            })
        }
    }

    #[tokio::test]
    async fn reasoning_and_usage_events_do_not_change_the_final_answer() {
        let model = ScriptedStreamModel {
            events: vec![
                StreamEvent::ReasoningDelta("private reasoning".to_owned()),
                StreamEvent::TextDelta("answer".to_owned()),
                StreamEvent::Completed {
                    finish_reason: Some("stop".to_owned()),
                    usage: None,
                },
                StreamEvent::Completed {
                    finish_reason: None,
                    usage: Some(Usage {
                        prompt_tokens: Some(12),
                        completion_tokens: Some(3),
                        total_tokens: Some(15),
                    }),
                },
            ],
        };
        let agent = Agent::new(model, ToolRegistry::new(), AgentConfig::default());
        let mut events = Vec::new();

        let result = agent
            .run_with_agent_events(request(), CancellationToken::new(), |event| {
                events.push(event);
                std::future::ready(Ok(()))
            })
            .await
            .expect("streamed agent completes");

        assert_eq!(result.final_message, ChatMessage::assistant("answer"));
        assert!(matches!(
            events.first(),
            Some(AgentEvent::TurnStarted { turn: 1 })
        ));
        assert!(matches!(
            events.get(2),
            Some(AgentEvent::Model(StreamEvent::ReasoningDelta(text))) if text == "private reasoning"
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ModelUsageObserved {
                actual_input_tokens: Some(12),
                ..
            }
        )));
        assert!(matches!(
            events.last(),
            Some(AgentEvent::Completed {
                turns: 1,
                tool_calls: 0
            })
        ));
    }

    #[tokio::test]
    async fn plain_and_event_runs_execute_the_same_tool_loop() {
        let responses = [
            assistant_tool_call("call-1", "missing", "{}"),
            assistant_text("recovered"),
        ];
        let plain_model = MockModel::new(responses.clone());
        let event_model = MockModel::new(responses);
        let plain_agent = Agent::new(
            plain_model.clone(),
            ToolRegistry::new(),
            AgentConfig::default(),
        );
        let event_agent = Agent::new(
            event_model.clone(),
            ToolRegistry::new(),
            AgentConfig::default(),
        );
        let mut events = Vec::new();

        let plain = plain_agent
            .run(request(), CancellationToken::new())
            .await
            .unwrap();
        let streamed = event_agent
            .run_with_agent_events(request(), CancellationToken::new(), |event| {
                events.push(event);
                std::future::ready(Ok(()))
            })
            .await
            .unwrap();

        assert_eq!(plain.history, streamed.history);
        assert_eq!(plain_model.requests(), event_model.requests());
        let committed = events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::MessageCommitted { message } => Some(message.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(committed, streamed.history[1..]);
        assert!(matches!(
            events.as_slice(),
            [
                AgentEvent::TurnStarted { turn: 1 },
                AgentEvent::ModelRequestPrepared { .. },
                AgentEvent::Model(_),
                AgentEvent::Model(_),
                AgentEvent::ModelUsageObserved { .. },
                AgentEvent::MessageCommitted { .. },
                AgentEvent::ToolStarted { .. },
                AgentEvent::MessageCommitted { .. },
                AgentEvent::ToolFailed { .. },
                AgentEvent::TurnStarted { turn: 2 },
                AgentEvent::ModelRequestPrepared { .. },
                AgentEvent::Model(_),
                AgentEvent::Model(_),
                AgentEvent::ModelUsageObserved { .. },
                AgentEvent::MessageCommitted { .. },
                AgentEvent::Completed {
                    turns: 2,
                    tool_calls: 1
                }
            ]
        ));
    }

    #[tokio::test]
    async fn incomplete_response_never_executes_its_tool_calls() {
        let mut incomplete = assistant_tool_call("call-1", "echo", "{}");
        incomplete.finish_reason = Some("length".to_owned());
        let model = MockModel::new([incomplete]);
        let executions = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = executions.clone();
        let mut tools = ToolRegistry::new();
        tools.register(ClosureTool::new(
            "echo",
            "Count executions",
            json!({ "type": "object" }),
            move |_arguments, _cancellation| {
                let observed = observed.clone();
                async move {
                    observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok("done".to_owned())
                }
            },
        ));
        let agent = Agent::new(model, tools, AgentConfig::default());

        let error = agent
            .run(request(), CancellationToken::new())
            .await
            .expect_err("truncated tool call must fail");

        assert!(matches!(error, AgentError::Model(ModelError::Protocol(_))));
        assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 0);
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
    fn text_only_projection_omits_images_without_changing_canonical_history() {
        let original = vec![ChatMessage::user_with_attachments(
            "Describe the scene",
            vec![Attachment::Image {
                name: "scene.png".into(),
                mime_type: "image/png".into(),
                path: "/unread/provider-only.png".into(),
                sha256: "digest".into(),
            }],
        )];

        let text_only = model_messages(&original, false);
        let vision = model_messages(&original, true);

        assert!(matches!(
            &text_only[0],
            ChatMessage::User { content, attachments }
                if attachments.is_empty() && content.contains("scene.png")
        ));
        assert_eq!(vision, original);
        assert_eq!(
            serde_json::to_value(&original).unwrap()[0]["attachments"][0]["name"],
            "scene.png"
        );
    }

    #[test]
    fn vision_projection_keeps_only_the_four_most_recent_images() {
        let history = (0..6)
            .map(|index| {
                ChatMessage::user_with_attachments(
                    format!("image {index}"),
                    vec![Attachment::Image {
                        name: format!("{index}.png"),
                        mime_type: "image/png".into(),
                        path: format!("/unread/{index}.png").into(),
                        sha256: "digest".into(),
                    }],
                )
            })
            .collect::<Vec<_>>();

        let projected = model_messages(&history, true);
        let retained = projected
            .iter()
            .filter_map(|message| match message {
                ChatMessage::User { attachments, .. } => Some(attachments.len()),
                _ => None,
            })
            .sum::<usize>();

        assert_eq!(retained, 4);
        assert!(
            matches!(&projected[0], ChatMessage::User { content, attachments }
            if attachments.is_empty() && content.contains("0.png"))
        );
        assert_eq!(history.len(), 6);
    }
}
