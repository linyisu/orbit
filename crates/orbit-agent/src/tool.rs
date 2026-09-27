use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use orbit_model::types::{Attachment, ToolCall, ToolDefinition};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<String, ToolError>> + Send + 'a>>;
pub type ToolOutputFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutput {
    pub content: String,
    pub attachments: Vec<Attachment>,
    pub changes: Vec<FileChange>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub before: Option<String>,
    pub after: String,
}

impl ToolOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            attachments: Vec::new(),
            changes: Vec::new(),
        }
    }

    pub fn with_attachments(content: impl Into<String>, attachments: Vec<Attachment>) -> Self {
        Self {
            content: content.into(),
            attachments,
            changes: Vec::new(),
        }
    }

    pub fn with_changes(content: impl Into<String>, changes: Vec<FileChange>) -> Self {
        Self {
            content: content.into(),
            attachments: Vec::new(),
            changes,
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ToolError {
    #[error("tool call has an empty id")]
    EmptyCallId,
    #[error("tool call has an empty name")]
    EmptyToolName,
    #[error("unknown tool: {0}")]
    UnknownTool(String),
    #[error("invalid arguments for tool {tool}: {message}")]
    InvalidArguments { tool: String, message: String },
    #[error("tool execution failed: {0}")]
    ExecutionFailed(String),
    #[error("tool execution was cancelled")]
    Cancelled,
}

pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a>;

    fn execute_output<'a>(
        &'a self,
        call: ToolCall,
        cancellation: CancellationToken,
    ) -> ToolOutputFuture<'a> {
        Box::pin(async move { self.execute(call, cancellation).await.map(ToolOutput::text) })
    }
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Arc<HashMap<String, Arc<dyn Tool>>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<T>(&mut self, tool: T)
    where
        T: Tool + 'static,
    {
        let mut tools = (*self.tools).clone();
        let definition = tool.definition();

        tools.insert(definition.function.name.clone(), Arc::new(tool));
        self.tools = Arc::new(tools);
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self
            .tools
            .values()
            .map(|tool| tool.definition())
            .collect::<Vec<_>>();

        definitions.sort_by(|left, right| left.function.name.cmp(&right.function.name));
        definitions
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let mut tools = (*self.tools).clone();
        let removed = tools.remove(name).is_some();
        self.tools = Arc::new(tools);
        removed
    }

    pub fn execute(&self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'static> {
        let future = self.execute_output(call, cancellation);
        Box::pin(async move { future.await.map(|output| output.content) })
    }

    pub fn execute_output(
        &self,
        call: ToolCall,
        cancellation: CancellationToken,
    ) -> ToolOutputFuture<'static> {
        if call.id.trim().is_empty() {
            return Box::pin(async { Err(ToolError::EmptyCallId) });
        }

        if call.function.name.trim().is_empty() {
            return Box::pin(async { Err(ToolError::EmptyToolName) });
        }

        let arguments = match call.arguments_json() {
            Ok(arguments) => arguments,
            Err(error) => {
                let tool = call.function.name.clone();
                return Box::pin(async move {
                    Err(ToolError::InvalidArguments {
                        tool,
                        message: error.to_string(),
                    })
                });
            }
        };

        let Some(tool) = self.tools.get(&call.function.name).cloned() else {
            let name = call.function.name.clone();
            return Box::pin(async move { Err(ToolError::UnknownTool(name)) });
        };

        let call = ToolCall {
            function: orbit_model::types::FunctionCall {
                arguments: arguments.to_string(),
                ..call.function
            },
            ..call
        };

        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }

            tool.execute_output(call, cancellation).await
        })
    }
}

pub struct ClosureTool<F> {
    definition: ToolDefinition,
    execute: F,
}

impl<F, Fut> ClosureTool<F>
where
    F: Fn(serde_json::Value, CancellationToken) -> Fut + Send + Sync,
    Fut: Future<Output = Result<String, ToolError>> + Send + 'static,
{
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
        execute: F,
    ) -> Self {
        Self {
            definition: ToolDefinition::function(name, description, parameters),
            execute,
        }
    }
}

impl<F, Fut> Tool for ClosureTool<F>
where
    F: Fn(serde_json::Value, CancellationToken) -> Fut + Send + Sync,
    Fut: Future<Output = Result<String, ToolError>> + Send + 'static,
{
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        let arguments = call
            .arguments_json()
            .expect("ToolRegistry validates arguments before execution");
        Box::pin((self.execute)(arguments, cancellation))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_model::types::FunctionCall;
    use serde_json::json;

    fn call(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            r#type: "function".to_owned(),
            function: FunctionCall {
                name: name.to_owned(),
                arguments: arguments.to_owned(),
            },
        }
    }

    fn registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.register(ClosureTool::new(
            "echo",
            "Echo a value",
            json!({ "type": "object" }),
            |arguments, _cancellation| async move { Ok(arguments.to_string()) },
        ));
        registry
    }

    #[tokio::test]
    async fn registers_and_executes_a_tool() {
        let registry = registry();

        assert!(registry.contains("echo"));
        assert_eq!(registry.definitions()[0].function.name, "echo");

        let result = registry
            .execute(
                call("call-1", "echo", r#"{"value":"hello"}"#),
                CancellationToken::new(),
            )
            .await
            .expect("tool executes");

        assert_eq!(result, r#"{"value":"hello"}"#);
    }

    #[tokio::test]
    async fn removed_tool_is_absent_from_definitions_and_execution() {
        let mut registry = registry();

        assert!(registry.remove("echo"));
        assert!(!registry.remove("echo"));

        assert!(!registry.contains("echo"));
        assert!(registry.definitions().is_empty());
        let error = registry
            .execute(call("call-1", "echo", "{}"), CancellationToken::new())
            .await
            .unwrap_err();
        assert_eq!(error, ToolError::UnknownTool("echo".into()));
    }

    #[tokio::test]
    async fn rejects_unknown_tool() {
        let error = registry()
            .execute(call("call-1", "missing", "{}"), CancellationToken::new())
            .await
            .expect_err("unknown tools must fail");

        assert_eq!(error, ToolError::UnknownTool("missing".to_owned()));
    }

    #[tokio::test]
    async fn rejects_invalid_arguments_before_execution() {
        let error = registry()
            .execute(call("call-1", "echo", "not-json"), CancellationToken::new())
            .await
            .expect_err("invalid arguments must fail");

        assert!(matches!(error, ToolError::InvalidArguments { tool, .. } if tool == "echo"));
    }

    #[tokio::test]
    async fn rejects_empty_call_id_and_name() {
        let empty_id = registry()
            .execute(call("", "echo", "{}"), CancellationToken::new())
            .await
            .expect_err("empty call IDs must fail");
        assert_eq!(empty_id, ToolError::EmptyCallId);

        let empty_name = registry()
            .execute(call("call-1", "", "{}"), CancellationToken::new())
            .await
            .expect_err("empty tool names must fail");
        assert_eq!(empty_name, ToolError::EmptyToolName);
    }

    #[tokio::test]
    async fn cancellation_is_checked_before_tool_execution() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = registry()
            .execute(call("call-1", "echo", "{}"), cancellation)
            .await
            .expect_err("cancelled tools must fail");

        assert_eq!(error, ToolError::Cancelled);
    }
}
