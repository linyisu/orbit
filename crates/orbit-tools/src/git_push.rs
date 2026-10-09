//! Explicit HTTPS push with host credentials confined to a trusted helper process.
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::{
    execution::{ExecutionEnvironment, ExecutionPolicy, ExecutionRequest, execution_backend},
    filesystem::Workspace,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::approval::{
    ApprovalPreview, ApprovalRequest, ApprovalSender, CommandPreview, CommandWorkspaceAccess,
};

const DEFAULT_PUSH_TIMEOUT: Duration = Duration::from_secs(120);
const CREDENTIAL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;
const REMOTE_URL_REQUIREMENT: &str = "remote_url must be an explicit HTTPS URL without embedded credentials, query or fragment; SSH/scp remotes are unsupported and SSH credentials cannot authenticate this HTTPS push";

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
            timeout: DEFAULT_PUSH_TIMEOUT,
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
    let invalid = |message: &str| ToolError::InvalidArguments {
        tool: "git_push".into(),
        message: message.into(),
    };
    let url = url::Url::parse(&args.remote_url).map_err(|_| invalid(REMOTE_URL_REQUIREMENT))?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(REMOTE_URL_REQUIREMENT));
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
            return Err(invalid("invalid branch name"));
        }
    }

    Ok(url)
}

impl Tool for GitPush {
    fn capabilities(&self) -> orbit_agent::tool::ToolCapabilities {
        orbit_agent::tool::ToolCapabilities::exclusive(
            orbit_agent::tool::ToolEffect::HostExecution,
            false,
        )
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "git_push",
            "Push one local branch to one explicit HTTPS remote branch. Prerequisite: the user's host global Git credential helper must already provide noninteractive HTTPS credentials for that remote. This tool never reads origin or converts an SSH remote; SSH keys/agents cannot authenticate it. Requires explicit remote-write approval. Credentials are passed privately to the helper and omitted from tool results. Only ordinary fast-forward pushes are supported; no force, deletion, mirror or SSH. Obtain the exact HTTPS URL and branches before calling. If credentials are unavailable, report the prerequisite instead of retrying or inventing credentials.",
            json!({"type":"object","properties":{"remote_url":{"type":"string","description":"Exact HTTPS repository URL; independent of origin and requires host HTTPS authentication"},"source_branch":{"type":"string"},"target_branch":{"type":"string"}},"required":["remote_url","source_branch","target_branch"],"additionalProperties":false}),
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
            let input = WorkerInput {
                arguments: args,
                username,
                password,
                git_dir: repo.git_dir.clone(),
                objects: repo.common_dir.join("objects"),
            };
            let secret = private.path().join("request.json");
            std::fs::write(&secret, serde_json::to_vec(&input).map_err(failed)?).map_err(failed)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600))
                .map_err(failed)?;
            let policy = ExecutionPolicy {
                resources: self.process_limits.clone().into(),
                ..Default::default()
            };
            let worker = std::env::current_exe().map_err(failed)?;
            let command = shlex::try_join([
                worker
                    .to_str()
                    .ok_or_else(|| failed("non-UTF-8 worker path"))?,
                "--internal-git-push-worker",
                secret
                    .to_str()
                    .ok_or_else(|| failed("non-UTF-8 request path"))?,
            ])
            .map_err(failed)?;
            let request = ExecutionRequest::new(command, self.workspace.root(), self.timeout);
            let environment =
                ExecutionEnvironment::new(self.workspace.root(), self.workspace.root())
                    .map_err(failed)?;
            let result = execution_backend()
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
    let mut stdout = child.stdout.take().unwrap().take(MAX_CREDENTIAL_BYTES + 1);
    let fetch = async {
        let mut bytes = Vec::new();
        stdout
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| failed("credential helper failed"))?;
        if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
            return Err(failed("credential helper response exceeds limit"));
        }
        let status = child
            .wait()
            .await
            .map_err(|_| failed("credential helper failed"))?;
        if !status.success() {
            return Err(missing_credentials(url));
        }
        String::from_utf8(bytes).map_err(|_| failed("invalid credential response"))
    };
    let text = tokio::select! {
        _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
        result = tokio::time::timeout(CREDENTIAL_TIMEOUT, fetch) => result.map_err(|_| failed("credential helper timed out"))??,
    };
    parse_credentials(url, &text)
}

fn missing_credentials(url: &url::Url) -> ToolError {
    failed(format!(
        "host global Git credential helper did not provide usable HTTPS credentials for {}. Configure noninteractive HTTPS authentication for this remote in the host global Git configuration before retrying. An SSH origin/key does not supply HTTPS credentials; repository-local helpers are not used. Credentials must not be supplied in tool arguments.",
        url.host_str().unwrap_or("the remote")
    ))
}

fn parse_credentials(url: &url::Url, text: &str) -> Result<(String, String), ToolError> {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    Ok((
        field("username=").ok_or_else(|| missing_credentials(url))?,
        field("password=").ok_or_else(|| missing_credentials(url))?,
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

    #[tokio::test]
    async fn ssh_remote_is_rejected_before_approval_or_authentication() {
        let dir = tempfile::tempdir().unwrap();
        let (sender, mut receiver) = crate::approval::channel();
        let tool = GitPush::new(Workspace::new(dir.path()).unwrap(), sender);
        for remote in [
            "git@example.com:owner/repo.git",
            "ssh://git@example.com/owner/repo.git",
        ] {
            let error = tool
                .execute(
                    ToolCall::function(
                        "push",
                        "git_push",
                        json!({
                            "remote_url": remote, "source_branch": "main", "target_branch": "main"
                        }),
                    ),
                    CancellationToken::new(),
                )
                .await
                .unwrap_err();

            assert!(matches!(error, ToolError::InvalidArguments { .. }));
            assert!(error.to_string().contains("explicit HTTPS URL"));
            assert!(matches!(
                receiver.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ));
        }
    }

    #[test]
    fn missing_or_empty_credentials_give_safe_actionable_guidance() {
        let url = url::Url::parse("https://example.com/repo").unwrap();
        for response in [
            "",
            "username=demo\n",
            "password=fake-test-secret\n",
            "username=demo\npassword=\n",
            "username=\npassword=fake-test-secret\n",
        ] {
            let error = parse_credentials(&url, response).unwrap_err().to_string();
            assert!(error.contains("host global Git credential helper"));
            assert!(error.contains("example.com"));
            assert!(error.contains("SSH origin/key does not supply HTTPS credentials"));
            assert!(!error.contains("fake-test-secret"));
        }

        assert_eq!(
            parse_credentials(
                &url,
                "protocol=https\nusername=demo\npassword=fake-test-secret\n\n"
            )
            .unwrap(),
            ("demo".into(), "fake-test-secret".into())
        );
    }

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
