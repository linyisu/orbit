use std::{path::PathBuf, time::Duration};

use orbit_agent::{
    background::BackgroundJobs,
    tool::{Tool, ToolError, ToolFuture, ToolOutcome, ToolOutput},
};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::{
    RuntimeError,
    execution::{ExecutionEnvironment, ExecutionPolicy, ExecutionRequest, execution_backend},
    filesystem::Workspace,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::approval::{
    ApprovalPreview, ApprovalRequest, ApprovalSender, CommandPreview, CommandWorkspaceAccess,
};
use crate::limits::DEFAULT_FILE_SCAN_BYTES;

#[derive(Clone)]
pub struct RunCommand {
    workspace: Workspace,
    approvals: ApprovalSender,
    limits: CommandLimits,
    output_store: Option<orbit_runtime::execution::output::CommandOutputs>,
    jobs: Option<BackgroundJobs>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    command: String,
    #[serde(default)]
    background: bool,
    #[serde(default = "offline_network")]
    network: orbit_runtime::capability::NetworkPolicy,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    workspace_access: CommandWorkspaceAccess,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

fn offline_network() -> orbit_runtime::capability::NetworkPolicy {
    orbit_runtime::capability::NetworkPolicy::Deny
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandLimits {
    pub network: orbit_runtime::capability::NetworkPolicy,
    pub max_scan_mib: usize,
    pub max_scan_entries: usize,
    pub process_limits: ProcessLimits,
    pub read_file_scan_bytes: u64,
    pub verification_commands: std::collections::BTreeMap<String, String>,
    pub default_timeout_seconds: u64,
    pub max_timeout_seconds: u64,
}

pub use orbit_runtime::execution::{ProcessLimits, WorkspaceScanLimits};

impl From<&CommandLimits> for WorkspaceScanLimits {
    fn from(limits: &CommandLimits) -> Self {
        Self {
            max_scan_mib: limits.max_scan_mib,
            max_scan_entries: limits.max_scan_entries,
        }
    }
}

impl From<CommandLimits> for WorkspaceScanLimits {
    fn from(limits: CommandLimits) -> Self {
        Self::from(&limits)
    }
}

impl Default for CommandLimits {
    fn default() -> Self {
        let scan_limits = WorkspaceScanLimits::default();

        Self {
            network: orbit_runtime::capability::NetworkPolicy::Allow,
            max_scan_mib: scan_limits.max_scan_mib,
            max_scan_entries: scan_limits.max_scan_entries,
            process_limits: ProcessLimits::default(),
            read_file_scan_bytes: DEFAULT_FILE_SCAN_BYTES,
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

        if !(1..=1_024).contains(&self.max_scan_mib)
            || !(1..=100_000).contains(&self.max_scan_entries)
        {
            return Err("max_scan_mib must be 1..1024 and max_scan_entries 1..100000");
        }

        self.process_limits.validate()?;
        if self.verification_commands.len() > 16
            || self
                .verification_commands
                .iter()
                .any(|(name, cmd)| {
                    name.is_empty()
                        || name.len() > 64
                        || cmd.trim().is_empty()
                        || cmd.len() > 4_096
                        || cmd.contains('\0')
                })
        {
            return Err(
                "at most 16 named verification commands; commands must be nonempty and at most \
                    4096 bytes",
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
            output_store: None,
            jobs: None,
        }
    }

    pub fn with_background_jobs(mut self, jobs: BackgroundJobs) -> Self {
        self.jobs = Some(jobs);
        self
    }

    pub fn with_output_store(
        mut self,
        output_store: orbit_runtime::execution::output::CommandOutputs,
    ) -> Self {
        self.output_store = Some(output_store);
        self
    }
}

impl Tool for RunCommand {
    fn capabilities(&self) -> orbit_agent::tool::ToolCapabilities {
        orbit_agent::tool::ToolCapabilities::COMMAND
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "Exec",
            format!(
                "Run builds, tests, Git, local processing or operations that need a shell. Prefer \
                    WebSearch/WebFetch for ordinary public web research, EditFile for \
                    localized text edits and WriteFile for new files. Scripts remain appropriate \
                    for bulk transformations or requests needing authentication/custom headers \
                    that WebFetch cannot provide. Uses the target user's shell, HOME, \
                    environment and real working directory, without a filesystem or network \
                    sandbox. workspace_access and network declare intended effects for approval, \
                    not OS restrictions. Auto approves all commands started in the workspace, \
                    including networked scripts; Ask requests confirmation; Yolo approves \
                    automatically. Approval does not confine a command to its starting directory. \
                    Save generated files in the project or a user-specified location; these \
                    outputs belong to the user. Timeout, cancellation and resource/output limits \
                    apply. background=true returns a job_id; use WaitCommands when idle and \
                    CommandOutput for retained stdout/stderr. Logs survive task completion \
                    and restart within this session, subject to bounded retention and eviction. \
                    Jobs do not survive a service restart. \
                    Verification commands: {}.",
                serde_json::to_string(&self.limits.verification_commands).unwrap_or_default()
            ),
            json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    },
                    "background": {
                        "type": "boolean",
                        "default": false,
                        "description": "Run independently and immediately return a job_id; \
                            WaitCommands sleeps without model requests and completions are \
                            delivered automatically"
                    },
                    "network": {
                        "type": "string",
                        "enum": ["deny", "allow"],
                        "default": "deny",
                        "description": "Request network access explicitly; cannot exceed the \
                            configured policy"
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Optional working directory for the command. Relative paths \
                            are resolved against the workspace root. Defaults to the workspace \
                            root."
                    },
                    "workspace_access": {
                        "type": "string",
                        "enum": ["read", "read_write"],
                        "default": "read",
                        "description": "Declared workspace access for approval, not an OS \
                            restriction. Defaults to 'read'. Specify 'read_write' if the command \
                            needs to modify files in the workspace (such as cargo build, npm \
                            install, code generation)."
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": self.limits.max_timeout_seconds,
                        "default": self.limits.default_timeout_seconds,
                        "description": "Execution timeout in seconds"
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_str(&call.function.arguments)
                .map_err(|e| invalid(e.to_string()))?;
            self.limits
                .validate()
                .map_err(invalid)?;
            if args.background && self.jobs.is_none() {
                return Err(invalid(
                    "background commands are unavailable for this adapter",
                ));
            }

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

            let requested_cwd = match args.cwd {
                Some(ref dir) => {
                    let dir_path = PathBuf::from(dir);
                    if dir_path.is_absolute() {
                        dir_path
                    } else {
                        self.workspace.root().join(dir_path)
                    }
                }

                None => self.workspace.root().to_path_buf(),
            };

            let execution_env = ExecutionEnvironment::new(self.workspace.root(), &requested_cwd)
                .map_err(|e| {
                    invalid(format!(
                        "invalid working directory '{}': {e}",
                        requested_cwd.display()
                    ))
                })?;

            let workspace_access = args.workspace_access;
            if args.network.is_allowed()
                && (!self
                    .workspace
                    .policy()
                    .network_policy()
                    .is_allowed()
                    || !self.limits.network.is_allowed())
            {
                return Err(invalid("network access is disabled by the command policy"));
            }

            let network_allowed = args.network.is_allowed();
            let mut external_capabilities = Vec::new();
            let mut external_write = false;

            for r in &self
                .workspace
                .policy()
                .effective_policy()
                .persistent_read
            {
                external_capabilities.push(format!("{} (ro, persistent)", r.canonical().display()));
            }

            for rw in &self
                .workspace
                .policy()
                .effective_policy()
                .persistent_read_write
            {
                external_write = true;
                external_capabilities
                    .push(format!("{} (rw, persistent)", rw.canonical().display()));
            }

            if let Ok(store) = self
                .workspace
                .policy()
                .session_grants()
                .read()
            {
                for grant in store.grants() {
                    if !grant.environment.is_local() {
                        continue;
                    }
                    external_write |= grant.write;
                    let mode = if grant.write { "rw" } else { "ro" };

                    external_capabilities.push(format!(
                        "{} ({mode}, session)",
                        grant.path.canonical().display()
                    ));
                }
            }

            let git_analysis = orbit_runtime::git::GitCommandAnalysis::parse(&args.command);
            let request = ApprovalRequest {
                preview: ApprovalPreview::Command(CommandPreview {
                    command: args.command.clone(),
                    cwd: args
                        .cwd
                        .clone()
                        .filter(|s| !s.is_empty()),
                    workspace_access,
                    timeout_seconds,
                    network_allowed,
                    external_capabilities,
                    external_write,
                    git: git_analysis.clone(),
                }),
                decision: sender,
            };
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                result = self.approvals.send(request) => result.map_err(|_| {
                    ToolError::ExecutionFailed("command approval unavailable".into())
                })?,
            }
            let approved = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                answer = receiver => answer.unwrap_or(false),
            };

            if !approved {
                return Err(ToolError::PermissionDenied(
                    "user denied command; do not retry without a new user request".into(),
                ));
            }

            let (output_log, retention_error) = match &self.output_store {
                Some(outputs) => match outputs.create_log().await {
                    Ok(log) => (Some(log), None),
                    Err(error) => (
                        None,
                        Some(format!("command output retention unavailable: {error}")),
                    ),
                },
                None => (None, None),
            };

            if let Some(jobs) = &self.jobs
                && let Some(log) = &output_log
            {
                jobs.track_output(&call.id, log.id());
            }

            let pre_scan = match orbit_runtime::execution::scan_workspace_summary(
                self.workspace.clone(),
                cancellation.clone(),
                self.limits.clone().into(),
            )
            .await
            {
                Ok(summary) => Ok(summary),
                Err(RuntimeError::Cancelled) => return Err(ToolError::Cancelled),
                Err(error) => Err(error),
            };

            let (fingerprint, workspace_files, skipped_entries, mut verification_error) =
                match pre_scan {
                    Ok((f, wf, se)) => (Some(f), wf, se, None),
                    Err(error) => (
                        None,
                        0,
                        0,
                        Some(format!("input workspace fingerprint unavailable: {error}")),
                    ),
                };

            let backend = execution_backend();
            let request =
                ExecutionRequest::new(args.command.clone(), Duration::from_secs(timeout_seconds))
                    .with_verification(fingerprint, workspace_files, skipped_entries);

            let execution_policy = ExecutionPolicy {
                resources: self
                    .limits
                    .process_limits
                    .clone()
                    .into(),
                ..Default::default()
            };

            let workspace = self.workspace.clone();
            let scan_limits = self.limits.clone().into();
            let output_id = output_log
                .as_ref()
                .map(|log| log.id().to_owned());
            let command = request.command.clone();
            let background = args.background;
            let output_store = self.output_store.clone();
            let execute = move |token: CancellationToken| async move {
                let mut result = backend
                    .execute(
                        &request,
                        &execution_policy,
                        &execution_env,
                        output_log,
                        token.clone(),
                    )
                    .await?;

                if let Some(error) = retention_error {
                    result.retained_output_truncated = true;
                    result.retained_output_error = Some(error);
                }

                let post_scan =
                    orbit_runtime::execution::scan_workspace_summary(workspace, token, scan_limits)
                        .await;

                let post_fingerprint = match post_scan {
                    Ok((fingerprint, _, _)) => Some(fingerprint),
                    Err(error) => {
                        let warning = format!("output workspace fingerprint unavailable: {error}");

                        if let Some(existing) = &mut verification_error {
                            existing.push_str("; ");
                            existing.push_str(&warning);
                        } else {
                            verification_error = Some(warning);
                        }

                        None
                    }
                };

                if let Some(evidence) = result.verification.as_mut() {
                    if background
                        && post_fingerprint
                            .as_ref()
                            .is_some_and(|output| output.digest != evidence.input.digest)
                    {
                        let warning = "workspace changed while the background command was running; \
                            this result does not verify the current workspace";
                        if let Some(existing) = &mut verification_error {
                            existing.push_str("; ");
                            existing.push_str(warning);
                        } else {
                            verification_error = Some(warning.into());
                        }

                        evidence.output = None;
                    } else {
                        evidence.output = post_fingerprint;
                    }
                }

                result.verification_error = verification_error;
                let evidence = result.verification.take();
                if let Some(outputs) = output_store
                    && let Err(error) = outputs.record_result(&result).await
                {
                    let message = format!("capture receipt unavailable: {error}");
                    match &mut result.retained_output_error {
                        Some(previous) => {
                            previous.push_str("; ");
                            previous.push_str(&message);
                        }
                        None => result.retained_output_error = Some(message),
                    }
                }
                let outcome = if result.cancelled {
                    ToolOutcome::Cancelled
                } else if result.is_success() {
                    ToolOutcome::Completed
                } else {
                    ToolOutcome::Failed
                };

                serde_json::to_string(&result)
                    .map(|content| {
                        let mut output = ToolOutput::text(content).with_outcome(outcome);
                        output.verification = evidence;
                        output
                    })
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))
            };

            if args.background {
                let job = self.jobs.as_ref().unwrap().start(
                    call,
                    command,
                    output_id,
                    cancellation,
                    execute,
                )?;
                let mut output = ToolOutput::text(json!({
                    "job_id": job.job_id,
                    "output_id": job.output_id,
                    "background": true,
                    "running": true,
                    "status": job.status,
                    "message": "Command started in the background. Completion will be delivered \
                        automatically; use WaitCommands when there is no independent work."
                }).to_string());

                output.background_job = Some(job.job_id);
                Ok(output)
            } else {
                execute(cancellation).await
            }
        })
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "Exec".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;

    #[test]
    fn command_configuration_validates_scan_and_process_budgets() {
        let limits: CommandLimits = toml::from_str(
            "max_scan_mib=128\nmax_scan_entries=20000\n[veri\
            fication_commands]\ntest='cargo test --offline'\n",
        )
        .unwrap();

        assert!(limits.validate().is_ok());
        assert_eq!(limits.max_scan_mib, 128);
        assert_eq!(limits.max_scan_entries, 20_000);

        let offline: CommandLimits = toml::from_str("network='deny'").unwrap();

        assert_eq!(
            offline.network,
            orbit_runtime::capability::NetworkPolicy::Deny
        );

        let mut bad = limits.clone();
        bad.max_scan_mib = 0;

        assert!(bad.validate().is_err());

        let mut bad = limits.clone();
        // < 16 MiB
        bad.process_limits.max_memory_bytes = 1_024;

        assert!(bad.validate().is_err());
    }
}
