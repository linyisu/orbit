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
    pub snapshot_mib: usize,
    pub snapshot_entries: usize,
    pub writable_snapshot_mib: usize,
    pub dependency_caches: Vec<DependencyCache>,
    pub verification_commands: std::collections::BTreeMap<String, String>,
    pub default_timeout_seconds: u64,
    pub max_timeout_seconds: u64,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyCache {
    pub name: String,
    pub path: PathBuf,
}

impl Default for CommandLimits {
    fn default() -> Self {
        Self {
            rust_toolchain: None,
            snapshot_mib: 64,
            snapshot_entries: 10_000,
            writable_snapshot_mib: 256,
            dependency_caches: Vec::new(),
            verification_commands: Default::default(),
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
        if !(1..=1024).contains(&self.snapshot_mib)
            || !(1..=100_000).contains(&self.snapshot_entries)
            || self.writable_snapshot_mib < self.snapshot_mib
            || self.writable_snapshot_mib > 2048
        {
            return Err(
                "snapshot_mib must be 1..1024, snapshot_entries 1..100000 and writable_snapshot_mib between snapshot_mib and 2048",
            );
        }
        let mut names = std::collections::BTreeSet::new();
        for cache in &self.dependency_caches {
            if cache.name.is_empty()
                || cache.name.len() > 48
                || !cache
                    .name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
                || !cache.path.is_absolute()
                || !names.insert(&cache.name)
            {
                return Err(
                    "dependency cache names must be unique ASCII letters/digits/-/_ up to 48 bytes, with absolute directory paths",
                );
            }
        }
        if self.dependency_caches.len() > 16
            || self.verification_commands.len() > 16
            || self.verification_commands.iter().any(|(name, cmd)| {
                name.is_empty()
                    || name.len() > 64
                    || cmd.trim().is_empty()
                    || cmd.len() > 4096
                    || cmd.contains('\0')
            })
        {
            return Err(
                "at most 16 caches and 16 named verification commands; commands must be nonempty and at most 4096 bytes",
            );
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
            format!(
                "Run a /bin/sh command under the user-selected approval policy in a Linux Bubblewrap sandbox. Use read_image instead when the task is to view or describe an image; shell metadata, OCR, pixel dumps and generated statistics do not expose the actual picture to the vision model. No network, credentials or host home. /workspace is a filtered snapshot, read-only by default. Set writable_snapshot=true to build or test in a disposable writable copy with host-configured capacity. Changes inside /workspace are discarded after each command; copy sources or build outputs to /scratch when they must survive subsequent commands. /tmp is a disposable 64 MiB tmpfs and each produced file is limited to 64 MiB. In the TUI, /scratch is a private writable directory retained across commands and session resume. Save intermediate files, binaries and checkpoints there; new sessions use separate scratch directories. It does not keep background processes alive. Cannot modify the original project. Metadata, symlinks, target/node_modules/.venv/__pycache__ are omitted. System tools are available; a host-configured Rust toolchain may also be mounted read-only at /opt/orbit-rust and added to PATH. Only explicitly configured dependency caches are mounted; network remains blocked. Timeout defaults and maximum are specified in the tool schema; timeout and cancellation stop the sandbox process, while CPU time has no separate limit. Command text is executed from a private script file, without an 8 KiB or OS argument-size limit. stdout/stderr each return the last 32 KiB with total byte counts. In the TUI, full output is privately retained by session and returns an output_id for read_command_output pagination; it is capped at 16 MiB per stream and 128 MiB per session. Never automatically retry a denied command. Configured source limits: {} MiB / {} entries, disposable writable capacity {} MiB. Read-only cache mounts: {}. Host-configured verification commands, not yet executed: {}.",
                self.limits.snapshot_mib,
                self.limits.snapshot_entries,
                self.limits.writable_snapshot_mib,
                serde_json::to_string(
                    &self
                        .limits
                        .dependency_caches
                        .iter()
                        .map(|c| format!("/opt/orbit-cache/{}", c.name))
                        .collect::<Vec<_>>()
                )
                .unwrap_or_default(),
                serde_json::to_string(&self.limits.verification_commands).unwrap_or_default()
            ),
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
            let mut result = sandbox::run_configured(
                self.workspace.clone(),
                args.command,
                Duration::from_secs(timeout_seconds),
                cancellation,
                args.writable_snapshot,
                &self.limits,
                scratch.as_deref(),
                output_log,
            )
            .await?;
            let evidence = result.verification.take();
            serde_json::to_string(&result)
                .map(|content| {
                    let mut output = ToolOutput::text(content);
                    output.verification = evidence;
                    output
                })
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

#[cfg(test)]
mod configuration_tests {
    use super::*;
    #[test]
    fn configurable_environment_rejects_ambiguous_mounts_and_invalid_budgets() {
        let limits:CommandLimits=toml::from_str("snapshot_mib=128\nsnapshot_entries=20000\nwritable_snapshot_mib=512\ndependency_caches=[{name='cargo',path='/offline/cargo'}]\n[verification_commands]\ntest='cargo test --offline'\n").unwrap();
        assert!(limits.validate().is_ok());
        let mut bad = limits.clone();
        bad.writable_snapshot_mib = 127;
        assert!(bad.validate().is_err());
        let mut bad = limits.clone();
        bad.dependency_caches.push(bad.dependency_caches[0].clone());
        assert!(bad.validate().is_err());
        let mut bad = limits.clone();
        bad.dependency_caches[0].name = "../escape".into();
        assert!(bad.validate().is_err());
        let mut bad = limits;
        bad.dependency_caches[0].path = "relative".into();
        assert!(bad.validate().is_err());
    }
}
