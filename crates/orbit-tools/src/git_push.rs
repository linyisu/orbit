//! Explicit HTTPS push with host credentials confined to a trusted sandbox worker.
use crate::approval::{
    ApprovalPreview, ApprovalRequest, ApprovalSender, CommandPreview, CommandWorkspaceAccess,
};
use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::{
    execution::{
        BwrapBackend, ExecutionBackend, ExecutionEnvironment, ExecutionPolicy, ExecutionRequest,
        MountSpec,
    },
    filesystem::Workspace,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub struct GitPush {
    workspace: Workspace,
    approvals: ApprovalSender,
    process_limits: orbit_runtime::execution::ProcessLimits,
    timeout: Duration,
}
impl GitPush {
    pub fn new(workspace: Workspace, approvals: ApprovalSender) -> Self {
        Self {
            workspace,
            approvals,
            process_limits: Default::default(),
            timeout: Duration::from_secs(120),
        }
    }
    pub fn with_limits(mut self, limits: &crate::run_command::CommandLimits) -> Self {
        self.process_limits = limits.process_limits.clone();
        self.timeout = Duration::from_secs(
            limits
                .default_timeout_seconds
                .min(limits.max_timeout_seconds),
        );
        self
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    remote_url: String,
    source_branch: String,
    target_branch: String,
}
#[derive(Deserialize, Serialize)]
struct WorkerInput {
    arguments: Arguments,
    username: String,
    password: String,
    git_dir: PathBuf,
    objects: PathBuf,
}
fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed(error.to_string())
}
fn validate(args: &Arguments) -> Result<url::Url, ToolError> {
    let url = url::Url::parse(&args.remote_url).map_err(failed)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(failed(
            "push requires an HTTPS URL without embedded credentials, query or fragment",
        ));
    }
    for branch in [&args.source_branch, &args.target_branch] {
        if branch.is_empty()
            || branch.len() > 200
            || branch.starts_with('-')
            || branch.contains("..")
            || branch.contains("@{")
            || !branch
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
        {
            return Err(failed("invalid branch name"));
        }
    }
    Ok(url)
}
impl Tool for GitPush {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "git_push",
            "Push one local branch to one HTTPS remote branch using the user's host Git credential helper. Requires explicit remote-write approval. Credentials are confined to a trusted sandbox worker and never exposed to run_command or tool results. Only ordinary fast-forward pushes are supported; no force, deletion, mirror or SSH. Obtain the exact remote URL and branches before calling.",
            json!({"type":"object","properties":{"remote_url":{"type":"string"},"source_branch":{"type":"string"},"target_branch":{"type":"string"}},"required":["remote_url","source_branch","target_branch"],"additionalProperties":false}),
        )
    }
    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_str(&call.function.arguments).map_err(failed)?;
            let url = validate(&args)?;
            if !self.workspace.policy().network_policy().is_allowed() {
                return Err(failed("network disabled by command policy"));
            }
            let repo = orbit_runtime::git::GitRepositoryInfo::discover(self.workspace.root())
                .map_err(failed)?
                .ok_or_else(|| failed("workspace is not a Git repository"))?;
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let effective = self.workspace.policy().effective_policy();
            let mut external_capabilities: Vec<_> = effective
                .persistent_read
                .iter()
                .map(|p| format!("{} (ro)", p.canonical().display()))
                .chain(
                    effective
                        .persistent_read_write
                        .iter()
                        .map(|p| format!("{} (rw)", p.canonical().display())),
                )
                .collect();
            if let Ok(store) = effective.session_grants.read() {
                external_capabilities.extend(
                    store
                        .grants()
                        .iter()
                        .filter(|g| g.environment.is_local())
                        .map(|g| {
                            format!(
                                "{} ({})",
                                g.path.canonical().display(),
                                if g.write { "rw" } else { "ro" }
                            )
                        }),
                );
            }
            let description = format!(
                "git push {} {}:refs/heads/{}",
                args.remote_url, args.source_branch, args.target_branch
            );
            let approval = self.approvals.send(ApprovalRequest {
                preview: ApprovalPreview::Command(CommandPreview {
                    command: description.clone(),
                    cwd: Some(self.workspace.root().display().to_string()),
                    timeout_seconds: self.timeout.as_secs(),
                    workspace_access: CommandWorkspaceAccess::Read,
                    network_allowed: true,
                    external_write: false,
                    external_capabilities,
                    git: Some(orbit_runtime::git::GitCommandAnalysis {
                        raw_command: description,
                        subcommand: "push".into(),
                        kind: orbit_runtime::git::GitOperationKind::RemoteWrite {
                            remote: args.remote_url.clone(),
                            source_branch: Some(args.source_branch.clone()),
                            target_branch: Some(args.target_branch.clone()),
                        },
                    }),
                }),
                decision: sender,
            });
            tokio::select! {
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                sent = approval => sent.map_err(failed)?,
            }
            let approved = tokio::select! { _ = cancellation.cancelled() => return Err(ToolError::Cancelled), answer = receiver => answer.map_err(failed)? };
            if !approved {
                return Err(failed("push was not approved"));
            }
            let (username, password) = credential(&url, cancellation.clone()).await?;
            let private = tempfile::tempdir().map_err(failed)?;
            let scratch = tempfile::tempdir().map_err(failed)?;
            let map_path = |path: &Path| {
                path.strip_prefix(self.workspace.root())
                    .map(|rel| Path::new("/workspace").join(rel))
                    .unwrap_or_else(|_| path.to_owned())
            };
            let input = WorkerInput {
                arguments: args,
                username,
                password,
                git_dir: map_path(&repo.git_dir),
                objects: map_path(&repo.common_dir.join("objects")),
            };
            let secret = private.path().join("request.json");
            std::fs::write(&secret, serde_json::to_vec(&input).map_err(failed)?).map_err(failed)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600))
                .map_err(failed)?;
            let mut policy = ExecutionPolicy::read_only();
            policy.network = orbit_runtime::capability::NetworkPolicy::Allow;
            policy.unmask_git = true;
            policy.resources = self.process_limits.clone().into();
            policy.protected_paths = self.workspace.protected().to_vec();
            policy.opaque_runtime_roots = self.workspace.opaque_runtime_roots().to_vec();
            policy.external_read = effective
                .persistent_read
                .iter()
                .chain(effective.persistent_read_write.iter())
                .map(|p| p.canonical().to_owned())
                .collect();
            if let Ok(store) = effective.session_grants.read() {
                policy.external_read.extend(
                    store
                        .grants()
                        .iter()
                        .filter(|g| g.environment.is_local() && (g.read || g.write))
                        .map(|g| g.path.canonical().to_owned()),
                );
            }
            policy.mounts = vec![
                MountSpec::read_only(
                    std::env::current_exe().map_err(failed)?,
                    "/opt/orbit-git-worker",
                ),
                MountSpec::read_only(&secret, "/opt/orbit-push-request"),
            ];
            let request = ExecutionRequest::new(
                "/opt/orbit-git-worker --internal-git-push-worker /opt/orbit-push-request",
                self.workspace.root(),
                self.timeout,
            )
            .with_scratch(scratch.path());
            let environment =
                ExecutionEnvironment::new(self.workspace.root(), self.workspace.root())
                    .map_err(failed)?;
            let result = BwrapBackend::new()
                .map_err(failed)?
                .execute(&request, &policy, &environment, None, cancellation)
                .await
                .map_err(failed)?;
            if result.cancelled {
                return Err(ToolError::Cancelled);
            }
            if result.timed_out {
                return Err(failed("push timed out; inspect the remote before retrying"));
            }
            if result.exit_code != Some(0) {
                return Err(failed(
                    "HTTPS push failed or was rejected; check credentials, connectivity and branch protection",
                ));
            }
            Ok(ToolOutput::text(json!({"remote_url":input.arguments.remote_url,"source_branch":input.arguments.source_branch,"target_branch":input.arguments.target_branch,"pushed":true}).to_string()))
        })
    }
}
async fn credential(
    url: &url::Url,
    cancellation: CancellationToken,
) -> Result<(String, String), ToolError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let directory = tempfile::tempdir().map_err(failed)?;
    let mut command = tokio::process::Command::new("/usr/bin/git");
    command
        .args(["-c", "credential.interactive=false", "credential", "fill"])
        .current_dir(directory.path())
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    for name in [
        "HOME",
        "XDG_CONFIG_HOME",
        "DBUS_SESSION_BUS_ADDRESS",
        "DISPLAY",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(failed)?;
    let request = format!(
        "protocol=https\nhost={}{}\npath={}\n\n",
        url.host_str().unwrap(),
        url.port().map(|p| format!(":{p}")).unwrap_or_default(),
        url.path().trim_start_matches('/')
    );
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.as_bytes())
        .await
        .map_err(failed)?;
    let mut stdout = child.stdout.take().unwrap().take(64 * 1024 + 1);
    let fetch = async {
        let mut bytes = Vec::new();
        stdout
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| failed("credential helper failed"))?;
        if bytes.len() > 64 * 1024 {
            return Err(failed("credential helper response exceeds limit"));
        }
        let status = child
            .wait()
            .await
            .map_err(|_| failed("credential helper failed"))?;
        if !status.success() {
            return Err(failed(
                "host Git credential helper did not provide HTTPS credentials",
            ));
        }
        String::from_utf8(bytes).map_err(|_| failed("invalid credential response"))
    };
    let text = tokio::select! {
        _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
        result = tokio::time::timeout(Duration::from_secs(30), fetch) => result.map_err(|_| failed("credential helper timed out"))??,
    };
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .map(str::to_owned)
    };
    Ok((
        field("username=").ok_or_else(|| failed("credential helper returned no username"))?,
        field("password=").ok_or_else(|| failed("credential helper returned no password"))?,
    ))
}
/// Trusted subprocess entry point; its Git repository has no project configuration/hooks.
pub fn push_worker(path: &Path) -> Result<(), String> {
    let input: WorkerInput =
        serde_json::from_slice(&std::fs::read(path).map_err(|_| "cannot read push request")?)
            .map_err(|_| "invalid push request")?;
    validate(&input.arguments).map_err(|_| "invalid push arguments")?;
    if input
        .objects
        .as_os_str()
        .as_encoded_bytes()
        .contains(&b'\n')
    {
        return Err("unsupported repository object path".into());
    }
    let git = |directory: &Path| {
        let mut cmd = std::process::Command::new("/usr/bin/git");
        cmd.current_dir(directory)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
            ]);
        cmd
    };
    let commit = git(Path::new("/tmp"))
        .arg(format!("--git-dir={}", input.git_dir.display()))
        .args([
            "rev-parse",
            "--verify",
            &format!("refs/heads/{}^{{commit}}", input.arguments.source_branch),
        ])
        .output()
        .map_err(|_| "cannot resolve source branch")?;
    let oid = String::from_utf8(commit.stdout)
        .map_err(|_| "invalid source commit")?
        .trim()
        .to_owned();
    if !commit.status.success()
        || ![40, 64].contains(&oid.len())
        || !oid.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("source branch does not resolve to a commit".into());
    }
    let bare = tempfile::tempdir().map_err(|_| "cannot prepare push repository")?;
    let status = git(Path::new("/tmp"))
        .args(["init", "--bare"])
        .arg(if oid.len() == 64 {
            "--object-format=sha256"
        } else {
            "--object-format=sha1"
        })
        .arg(bare.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "cannot initialize push repository")?;
    if !status.success() {
        return Err("cannot initialize push repository".into());
    }
    std::fs::write(
        bare.path().join("objects/info/alternates"),
        format!("{}\n", input.objects.display()),
    )
    .map_err(|_| "cannot reference repository objects")?;
    use base64::Engine;
    let header = format!(
        "Authorization: Basic {}",
        base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", input.username, input.password))
    );
    let status = git(bare.path())
        .env("GIT_CONFIG_COUNT", "2")
        .env("GIT_CONFIG_KEY_0", "http.extraHeader")
        .env("GIT_CONFIG_VALUE_0", header)
        .env("GIT_CONFIG_KEY_1", "http.followRedirects")
        .env("GIT_CONFIG_VALUE_1", "false")
        .args([
            "push",
            "--",
            &input.arguments.remote_url,
            &format!("{oid}:refs/heads/{}", input.arguments.target_branch),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| "push failed")?;
    if status.success() {
        Ok(())
    } else {
        Err("push failed or rejected".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restricts_push_to_one_https_branch() {
        for remote in [
            "http://example.com/repo",
            "ssh://example.com/repo",
            "https://user:secret@example.com/repo",
            "https://example.com/repo?token=a",
        ] {
            assert!(
                validate(&Arguments {
                    remote_url: remote.into(),
                    source_branch: "main".into(),
                    target_branch: "main".into()
                })
                .is_err()
            );
        }
        for branch in ["", "--force", ":main", "+main", "main~1", "a..b"] {
            assert!(
                validate(&Arguments {
                    remote_url: "https://example.com/repo".into(),
                    source_branch: branch.into(),
                    target_branch: "main".into()
                })
                .is_err()
            );
        }
        assert!(
            validate(&Arguments {
                remote_url: "https://example.com/repo".into(),
                source_branch: "feature/example".into(),
                target_branch: "main".into()
            })
            .is_ok()
        );
    }
}
