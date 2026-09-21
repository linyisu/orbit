use futures_util::StreamExt;
use orbit_model::types::{
    ChatCompletion, ChatMessage, ChatRequest, FunctionCall, StreamEvent, ToolCall, ToolCallDelta,
};
use tokio_util::sync::CancellationToken;

use crate::{error::AgentError, model::ModelBackend, tool::ToolRegistry};

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

            let assistant = completion.message.clone();
            let calls = match &assistant {
                ChatMessage::Assistant { tool_calls, .. } => tool_calls.clone(),
                _ => Vec::new(),
            };
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
        let mut stream = self.backend.stream(request, cancellation).await?;
        let mut text = String::new();
        let mut calls = Vec::new();
        let mut finish_reason = None;
        let mut usage = None;

        while let Some(event) = stream.next().await {
            let event = event.map_err(AgentError::Model)?;
            on_event(event.clone());

            match event {
                StreamEvent::TextDelta(delta) | StreamEvent::ReasoningDelta(delta) => {
                    text.push_str(&delta);
                }
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

        let error = agent
            .run(request(), CancellationToken::new())
            .await
            .expect_err("tool budget must stop the loop");

        assert!(matches!(error, AgentError::MaxToolCalls));
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
