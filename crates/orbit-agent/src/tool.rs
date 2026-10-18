use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

use orbit_model::types::{Attachment, ToolCall, ToolDefinition};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

mod capabilities;
pub use capabilities::{ToolCapabilities, ToolEffect};

pub type ToolFuture<'a> = Pin<Box<dyn Future<Output = Result<ToolOutput, ToolError>> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolOutput {
    pub content: String,
    /// Host-owned lifecycle evidence, independent of the model-facing content.
    pub outcome: ToolOutcome,
    /// Confirmed file access, recorded only after the operation succeeds.
    pub files: crate::memory::FileActivity,
    pub attachments: Vec<Attachment>,
    /// Local audit data for a successfully committed workspace write. This is
    /// intentionally excluded from the model-facing tool result.
    pub changes: Vec<FileChange>,
    pub verification: Option<orbit_runtime::verification::VerificationEvidence>,
    pub background_job: Option<String>,
}

/// Terminal outcome reported by a tool that returns a receipt rather than an error.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    #[default]
    Completed,
    Failed,
    Cancelled,
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
            outcome: ToolOutcome::Completed,
            files: Default::default(),
            attachments: Vec::new(),
            changes: Vec::new(),
            verification: None,
            background_job: None,
        }
    }

    pub fn with_attachments(content: impl Into<String>, attachments: Vec<Attachment>) -> Self {
        Self {
            content: content.into(),
            outcome: ToolOutcome::Completed,
            files: Default::default(),
            attachments,
            changes: Vec::new(),
            verification: None,
            background_job: None,
        }
    }

    pub fn with_changes(content: impl Into<String>, changes: Vec<FileChange>) -> Self {
        let files = crate::memory::FileActivity {
            read: Default::default(),
            written: changes
                .iter()
                .map(|change| change.path.clone())
                .collect(),
        };
        Self {
            content: content.into(),
            outcome: ToolOutcome::Completed,
            files,
            attachments: Vec::new(),
            changes,
            verification: None,
            background_job: None,
        }
    }

    pub fn with_outcome(mut self, outcome: ToolOutcome) -> Self {
        self.outcome = outcome;
        self
    }

    pub fn with_read(mut self, path: impl Into<String>) -> Self {
        self.files.read.insert(path.into());
        self
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
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("authorization required: {0}")]
    AuthorizationRequired(String),
    #[error("policy denied: {0}")]
    PolicyDenied(String),
    #[error("input changed: {0}")]
    Conflict(String),
    #[error("dependency unavailable: {0}")]
    DependencyUnavailable(String),
    #[error("tool execution was cancelled")]
    Cancelled,
}

impl ToolError {
    /// Preserve typed failures through service orchestration; unknown errors stay unclassified.
    pub fn from_anyhow(error: anyhow::Error) -> Self {
        match error.downcast::<Self>() {
            Ok(error) => error,
            Err(error) => match error.downcast::<orbit_runtime::RuntimeError>() {
                Ok(error) => error.into(),
                Err(error) => Self::ExecutionFailed(format!("{error:#}")),
            },
        }
    }

    /// Stable model-facing failure envelope. Recovery never authorizes an automatic replay.
    pub fn model_content(&self) -> String {
        const MAX_ERROR_MESSAGE_BYTES: usize = 4_096;
        let mut message = self.to_string();
        let truncated = message.len() > MAX_ERROR_MESSAGE_BYTES;
        if truncated {
            let mut end = MAX_ERROR_MESSAGE_BYTES;
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
        }
        let (code, recovery) = match self {
            Self::EmptyCallId | Self::EmptyToolName | Self::InvalidArguments { .. } => {
                ("invalid_arguments", "correct_arguments")
            }
            Self::UnknownTool(_) => ("unknown_tool", "select_available_tool"),
            Self::PermissionDenied(_) => ("permission_denied", "wait_for_new_user_request"),
            Self::AuthorizationRequired(_) => {
                ("authorization_required", "request_user_authorization")
            }
            Self::PolicyDenied(_) => ("policy_denied", "choose_permitted_operation"),
            Self::Conflict(_) => ("conflict", "refresh_input"),
            Self::DependencyUnavailable(_) => ("dependency_unavailable", "configure_dependency"),
            Self::ExecutionFailed(_) => ("execution_failed", "inspect_failure"),
            Self::Cancelled => ("cancelled", "stop"),
        };
        serde_json::json!({
            "error": {
                "code": code,
                "message": message,
                "message_truncated": truncated,
                "recovery": recovery,
                "retryable": false
            }
        })
        .to_string()
    }
}

impl From<orbit_runtime::filesystem::WorkspaceError> for ToolError {
    fn from(error: orbit_runtime::filesystem::WorkspaceError) -> Self {
        use orbit_runtime::filesystem::WorkspaceError;
        match error {
            WorkspaceError::PermissionDenied(message) => Self::AuthorizationRequired(message),
            _ => Self::ExecutionFailed(error.to_string()),
        }
    }
}

impl From<orbit_runtime::RuntimeError> for ToolError {
    fn from(err: orbit_runtime::RuntimeError) -> Self {
        match err {
            orbit_runtime::RuntimeError::Cancelled => ToolError::Cancelled,
            orbit_runtime::RuntimeError::ExecutionFailed(msg) => ToolError::ExecutionFailed(msg),
            orbit_runtime::RuntimeError::DependencyUnavailable { program, message } => {
                ToolError::DependencyUnavailable(format!("{program}: {message}"))
            }
            orbit_runtime::RuntimeError::Workspace(err) => err.into(),
            orbit_runtime::RuntimeError::Io(err) => ToolError::ExecutionFailed(err.to_string()),
        }
    }
}

#[derive(Clone, Copy)]
pub struct ToolContext {
    /// Whether image attachments can be delivered to the current model.
    pub supports_vision: bool,
}

/// Builds the model contract from the same typed arguments used by the executor.
/// Runtime validation still owns cross-field constraints and resource authorization.
pub fn typed_definition<T: schemars::JsonSchema>(name: &str, description: &str) -> ToolDefinition {
    let mut settings = schemars::generate::SchemaSettings::draft07();
    settings.inline_subschemas = true;
    let mut parameters = settings
        .into_generator()
        .into_root_schema_for::<T>()
        .to_value();
    if let Some(schema) = parameters.as_object_mut() {
        schema.remove("$schema");
        schema.remove("title");
    }
    ToolDefinition::function(name, description, parameters)
}

/// A tool validates its arguments and reports failures without panicking.
/// Cancellation must not claim that an already committed operation was undone.
/// The registry applies policy before calling this interface; direct callers own authorization.
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;

    fn origin(&self) -> ToolOrigin {
        ToolOrigin::Builtin
    }

    /// Unknown tools are exclusive and unavailable in read-only modes.
    fn capabilities(&self) -> ToolCapabilities {
        ToolCapabilities::UNKNOWN
    }

    /// Refine effects when an action's arguments select different capabilities.
    fn call_capabilities(&self, _call: &ToolCall) -> ToolCapabilities {
        self.capabilities()
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a>;

    fn execute_with_context<'a>(
        &'a self,
        call: ToolCall,
        cancellation: CancellationToken,
        _context: ToolContext,
    ) -> ToolFuture<'a> {
        self.execute(call, cancellation)
    }
}

/// Host-owned catalog membership, independent of model-facing names and effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolOrigin {
    Builtin,
    Mcp,
}

/// A server-owned policy rechecked immediately before every execution.
/// The returned lease lives until the tool finishes, including cancellation.
pub trait ToolPolicy: Send + Sync {
    fn context(&self) -> Option<String> {
        None
    }

    fn visible(&self, capabilities: ToolCapabilities) -> bool;
    fn acquire(
        &self,
        call: &ToolCall,
        capabilities: ToolCapabilities,
    ) -> Result<Box<dyn Send>, ToolError>;
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
        self.policy
            .as_ref()
            .and_then(|p| p.context())
    }

    pub fn set_policy(&mut self, policy: Arc<dyn ToolPolicy>) {
        self.policy = Some(policy);
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    pub fn retain(&mut self, predicate: impl Fn(ToolCapabilities) -> bool) {
        Arc::make_mut(&mut self.tools).retain(|_, tool| predicate(tool.capabilities()));
    }

    pub fn call_capabilities(&self, call: &ToolCall) -> ToolCapabilities {
        self.tools
            .get(&call.function.name)
            .map_or(ToolCapabilities::UNKNOWN, |tool| {
                tool.call_capabilities(call)
            })
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

    pub fn remove_origin(&mut self, origin: ToolOrigin) {
        Arc::make_mut(&mut self.tools).retain(|_, tool| tool.origin() != origin);
    }

    /// Replaces a catalog without modifying snapshots held by running agents.
    pub fn replace_origin(&mut self, origin: ToolOrigin, catalog: &Self) {
        let tools = Arc::make_mut(&mut self.tools);
        tools.retain(|_, tool| tool.origin() != origin);
        tools.extend(
            catalog
                .tools
                .iter()
                .filter(|(_, tool)| tool.origin() == origin)
                .map(|(name, tool)| (name.clone(), tool.clone())),
        );
    }

    /// Replace one namespaced catalog without disturbing unrelated tools or
    /// existing registry snapshots held by in-flight runs.
    pub fn replace_prefix(&mut self, prefix: &str, catalog: &Self) {
        let tools = Arc::make_mut(&mut self.tools);
        tools.retain(|name, _| !name.starts_with(prefix));
        tools.extend(
            catalog
                .tools
                .iter()
                .filter(|(name, _)| name.starts_with(prefix))
                .map(|(name, tool)| (name.clone(), tool.clone())),
        );
    }

    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions = self
            .tools
            .values()
            .filter(|tool| {
                self.policy
                    .as_ref()
                    .is_none_or(|p| p.visible(tool.capabilities()))
            })
            .map(|tool| {
                let mut definition = tool.definition();
                orbit_model::tool_purpose::advertise(&mut definition);
                definition
            })
            .collect::<Vec<_>>();

        definitions.sort_by(|left, right| {
            left.function
                .name
                .cmp(&right.function.name)
        });

        definitions
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Separates model intent only for tools whose schema advertises this extension.
    pub fn normalize_call(&self, call: &mut ToolCall) {
        if let Some(tool) = self.tools.get(&call.function.name)
            && orbit_model::tool_purpose::advertise(&mut tool.definition())
        {
            orbit_model::tool_purpose::extract(call);
        }
    }

    pub fn execute(&self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'static> {
        self.execute_with_context(
            call,
            cancellation,
            ToolContext {
                supports_vision: true,
            },
        )
    }

    pub fn execute_with_context(
        &self,
        mut call: ToolCall,
        cancellation: CancellationToken,
        context: ToolContext,
    ) -> ToolFuture<'static> {
        self.normalize_call(&mut call);
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

        let Some(tool) = self
            .tools
            .get(&call.function.name)
            .cloned()
        else {
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

            let capabilities = tool.call_capabilities(&call);
            let _lease = policy
                .as_ref()
                .map(|p| p.acquire(&call, capabilities))
                .transpose()?;
            tool.execute_with_context(call, cancellation, context)
                .await
        })
    }
}

pub struct ClosureTool<F> {
    definition: ToolDefinition,
    capabilities: ToolCapabilities,
    origin: ToolOrigin,
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
            capabilities: ToolCapabilities::UNKNOWN,
            origin: ToolOrigin::Builtin,
            execute,
        }
    }

    pub fn with_capabilities(mut self, capabilities: ToolCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    pub fn with_origin(mut self, origin: ToolOrigin) -> Self {
        self.origin = origin;
        self
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

    fn capabilities(&self) -> ToolCapabilities {
        self.capabilities
    }

    fn origin(&self) -> ToolOrigin {
        self.origin
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let arguments = call
                .arguments_json()
                .map_err(|error| ToolError::InvalidArguments {
                    tool: call.function.name.clone(),
                    message: error.to_string(),
                })?;
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
            purpose: None,
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
            json!({
                "type": "object"
            }),
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
        let receipt: serde_json::Value = serde_json::from_str(&error.model_content()).unwrap();
        assert_eq!(receipt["error"]["code"], "unknown_tool");
        assert_eq!(receipt["error"]["retryable"], false);
    }

    #[test]
    fn orchestration_context_preserves_typed_cancellation_and_policy_failures() {
        let error = anyhow::Error::new(ToolError::Cancelled).context("plan review interrupted");
        assert_eq!(ToolError::from_anyhow(error), ToolError::Cancelled);
        let error = ToolError::PolicyDenied("plan mode requires read-only tools".into());
        let receipt: serde_json::Value = serde_json::from_str(&error.model_content()).unwrap();
        assert_eq!(receipt["error"]["code"], "policy_denied");
        assert_eq!(receipt["error"]["recovery"], "choose_permitted_operation");
        let denied = ToolError::PermissionDenied("拒绝".repeat(2_000));
        let receipt: serde_json::Value = serde_json::from_str(&denied.model_content()).unwrap();
        assert_eq!(receipt["error"]["recovery"], "wait_for_new_user_request");
        assert_eq!(receipt["error"]["message_truncated"], true);
        assert!(
            receipt["error"]["message"]
                .as_str()
                .unwrap()
                .len()
                <= 4_096
        );
    }

    #[test]
    fn removes_named_and_namespaced_tools_from_definitions() {
        let mut registry = registry();
        registry.register(ClosureTool::new(
            "MCP_Files_Search",
            "search",
            json!({}),
            |_, _| async { Ok("ok".into()) },
        ));
        registry.register(ClosureTool::new(
            "MCP_Files_Read",
            "read",
            json!({}),
            |_, _| async { Ok("ok".into()) },
        ));

        assert!(registry.remove("echo"));
        assert!(!registry.contains("echo"));
        assert_eq!(registry.remove_prefix("MCP_Files_"), 2);
        assert!(registry.definitions().is_empty());
    }

    #[tokio::test]
    async fn rejects_invalid_arguments_before_execution() {
        let error = registry()
            .execute(call("call-1", "echo", "not-json"), CancellationToken::new())
            .await
            .expect_err("invalid arguments must fail");

        assert!(matches!(
            error,
            ToolError::InvalidArguments { tool, .. } if tool == "echo"
        ));
    }

    #[tokio::test]
    async fn directly_called_closure_returns_argument_errors_without_invoking_work() {
        let tool = ClosureTool::new(
            "inspect",
            "inspect a generated value",
            json!({}),
            |_, _| async { panic!("invalid arguments must not reach the implementation") },
        );
        let error = tool
            .execute(call("direct", "inspect", "{"), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::InvalidArguments { tool, .. } if tool == "inspect"));
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
