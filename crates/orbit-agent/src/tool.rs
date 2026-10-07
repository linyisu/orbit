use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use orbit_model::types::{Attachment, ToolCall, ToolDefinition};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutput {
    pub content: String,
    pub attachments: Vec<Attachment>,
    /// Local audit data for a successfully committed workspace write. This is
    /// intentionally excluded from the model-facing tool result.
    pub changes: Vec<FileChange>,
    pub verification: Option<orbit_runtime::verification::VerificationEvidence>,
    pub background_job: Option<String>,
}

/// A reversible text-file mutation reported only after the filesystem commit
/// succeeded. `before` is `None` for a newly created file.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
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
            verification: None,
            background_job: None,
        }
    }

    pub fn with_attachments(content: impl Into<String>, attachments: Vec<Attachment>) -> Self {
        Self {
            content: content.into(),
            attachments,
            changes: Vec::new(),
            verification: None,
            background_job: None,
        }
    }

    pub fn with_changes(content: impl Into<String>, changes: Vec<FileChange>) -> Self {
        Self {
            content: content.into(),
            attachments: Vec::new(),
            changes,
            verification: None,
            background_job: None,
        }
    }
}

impl std::ops::Deref for ToolOutput {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.content
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

impl From<orbit_runtime::RuntimeError> for ToolError {
    fn from(err: orbit_runtime::RuntimeError) -> Self {
        match err {
            orbit_runtime::RuntimeError::Cancelled => ToolError::Cancelled,
            orbit_runtime::RuntimeError::ExecutionFailed(msg) => ToolError::ExecutionFailed(msg),
            orbit_runtime::RuntimeError::Workspace(err) => {
                ToolError::ExecutionFailed(err.to_string())
            }
            orbit_runtime::RuntimeError::Io(err) => ToolError::ExecutionFailed(err.to_string()),
        }
    }
}

pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a>;
}

/// A server-owned policy rechecked immediately before every execution.
/// The returned lease lives until the tool finishes, including cancellation.
pub trait ToolPolicy: Send + Sync {
    fn context(&self) -> Option<String> {
        None
    }
    fn visible(&self, name: &str) -> bool;
    fn acquire(&self, call: &ToolCall) -> Result<Box<dyn Send>, ToolError>;
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Arc<HashMap<String, Arc<dyn Tool>>>,
    policy: Option<Arc<dyn ToolPolicy>>,
    background: crate::background::BackgroundJobs,
    additional_tasks: Vec<crate::background::BackgroundJobs>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn policy_context(&self) -> Option<String> {
        self.policy.as_ref().and_then(|p| p.context())
    }

    pub fn set_policy(&mut self, policy: Arc<dyn ToolPolicy>) {
        self.policy = Some(policy);
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    pub fn policy(&self) -> Option<Arc<dyn ToolPolicy>> {
        self.policy.clone()
    }

    pub fn background_jobs(&self) -> &crate::background::BackgroundJobs {
        &self.background
    }

    pub fn add_task_group(&mut self, tasks: crate::background::BackgroundJobs) {
        self.additional_tasks.push(tasks);
    }

    pub fn task_groups(&self) -> impl Iterator<Item = &crate::background::BackgroundJobs> {
        std::iter::once(&self.background).chain(self.additional_tasks.iter())
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

    pub fn remove(&mut self, name: &str) -> bool {
        let mut tools = (*self.tools).clone();
        let removed = tools.remove(name).is_some();

        if removed {
            self.tools = Arc::new(tools);
        }

        removed
    }

    pub fn remove_prefix(&mut self, prefix: &str) -> usize {
        let mut tools = (*self.tools).clone();
        let before = tools.len();
        tools.retain(|name, _| !name.starts_with(prefix));
        let removed = before.saturating_sub(tools.len());

        if removed != 0 {
            self.tools = Arc::new(tools);
        }

        removed
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self
            .tools
            .values()
            .map(|tool| tool.definition())
            .filter(|d| {
                self.policy
                    .as_ref()
                    .is_none_or(|p| p.visible(&d.function.name))
            })
            .collect::<Vec<_>>();

        definitions.sort_by(|left, right| left.function.name.cmp(&right.function.name));

        definitions
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn execute(&self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'static> {
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

        let policy = self.policy.clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }

            let _lease = policy.as_ref().map(|p| p.acquire(&call)).transpose()?;
            tool.execute(call, cancellation).await
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

        Box::pin(async move {
            (self.execute)(arguments, cancellation)
                .await
                .map(ToolOutput::text)
        })
    }
}

#[cfg(test)]
mod tests {
    use orbit_model::types::FunctionCall;
    use serde_json::json;

    use super::*;

    fn call(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            metadata: None,
            id: id.to_owned(),
            kind: "function".to_owned(),
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

        assert_eq!(result, ToolOutput::text(r#"{"value":"hello"}"#));
    }

    #[tokio::test]
    async fn rejects_unknown_tool() {
        let error = registry()
            .execute(call("call-1", "missing", "{}"), CancellationToken::new())
            .await
            .expect_err("unknown tools must fail");

        assert_eq!(error, ToolError::UnknownTool("missing".to_owned()));
    }

    #[test]
    fn removes_named_and_namespaced_tools_from_definitions() {
        let mut registry = registry();
        registry.register(ClosureTool::new(
            "mcp__files__search",
            "search",
            json!({}),
            |_, _| async { Ok("ok".into()) },
        ));
        registry.register(ClosureTool::new(
            "mcp__files__read",
            "read",
            json!({}),
            |_, _| async { Ok("ok".into()) },
        ));

        assert!(registry.remove("echo"));
        assert!(!registry.contains("echo"));
        assert_eq!(registry.remove_prefix("mcp__files__"), 2);
        assert!(registry.definitions().is_empty());
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
