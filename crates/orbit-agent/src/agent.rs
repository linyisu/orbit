use std::collections::{BTreeMap, HashSet};

use futures_util::StreamExt;
use orbit_model::types::{
    ChatCompletion, ChatMessage, ChatRequest, FunctionCall, StreamEvent, ToolCall, ToolCallDelta,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{error::AgentError, events::AgentEvent, model::ModelBackend, tool::ToolRegistry};

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub max_turns: usize,
    pub max_tool_calls: usize,
    pub context_tokens: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_turns: 0,
            max_tool_calls: 0,
            context_tokens: 128_000,
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
        }
    }

    pub async fn run(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> Result<AgentResult, AgentError> {
        self.run_internal(request, cancellation, None).await
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
        self.run_internal(request, cancellation, Some(&on_event))
            .await
    }

    pub async fn run_with_agent_events(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
        sender: mpsc::Sender<AgentEvent>,
    ) -> Result<AgentResult, AgentError> {
        let result = self
            .run_agent_events(request, cancellation.clone(), &sender)
            .await;

        match &result {
            Ok(agent_result) => {
                send_agent_event(
                    &sender,
                    AgentEvent::Completed {
                        turns: agent_result.turns,
                        tool_calls: agent_result.tool_calls,
                    },
                )
                .await?;
            }
            Err(AgentError::Cancelled) => {
                send_agent_event(&sender, AgentEvent::Cancelled).await?;
            }
            Err(error) => {
                send_agent_event(
                    &sender,
                    AgentEvent::Failed {
                        error: error.to_string(),
                    },
                )
                .await?;
            }
        }

        result
    }

    async fn run_agent_events(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
        sender: &mpsc::Sender<AgentEvent>,
    ) -> Result<AgentResult, AgentError> {
        if request.messages.is_empty() {
            return Err(AgentError::InvalidInitialRequest);
        }

        let mut history = request.messages;
        let mut turns = 0;
        let mut tool_calls = 0;

        loop {
            if cancellation.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            if self.config.max_turns > 0 && turns >= self.config.max_turns {
                return Err(AgentError::MaxTurns);
            }

            crate::context::check_budget(
                &history,
                &self.tools.definitions(),
                request.max_tokens.unwrap_or(4096) as usize,
                self.config.context_tokens,
            )?;
            turns += 1;
            send_agent_event(sender, AgentEvent::TurnStarted { turn: turns }).await?;

            let completion = self
                .collect_stream_with_agent_events(
                    ChatRequest {
                        messages: history.clone(),
                        tools: self.tools.definitions(),
                        temperature: request.temperature,
                        max_tokens: request.max_tokens,
                    },
                    cancellation.clone(),
                    sender,
                )
                .await?;
            check_finish_reason(completion.finish_reason.as_deref())?;
            let assistant = completion.message.clone();
            let calls = match &assistant {
                ChatMessage::Assistant { tool_calls, .. } => tool_calls.clone(),
                _ => Vec::new(),
            };
            validate_tool_ids(&calls)?;
            history.push(assistant);

            if calls.is_empty() {
                return Ok(AgentResult {
                    final_message: completion.message,
                    history,
                    turns,
                    tool_calls,
                });
            }

            for call in calls {
                if cancellation.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }
                if self.config.max_tool_calls > 0 && tool_calls >= self.config.max_tool_calls {
                    return Err(AgentError::MaxToolCalls);
                }

                tool_calls += 1;
                send_agent_event(sender, AgentEvent::ToolStarted { call: call.clone() }).await?;
                match self.tools.execute(call.clone(), cancellation.clone()).await {
                    Ok(content) => {
                        send_agent_event(
                            sender,
                            AgentEvent::ToolFinished {
                                call: call.clone(),
                                content: content.clone(),
                            },
                        )
                        .await?;
                        history.push(ChatMessage::tool(call.id, content));
                    }
                    Err(error) => {
                        let message = error.to_string();
                        send_agent_event(
                            sender,
                            AgentEvent::ToolFailed {
                                call: call.clone(),
                                error: message.clone(),
                            },
                        )
                        .await?;
                        history.push(ChatMessage::tool(call.id, format!("tool error: {message}")));
                    }
                }
            }
        }
    }

    async fn collect_stream_with_agent_events(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
        sender: &mpsc::Sender<AgentEvent>,
    ) -> Result<ChatCompletion, AgentError> {
        let fallback_request = request.clone();
        let mut stream = self.backend.stream(request, cancellation.clone()).await?;
        let mut text = String::new();
        let mut calls = BTreeMap::new();
        let mut finish_reason = None;
        let mut usage = None;

        while let Some(event) = stream.next().await {
            let event = event.map_err(AgentError::Model)?;
            send_agent_event(sender, AgentEvent::Model(event.clone())).await?;
            match event {
                StreamEvent::TextDelta(delta) => text.push_str(&delta),
                StreamEvent::ReasoningDelta(_) => {}
                StreamEvent::ToolCallDelta(delta) => merge_tool_call(&mut calls, delta),
                StreamEvent::Completed {
                    finish_reason: reason,
                    usage: event_usage,
                } => {
                    finish_reason = reason;
                    usage = event_usage;
                }
            }
        }

        check_finish_reason(finish_reason.as_deref())?;
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
                    send_agent_event(
                        sender,
                        AgentEvent::Model(StreamEvent::TextDelta(content.clone())),
                    )
                    .await?;
                }
                for (index, call) in tool_calls.iter().enumerate() {
                    send_agent_event(
                        sender,
                        AgentEvent::Model(StreamEvent::ToolCallDelta(ToolCallDelta {
                            index,
                            id: Some(call.id.clone()),
                            name: Some(call.function.name.clone()),
                            arguments: call.function.arguments.clone(),
                        })),
                    )
                    .await?;
                }
                send_agent_event(
                    sender,
                    AgentEvent::Model(StreamEvent::Completed {
                        finish_reason: completion.finish_reason.clone(),
                        usage: completion.usage.clone(),
                    }),
                )
                .await?;
            }

            return Ok(completion);
        }

        Ok(ChatCompletion {
            id: None,
            model: None,
            message: ChatMessage::Assistant {
                content: (!text.is_empty()).then_some(text),
                tool_calls: calls.into_values().collect(),
            },
            finish_reason,
            usage,
        })
    }

    async fn run_internal(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
        on_event: Option<&dyn Fn(StreamEvent)>,
    ) -> Result<AgentResult, AgentError> {
        if request.messages.is_empty() {
            return Err(AgentError::InvalidInitialRequest);
        }

        let mut history = request.messages;
        let mut turns = 0;
        let mut tool_calls = 0;

        loop {
            if cancellation.is_cancelled() {
                return Err(AgentError::Cancelled);
            }

            if self.config.max_turns > 0 && turns >= self.config.max_turns {
                return Err(AgentError::MaxTurns);
            }

            crate::context::check_budget(
                &history,
                &self.tools.definitions(),
                request.max_tokens.unwrap_or(4096) as usize,
                self.config.context_tokens,
            )?;
            turns += 1;
            let model_request = ChatRequest {
                messages: history.clone(),
                tools: self.tools.definitions(),
                temperature: request.temperature,
                max_tokens: request.max_tokens,
            };
            let completion = match on_event {
                Some(on_event) => {
                    self.collect_stream(model_request, cancellation.clone(), on_event)
                        .await?
                }
                None => {
                    self.backend
                        .complete(model_request, cancellation.clone())
                        .await?
                }
            };

            check_finish_reason(completion.finish_reason.as_deref())?;
            let assistant = completion.message.clone();
            let calls = match &assistant {
                ChatMessage::Assistant { tool_calls, .. } => tool_calls.clone(),
                _ => Vec::new(),
            };
            validate_tool_ids(&calls)?;
            history.push(assistant.clone());

            if calls.is_empty() {
                return Ok(AgentResult {
                    final_message: assistant,
                    history,
                    turns,
                    tool_calls,
                });
            }

            for call in calls {
                if cancellation.is_cancelled() {
                    return Err(AgentError::Cancelled);
                }

                if self.config.max_tool_calls > 0 && tool_calls >= self.config.max_tool_calls {
                    return Err(AgentError::MaxToolCalls);
                }

                tool_calls += 1;
                let result = self.tools.execute(call.clone(), cancellation.clone()).await;
                let content = match result {
                    Ok(content) => content,
                    Err(error) => format!("tool error: {error}"),
                };
                history.push(ChatMessage::tool(call.id, content));
            }
        }
    }

    async fn collect_stream(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
        on_event: &dyn Fn(StreamEvent),
    ) -> Result<ChatCompletion, AgentError> {
        let fallback_request = request.clone();
        let mut stream = self.backend.stream(request, cancellation.clone()).await?;
        let mut text = String::new();
        let mut calls = BTreeMap::new();
        let mut finish_reason = None;
        let mut usage = None;

        while let Some(event) = stream.next().await {
            let event = event.map_err(AgentError::Model)?;
            on_event(event.clone());

            match event {
                StreamEvent::TextDelta(delta) => text.push_str(&delta),
                StreamEvent::ReasoningDelta(_) => {}
                StreamEvent::ToolCallDelta(delta) => merge_tool_call(&mut calls, delta),
                StreamEvent::Completed {
                    finish_reason: reason,
                    usage: event_usage,
                } => {
                    finish_reason = reason;
                    usage = event_usage;
                }
            }
        }

        check_finish_reason(finish_reason.as_deref())?;
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
                    on_event(StreamEvent::TextDelta(content.clone()));
                }
                for (index, call) in tool_calls.iter().enumerate() {
                    on_event(StreamEvent::ToolCallDelta(ToolCallDelta {
                        index,
                        id: Some(call.id.clone()),
                        name: Some(call.function.name.clone()),
                        arguments: call.function.arguments.clone(),
                    }));
                }
                on_event(StreamEvent::Completed {
                    finish_reason: completion.finish_reason.clone(),
                    usage: completion.usage.clone(),
                });
            }

            return Ok(completion);
        }

        Ok(ChatCompletion {
            id: None,
            model: None,
            message: ChatMessage::Assistant {
                content: (!text.is_empty()).then_some(text),
                tool_calls: calls.into_values().collect(),
            },
            finish_reason,
            usage,
        })
    }
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
        tool::{ClosureTool, ToolError},
    };
    use orbit_model::types::{FunctionCall, ToolCall};
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
                ToolCall::function("first", "echo", json!({"text":"one"})),
                ToolCall::function("second", "echo", json!({"text":"two"})),
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
                json!({"type":"object"}),
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
            json!({"type": "object"}),
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
            id: "call-1".to_owned(),
            kind: "function".to_owned(),
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
                ..AgentConfig::default()
            },
        );

        let error = agent
            .run(request(), CancellationToken::new())
            .await
            .expect_err("tool budget must stop the loop");

        assert!(matches!(error, AgentError::MaxToolCalls));
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
}
