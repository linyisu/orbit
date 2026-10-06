use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
    time::Duration,
};

use orbit_agent::{
    background::BackgroundJobs,
    tool::{Tool, ToolError, ToolFuture, ToolOutput},
};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::{
    RuntimeError,
    execution::{
        ExecutionEnvironment, ExecutionPolicy, ExecutionRequest, OutputLog, execution_backend,
    },
    filesystem::Workspace,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::approval::{
    ApprovalPreview, ApprovalRequest, ApprovalSender, CommandPreview, CommandWorkspaceAccess,
    pinned_local_command,
};
use crate::limits::DEFAULT_FILE_SCAN_BYTES;

pub type ScratchDirectory = Arc<RwLock<PathBuf>>;

pub type CommandOutputDirectory = crate::read_command_output::CommandOutputDirectory;

#[derive(Clone)]
pub struct RunCommand {
    workspace: Workspace,
    approvals: ApprovalSender,
    limits: CommandLimits,
    scratch: Option<ScratchDirectory>,
    output_directory: Option<CommandOutputDirectory>,
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
    writable_snapshot: bool,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

fn offline_network() -> orbit_runtime::capability::NetworkPolicy {
    orbit_runtime::capability::NetworkPolicy::Deny
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandLimits {
    pub sandbox: bool,
    pub network: orbit_runtime::capability::NetworkPolicy,
    pub rust_toolchain: Option<std::path::PathBuf>,
    #[serde(alias = "snapshot_mib")]
    pub max_scan_mib: usize,
    #[serde(alias = "snapshot_entries")]
    pub max_scan_entries: usize,
    #[serde(alias = "writable_snapshot_mib")]
    pub legacy_writable_snapshot_mib: Option<usize>,
    pub process_limits: ProcessLimits,
    pub artifacts: crate::export_artifact::ArtifactLimits,
    pub read_file_scan_bytes: u64,
    pub dependency_caches: Vec<DependencyCache>,
    pub verification_commands: std::collections::BTreeMap<String, String>,
    pub default_timeout_seconds: u64,
    pub max_timeout_seconds: u64,
}

pub use orbit_runtime::execution::{MountAccess, MountSpec, ProcessLimits, WorkspaceScanLimits};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct DependencyCache {
    pub name: String,
    pub path: std::path::PathBuf,
}

impl DependencyCache {
    pub fn to_mount_spec(&self) -> MountSpec {
        MountSpec::read_only(
            &self.path,
            std::path::PathBuf::from(format!("/opt/orbit-cache/{}", self.name)),
        )
    }
}

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
            sandbox: false,
            network: orbit_runtime::capability::NetworkPolicy::Allow,
            rust_toolchain: None,
            max_scan_mib: scan_limits.max_scan_mib,
            max_scan_entries: scan_limits.max_scan_entries,
            legacy_writable_snapshot_mib: None,
            process_limits: ProcessLimits::default(),
            artifacts: Default::default(),
            read_file_scan_bytes: DEFAULT_FILE_SCAN_BYTES,
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

        if !(1..=1024).contains(&self.max_scan_mib)
            || !(1..=100_000).contains(&self.max_scan_entries)
        {
            return Err("max_scan_mib must be 1..1024 and max_scan_entries 1..100000");
        }

        self.process_limits.validate()?;
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
            jobs: None,
        }
    }

    pub fn with_background_jobs(mut self, jobs: BackgroundJobs) -> Self {
        self.jobs = Some(jobs);
        self
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
            if self.limits.sandbox {
                format!(
                    "Run a /bin/sh command inside a secure Linux Bubblewrap sandbox. /workspace is mounted read-only by default ('workspace_access': 'read'); commands cannot modify project files unless 'workspace_access' is explicitly set to 'read_write' (usually requires approval in Auto mode). Literal mkdir, echo, printf, cat and tee commands with workspace-scoped paths, pipelines, > / >> redirection and && sequencing run automatically in Auto mode when offline and without external write access. Shell expansions and other programs still require approval. Native Git commands are available within the requested sandbox permissions. Offline read-only queries may run automatically; writes and network requests follow approval policy. Use git_push for an ordinary HTTPS push requiring host credentials. Protected paths (.env*, orbit config/db) are isolated and masked; Existing repository configuration and hooks are read-only unless an explicitly approved configuration operation requests read_write. Access to paths outside /workspace is denied by default unless granted via request_capability or persistent workspace configuration. Network is denied by default; request network=allow explicitly, subject to the configured policy and approval. Enforces execution timeout, private script execution, and resource limits. When background=true, starts a managed command and returns a job_id after approval; the same timeout and permissions apply. Independent background commands may overlap. Completion is delivered automatically to this session between model requests. Continue useful independent work; when nothing remains, call wait_commands once rather than polling get_command_status. Use cancel_commands to stop jobs. Background jobs do not survive an Orbit restart and are cancelled when the Agent run is stopped. /tmp is a disposable command temporary directory. Persistent scratch is mounted at /scratch when configured; HOME is /scratch/home in that case, or /tmp/home. System tools are available; a host-configured Rust toolchain may be mounted read-only at /opt/orbit-rust and added to PATH. Host-configured dependency caches are mounted read-only at /opt/orbit-cache/<name>. Output is captured with byte counts and retained per session; large outputs return an output_id for pagination via read_command_output. Read-only cache mounts: {}. Host-configured verification commands: {}.",
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
                )
            } else {
                format!(
                    "Run a command with the target user's shell, real working directory, HOME and development environment. There is no filesystem or network sandbox. workspace_access and network declare intended effects for approval, not OS restrictions. Use real host paths; there are no /workspace or /scratch mounts. Session scratch is available through $ORBIT_SCRATCH. Use native Git with the target user's configured SSH or HTTPS authentication. Commands require approval according to the active permission mode. Timeout, cancellation, bounded output and process resource limits apply. background=true returns a managed job_id; use wait_commands when idle and read_command_output for retained output. Jobs do not survive a service restart. Verification commands: {}.",
                    serde_json::to_string(&self.limits.verification_commands).unwrap_or_default()
                )
            },
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The shell command to execute" },
                    "background": { "type": "boolean", "default": false, "description": "Run independently and immediately return a job_id; wait_commands sleeps without model requests and completions are delivered automatically" },
                    "network": { "type": "string", "enum": ["deny", "allow"], "default": "deny", "description": "Request network access explicitly; cannot exceed the configured policy" },
                    "cwd": {
                        "type": "string",
                        "description": "Optional working directory for the command. Relative paths are resolved against the workspace root. Defaults to the workspace root."
                    },
                    "workspace_access": {
                        "type": "string",
                        "enum": ["read", "read_write"],
                        "default": "read",
                        "description": "Workspace access mode. Defaults to 'read'; enforced as a read-only mount only when sandboxing is enabled. Specify 'read_write' if the command needs to modify files in the workspace (such as cargo build, npm install, code generation)."
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
            self.limits.validate().map_err(invalid)?;
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

            if args.writable_snapshot {
                return Err(invalid(
                    "'writable_snapshot' is deprecated. Orbit executes directly in the workspace with protected path masking. For workspace write access, specify 'workspace_access': 'read_write'.",
                ));
            }

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
                && (!self.workspace.policy().network_policy().is_allowed()
                    || !self.limits.network.is_allowed())
            {
                return Err(invalid("network access is disabled by the command policy"));
            }

            let network_allowed = args.network.is_allowed();
            let mut external_capabilities = Vec::new();
            let mut external_write = false;

            for r in &self.workspace.policy().effective_policy().persistent_read {
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

            if let Ok(store) = self.workspace.policy().session_grants().read() {
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

            for cache in &self.limits.dependency_caches {
                external_capabilities
                    .push(format!("{} (ro, dependency cache)", cache.path.display()));
            }

            let git_analysis = orbit_runtime::git::GitCommandAnalysis::parse(&args.command);
            let command =
                pinned_local_command(&args.command).unwrap_or_else(|| args.command.clone());
            let native_workspace_scoped = !self.limits.sandbox
                && crate::approval::native_workspace_command(
                    &args.command,
                    args.cwd.as_deref(),
                    &self.workspace,
                )
                .is_some();

            let request = ApprovalRequest {
                preview: ApprovalPreview::Command(CommandPreview {
                    sandbox: self.limits.sandbox,
                    native_workspace_scoped,
                    command: args.command.clone(),
                    cwd: args.cwd.clone().filter(|s| !s.is_empty()),
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
                    Some(OutputLog::create(&directory).await?)
                }

                None => None,
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

            let backend = execution_backend(self.limits.sandbox)?;
            let mut request = ExecutionRequest::new(
                command,
                execution_env.cwd.clone(),
                Duration::from_secs(timeout_seconds),
            )
            .with_verification(fingerprint, workspace_files, skipped_entries);

            if let Some(s) = scratch.as_ref() {
                request = request.with_scratch(s);
            }

            let effective_policy = self.workspace.policy().effective_policy();
            let mut execution_policy = ExecutionPolicy::dev_default();
            execution_policy.read_only_workspace = workspace_access == CommandWorkspaceAccess::Read;
            execution_policy.network = args.network;
            execution_policy.mounts = self
                .limits
                .dependency_caches
                .iter()
                .map(DependencyCache::to_mount_spec)
                .collect();
            execution_policy.protected_paths = self.workspace.protected().to_vec();
            execution_policy.opaque_runtime_roots = self.workspace.opaque_runtime_roots().to_vec();
            execution_policy.resources = self.limits.process_limits.clone().into();
            execution_policy.rust_toolchain = self.limits.rust_toolchain.clone();
            execution_policy.unmask_git = true;
            execution_policy.allow_git_config_write = workspace_access
                == CommandWorkspaceAccess::ReadWrite
                && orbit_runtime::git::GitCommandAnalysis::parse(&request.command).is_some_and(
                    |g| {
                        matches!(
                            g.kind,
                            orbit_runtime::git::GitOperationKind::ConfigMutation { .. }
                        )
                    },
                );
            execution_policy.read_templates = effective_policy.read_templates.clone();
            execution_policy.write_templates = effective_policy.write_templates.clone();
            execution_policy.external_read = effective_policy
                .persistent_read
                .iter()
                .map(|p| p.canonical().to_path_buf())
                .collect();
            execution_policy.external_read_write = effective_policy
                .persistent_read_write
                .iter()
                .map(|p| p.canonical().to_path_buf())
                .collect();

            if let Ok(store) = self.workspace.policy().session_grants().read() {
                for grant in store.grants() {
                    if !grant.environment.is_local() {
                        continue;
                    }
                    if grant.write {
                        execution_policy
                            .external_read_write
                            .push(grant.path.canonical().to_path_buf());
                    } else {
                        execution_policy
                            .external_read
                            .push(grant.path.canonical().to_path_buf());
                    }
                }
            }

            let workspace = self.workspace.clone();
            let scan_limits = self.limits.clone().into();
            let output_id = output_log.as_ref().map(|log| log.id().to_owned());
            let command = request.command.clone();
            let background = args.background;
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
                        let warning = "workspace changed while the background command was running; this result does not verify the current workspace";
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

                serde_json::to_string(&result)
                    .map(|content| {
                        let mut output = ToolOutput::text(content);
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
                    "message": "Command started in the background. Completion will be delivered automatically; use wait_commands when there is no independent work."
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
        tool: "run_command".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;

    #[test]
    fn configurable_environment_rejects_ambiguous_mounts_and_invalid_budgets() {
        let limits: CommandLimits = toml::from_str("max_scan_mib=128\nmax_scan_entries=20000\ndependency_caches=[{name='cargo',path='/offline/cargo'}]\n[verification_commands]\ntest='cargo test --offline'\n").unwrap();

        assert!(limits.validate().is_ok());
        assert_eq!(limits.max_scan_mib, 128);
        assert_eq!(limits.max_scan_entries, 20000);

        let offline: CommandLimits = toml::from_str("network='deny'").unwrap();

        assert_eq!(
            offline.network,
            orbit_runtime::capability::NetworkPolicy::Deny
        );

        let legacy: CommandLimits =
            toml::from_str("snapshot_mib=256\nsnapshot_entries=30000\nwritable_snapshot_mib=512\n")
                .unwrap();

        assert!(legacy.validate().is_ok());
        assert_eq!(legacy.max_scan_mib, 256);
        assert_eq!(legacy.max_scan_entries, 30000);

        let mut bad = limits.clone();
        bad.max_scan_mib = 0;

        assert!(bad.validate().is_err());

        let mut bad = limits.clone();
        bad.process_limits.max_memory_bytes = 1024; // < 16 MiB

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

    #[test]
    fn tool_schema_omits_writable_snapshot() {
        let (sender, _receiver) = crate::approval::channel();
        let ws_temp = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(ws_temp.path()).unwrap();
        let tool = RunCommand::new(workspace, sender);
        let def = tool.definition();
        let props = def.function.parameters["properties"].as_object().unwrap();

        assert!(props.contains_key("command"));
        assert!(props.contains_key("cwd"));
        assert!(props.contains_key("workspace_access"));
        assert!(props.contains_key("timeout_seconds"));
        assert!(!props.contains_key("writable_snapshot"));
    }
}
