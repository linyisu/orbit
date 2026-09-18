use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};

pub type ScratchDirectory = Arc<RwLock<PathBuf>>;
pub type CommandOutputDirectory = crate::read_command_output::CommandOutputDirectory;

use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::{
    approval::{ApprovalPreview, ApprovalRequest, ApprovalSender, CommandPreview},
    sandbox,
    workspace::Workspace,
};

pub struct RunCommand {
    workspace: Workspace,
    approvals: ApprovalSender,
    limits: CommandLimits,
    scratch: Option<ScratchDirectory>,
    output_directory: Option<CommandOutputDirectory>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    command: String,
    #[serde(default)]
    writable_snapshot: bool,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandLimits {
    pub rust_toolchain: Option<std::path::PathBuf>,
    pub default_timeout_seconds: u64,
    pub max_timeout_seconds: u64,
}

impl Default for CommandLimits {
    fn default() -> Self {
        Self {
            rust_toolchain: None,
            default_timeout_seconds: 120,
            max_timeout_seconds: 600,
        }
    }
}

impl CommandLimits {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.default_timeout_seconds == 0
            || self.default_timeout_seconds > self.max_timeout_seconds
        {
            return Err(
                "command timeout defaults must be positive and not exceed max_timeout_seconds",
            );
        }
        if self
            .rust_toolchain
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
        {
            return Err("command.rust_toolchain must be an absolute toolchain directory");
        }
        Ok(())
    }
}

impl RunCommand {
    pub fn new(workspace: Workspace, approvals: ApprovalSender) -> Self {
        Self::with_limits(workspace, approvals, CommandLimits::default())
    }

    pub fn with_limits(
        workspace: Workspace,
        approvals: ApprovalSender,
        limits: CommandLimits,
    ) -> Self {
        Self {
            workspace,
            approvals,
            limits,
            scratch: None,
            output_directory: None,
        }
    }
    pub fn with_scratch(mut self, scratch: ScratchDirectory) -> Self {
        self.scratch = Some(scratch);
        self
    }
    pub fn with_output_directory(mut self, output_directory: CommandOutputDirectory) -> Self {
        self.output_directory = Some(output_directory);
        self
    }
}

impl Tool for RunCommand {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "run_command",
            "Run a /bin/sh command under the user-selected approval policy in a Linux Bubblewrap sandbox. Use read_image instead when the task is to view or describe an image; shell metadata, OCR, pixel dumps and generated statistics do not expose the actual picture to the vision model. No network, credentials or host home. /workspace is a filtered snapshot, read-only by default. Set writable_snapshot=true to build or test in a disposable writable copy (128 MiB total including source). Changes inside /workspace are discarded after each command; copy sources or build outputs to /scratch when they must survive subsequent commands. /tmp is a disposable 64 MiB tmpfs and each produced file is limited to 64 MiB. In the TUI, /scratch is a private writable directory retained across commands and session resume. Save intermediate files, binaries and checkpoints there; new sessions use separate scratch directories. It does not keep background processes alive. Cannot modify the original project. Metadata, symlinks, target/node_modules/.venv/__pycache__ are omitted. Snapshot limit 64 MiB/10000 entries. System tools are available; a host-configured Rust toolchain may also be mounted read-only at /opt/orbit-rust and added to PATH. Cargo dependencies are not inherited and network remains blocked. Timeout defaults and maximum are specified in the tool schema; timeout and cancellation stop the sandbox process, while CPU time has no separate limit. Command text is executed from a private script file, without an 8 KiB or OS argument-size limit. stdout/stderr each return the last 32 KiB with total byte counts. In the TUI, full output is privately retained by session and returns an output_id for read_command_output pagination; it is capped at 16 MiB per stream and 128 MiB per session. Never automatically retry a denied command.",
            json!({"type":"object","properties":{"writable_snapshot":{"type":"boolean","default":false},"command":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":self.limits.max_timeout_seconds,"default":self.limits.default_timeout_seconds}},"required":["command"],"additionalProperties":false}),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_str(&call.function.arguments)
                .map_err(|e| invalid(e.to_string()))?;
            self.limits.validate().map_err(invalid)?;
            let timeout_seconds = args
                .timeout_seconds
                .unwrap_or(self.limits.default_timeout_seconds);
            if args.command.trim().is_empty()
                || args.command.contains('\0')
                || !(1..=self.limits.max_timeout_seconds).contains(&timeout_seconds)
            {
                return Err(invalid(format!(
                    "command must be nonempty and contain no NUL; timeout_seconds must be 1..{}",
                    self.limits.max_timeout_seconds
                )));
            }
            let (sender, receiver) = oneshot::channel();
            let request = ApprovalRequest {
                preview: ApprovalPreview::Command(CommandPreview {
                    command: args.command.clone(),
                    writable_snapshot: args.writable_snapshot,
                    timeout_seconds,
                }),
                decision: sender,
            };
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                result = self.approvals.send(request) => result.map_err(|_| ToolError::ExecutionFailed("command approval unavailable".into()))?,
            }
            let approved = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                answer = receiver => answer.unwrap_or(false),
            };
            if !approved {
                return Err(ToolError::ExecutionFailed(
                    "user denied command; do not retry without a new user request".into(),
                ));
            }
            let scratch = self
                .scratch
                .as_ref()
                .map(|directory| directory.read().expect("scratch lock poisoned").clone());
            let output_log = match &self.output_directory {
                Some(directory) => {
                    let directory = directory
                        .read()
                        .expect("command output lock poisoned")
                        .clone();
                    Some(sandbox::OutputLog::create(&directory).await?)
                }
                None => None,
            };
            let result = sandbox::run_with_resources_and_output(
                self.workspace.clone(),
                args.command,
                Duration::from_secs(timeout_seconds),
                cancellation,
                args.writable_snapshot,
                self.limits.rust_toolchain.as_deref(),
                scratch.as_deref(),
                output_log,
            )
            .await?;
            serde_json::to_string(&result)
                .map(ToolOutput::text)
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
        })
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "run_command".into(),
        message: message.into(),
    }
}
