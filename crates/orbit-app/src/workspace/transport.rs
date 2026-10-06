use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use tokio::{
    io::AsyncReadExt,
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::Mutex,
};

use super::protocol::{self, Request, Response};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(45);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct SshOptions {
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Local,
    Ssh {
        host: String,
        options: SshOptions,
    },
    Wsl {
        distribution: String,
        user: Option<String>,
    },
}

impl Target {
    pub fn label(&self) -> String {
        match self {
            Self::Local => "本地".into(),
            Self::Ssh { host, .. } => format!("SSH {host}"),
            Self::Wsl { distribution, .. } => format!("WSL {distribution}"),
        }
    }

    pub fn command(
        &self,
        arguments: &[String],
        unattended: bool,
        interactive: Option<&super::interaction::Interaction>,
    ) -> Result<Command> {
        let command = match self {
            Target::Local => {
                let mut command = Command::new(&arguments[0]);
                command.args(&arguments[1..]);
                command
            }
            Target::Ssh { host, options } => {
                ensure!(
                    !host.is_empty()
                        && !host.starts_with('-')
                        && !host.contains(['\n', '\r', '\0']),
                    "invalid SSH host"
                );
                let mut command = Command::new("ssh");
                if let Some(user) = &options.user {
                    command.arg("-l").arg(user);
                }
                if let Some(port) = options.port {
                    command.arg("-p").arg(port.to_string());
                }
                if let Some(identity) = &options.identity {
                    command.arg("-i").arg(identity);
                }
                if let Some(interactive) = interactive {
                    interactive.configure(&mut command);
                } else if unattended {
                    command.args(["-o", "BatchMode=yes"]);
                }
                command.args([
                    "-T",
                    "-o",
                    "ConnectTimeout=15",
                    "-o",
                    "ConnectionAttempts=1",
                    "-o",
                    "ServerAliveInterval=15",
                    "-o",
                    "ServerAliveCountMax=3",
                    "--",
                    host,
                ]);
                command.arg(format!("exec {}", shell_words(arguments)?));
                command
            }
            Target::Wsl { distribution, user } => {
                ensure!(
                    !distribution.is_empty(),
                    "WSL distribution must not be empty"
                );
                let mut command = Command::new("wsl.exe");
                command.args(["--distribution", distribution]);
                if let Some(user) = user {
                    command.args(["--user", user]);
                }
                command.args(["--exec", "sh", "-lc"]);
                command.arg(format!("exec {}", shell_words(arguments)?));
                command
            }
        };
        Ok(command)
    }
}

#[derive(Clone)]
pub struct Connection {
    pub local_configuration: Option<super::configuration::Local>,
    pub target: Target,
    pub workspace: String,
    pub executable: String,
    pub config: PathBuf,
    pub database: Option<PathBuf>,
    pub auth_dir: Option<PathBuf>,
    pub allow_mcp_start: bool,
    pub unattended: bool,
    pub interactive: Option<Arc<super::interaction::Interaction>>,
}

impl Connection {
    pub async fn configuration(&self) -> Result<Option<super::configuration::Configuration>> {
        match &self.local_configuration {
            Some(configuration) => Ok(Some(configuration.snapshot().await?)),
            None => Ok(None),
        }
    }

    pub fn command(&self) -> Result<Command> {
        ensure!(!self.workspace.is_empty(), "workspace must not be empty");
        ensure!(
            !self.executable.is_empty(),
            "server executable must not be empty"
        );
        let mut arguments = vec![
            self.executable.clone(),
            "workspace-service".into(),
            "--workspace".into(),
            self.workspace.clone(),
            "--config".into(),
            if self.local_configuration.is_some() {
                "config.toml".into()
            } else {
                self.config.to_string_lossy().into_owned()
            },
        ];
        for (flag, path) in [
            ("--database", &self.database),
            (
                "--auth-dir",
                if self.local_configuration.is_some() {
                    &None
                } else {
                    &self.auth_dir
                },
            ),
        ] {
            if let Some(path) = path {
                arguments.extend([flag.into(), path.to_string_lossy().into_owned()]);
            }
        }
        if self.allow_mcp_start {
            arguments.push("--allow-mcp-start".into());
        }
        let mut command =
            self.target
                .command(&arguments, self.unattended, self.interactive.as_deref())?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        Ok(command)
    }

    pub async fn connect(&self) -> Result<Client> {
        self.connect_progress(None).await
    }

    pub async fn connect_progress(
        &self,
        progress: Option<&super::interaction::Progress>,
    ) -> Result<Client> {
        let mut process = self
            .command()?
            .spawn()
            .context("cannot start workspace transport")?;
        let stdin = process
            .stdin
            .take()
            .context("workspace transport has no stdin")?;
        let stdout = process
            .stdout
            .take()
            .context("workspace transport has no stdout")?;
        let mut stderr = process
            .stderr
            .take()
            .context("workspace transport has no stderr")?;
        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let captured = diagnostics.clone();
        let progress = progress.cloned().or_else(|| {
            self.interactive
                .as_ref()
                .map(|interactive| interactive.progress.clone())
        });
        let log_progress = progress.clone();
        let drain = tokio::spawn(async move {
            let mut bytes = [0; 4096];
            while let Ok(count) = stderr.read(&mut bytes).await {
                if count == 0 {
                    break;
                }
                if let Some(progress) = &log_progress {
                    progress.log(&String::from_utf8_lossy(&bytes[..count]));
                }
                let mut output = captured.lock().await;
                output.extend_from_slice(&bytes[..count]);
                let excess = output.len().saturating_sub(MAX_DIAGNOSTIC_BYTES);
                output.drain(..excess);
            }
        });
        let mut client = Client {
            process,
            stdin,
            stdout,
            diagnostics,
            drain,
            failed: false,
        };
        let marker = super::interaction::timeout(CONNECT_TIMEOUT, progress.as_ref(), async {
            Ok(protocol::read_preamble(&mut client.stdout).await?)
        })
        .await;
        if let Err(error) = marker {
            let _ = client.process.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(1), &mut client.drain).await;
            let log = client.diagnostics.lock().await;
            if log.is_empty() {
                bail!("工作区握手失败：{error:#}");
            }
            bail!(
                "{}",
                super::bootstrap::runtime_error(&String::from_utf8_lossy(&log))
            );
        }
        let result = tokio::time::timeout(
            CONNECT_TIMEOUT,
            client.request(Request::Hello {
                version: protocol::VERSION,
            }),
        )
        .await;
        match result {
            Ok(Ok(Response::Hello { version: protocol::VERSION, .. })) => Ok(client),
            Ok(Ok(response)) => bail!("incompatible workspace server: {response:?}"),
            Ok(Err(error)) => Err(error.context("workspace connection failed; ensure Orbit is installed on the target and available through --server")),
            Err(_) => bail!("workspace connection timed out"),
        }
    }
}

pub struct Client {
    process: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    diagnostics: Arc<Mutex<Vec<u8>>>,
    drain: tokio::task::JoinHandle<()>,
    failed: bool,
}

impl Client {
    pub fn failed(&self) -> bool {
        self.failed
    }

    pub async fn request(&mut self, request: Request) -> Result<Response> {
        ensure!(
            !self.failed,
            "workspace connection is closed; reconnect before sending another request"
        );
        // Mark it unusable before awaiting: cancellation halfway through a frame
        // must never let the next request consume the previous response.
        self.failed = true;
        let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
            protocol::write(&mut self.stdin, &request).await?;
            protocol::read::<Response>(&mut self.stdout).await
        })
        .await;
        match result {
            Ok(Ok(response)) => {
                self.failed = false;
                if let Response::Error(error) = response {
                    bail!("{error}");
                }
                Ok(response)
            }
            result => {
                let _ = self.process.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(1), &mut self.drain).await;
                let log = self.diagnostics.lock().await;
                let diagnostic = String::from_utf8_lossy(&log);
                bail!("workspace transport failed: {result:?}\n{diagnostic}")
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.process.start_kill();
        self.drain.abort();
    }
}

fn shell_words(arguments: &[String]) -> Result<String> {
    arguments
        .iter()
        .map(|argument| {
            ensure!(
                !argument.contains('\0'),
                "NUL is not allowed in a remote argument"
            );
            Ok(format!("'{}'", argument.replace('\'', "'\\''")))
        })
        .collect::<Result<Vec<_>>>()
        .map(|words| words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection(target: Target) -> Connection {
        Connection {
            local_configuration: None,
            target,
            workspace: "/tmp/project '中文';$(touch nope)".into(),
            executable: "orbit".into(),
            config: "config.toml".into(),
            database: None,
            auth_dir: None,
            allow_mcp_start: false,
            unattended: false,
            interactive: None,
        }
    }

    #[test]
    fn embedded_ssh_never_prompts_on_the_tui_terminal() {
        let mut config = connection(Target::Ssh {
            host: "dev".into(),
            options: Default::default(),
        });
        config.unattended = true;
        let command = config.command().unwrap();
        assert!(
            command
                .as_std()
                .get_args()
                .any(|arg| arg == "BatchMode=yes")
        );
        config.unattended = false;
        let command = config.command().unwrap();
        assert!(
            !command
                .as_std()
                .get_args()
                .any(|arg| arg == "BatchMode=yes")
        );
    }

    #[test]
    fn transports_use_one_service_command_and_quote_every_remote_argument() {
        let ssh = connection(Target::Ssh {
            host: "my-jump-host".into(),
            options: Default::default(),
        })
        .command()
        .unwrap();
        let args: Vec<_> = ssh
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert!(args.windows(2).any(|pair| pair == ["--", "my-jump-host"]));
        let remote = shlex::split(args.last().unwrap()).unwrap();
        assert_eq!(
            &remote[..4],
            ["exec", "orbit", "workspace-service", "--workspace"]
        );
        assert_eq!(remote[4], "/tmp/project '中文';$(touch nope)");
        assert!(
            connection(Target::Ssh {
                host: "-oProxyCommand=evil".into(),
                options: Default::default()
            })
            .command()
            .is_err()
        );
        let local = connection(Target::Local).command().unwrap();
        assert_eq!(
            local.as_std().get_args().next().unwrap(),
            "workspace-service"
        );
        let wsl = connection(Target::Wsl {
            distribution: "Ubuntu".into(),
            user: Some("dev".into()),
        })
        .command()
        .unwrap();
        let args: Vec<_> = wsl
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(&args[..4], ["--distribution", "Ubuntu", "--user", "dev"]);
        assert!(args.contains(&"--exec".into()));
    }
}
