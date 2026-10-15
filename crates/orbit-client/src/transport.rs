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
const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1_024;

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SshOptions {
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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
    /// Validate transport identity once before constructing any platform command.
    pub fn validate(&self) -> Result<()> {
        let user = match self {
            Self::Local => return Ok(()),
            Self::Ssh { host, options } => {
                ensure!(
                    !host.is_empty()
                        && !host.starts_with('-')
                        && !host
                            .chars()
                            .any(|c| c.is_whitespace() || c.is_control())
                        && !host.contains("://"),
                    "invalid SSH host"
                );
                ensure!(
                    options.port != Some(0),
                    "SSH port must be between 1 and 65535"
                );
                ensure!(
                    options
                        .identity
                        .as_ref()
                        .is_none_or(|path| !path.chars().any(char::is_control)),
                    "invalid SSH identity path"
                );
                options.user.as_ref()
            }
            Self::Wsl { distribution, user } => {
                ensure!(
                    !distribution.is_empty()
                        && !distribution.starts_with('-')
                        && !distribution
                            .chars()
                            .any(char::is_control),
                    "invalid WSL distribution"
                );
                user.as_ref()
            }
        };
        ensure!(
            user.is_none_or(|user| !user.is_empty()
                && !user.starts_with('-')
                && !user
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control())),
            "invalid remote username"
        );
        Ok(())
    }

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
        self.validate()?;
        ensure!(!arguments.is_empty(), "workspace command is empty");
        let command = match self {
            Target::Local => {
                let mut command = Command::new(&arguments[0]);
                command.args(&arguments[1..]);
                command
            }
            Target::Ssh { host, options } => {
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

/// Identity of the host and data source in which a session ID is meaningful.
pub struct DataSource<'a> {
    pub target: Target,
    pub server: Option<&'a str>,
    pub config: &'a std::path::Path,
    pub auth: Option<&'a std::path::Path>,
    pub database: Option<&'a std::path::Path>,
    pub workspace: &'a std::path::Path,
}

impl DataSource<'_> {
    /// Relative databases belong to their workspace; home-relative stores span projects.
    pub fn matches(&self, other: &Self) -> bool {
        let workspace_scoped = self.database.is_some_and(|path| {
            path.is_relative()
                && !path
                    .to_str()
                    .is_some_and(|path| path == "~" || path.starts_with("~/"))
        });
        self.target == other.target
            && self.server == other.server
            && self.config == other.config
            && (self.auth == other.auth
                || super::configuration::auth_directory(self.auth.map(PathBuf::from))
                    .ok()
                    .zip(super::configuration::auth_directory(other.auth.map(PathBuf::from)).ok())
                    .is_some_and(|(left, right)| left == right))
            && self.database == other.database
            && (!workspace_scoped || self.workspace == other.workspace)
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
    pub fn data_source(&self) -> DataSource<'_> {
        DataSource {
            target: self.target.clone(),
            server: Some(&self.executable),
            config: self
                .local_configuration
                .as_ref()
                .map_or(self.config.as_path(), |local| local.config.as_path()),
            auth: self
                .local_configuration
                .as_ref()
                .map(|local| local.auth.as_path())
                .or(self.auth_dir.as_deref()),
            database: self.database.as_deref(),
            workspace: std::path::Path::new(&self.workspace),
        }
    }

    pub async fn configuration(&self) -> Result<Option<super::configuration::Configuration>> {
        if matches!(self.target, Target::Local) {
            return Ok(None);
        }
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
            if let Some(configuration) = &self.local_configuration {
                configuration
                    .config
                    .to_string_lossy()
                    .into_owned()
            } else {
                self.config
                    .to_string_lossy()
                    .into_owned()
            },
        ];
        for (flag, path) in [
            ("--database", &self.database),
            (
                "--auth-dir",
                if self.local_configuration.is_some() && !matches!(self.target, Target::Local) {
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
            let mut bytes = [0; 4_096];
            while let Ok(count) = stderr.read(&mut bytes).await {
                if count == 0 {
                    break;
                }
                if let Some(progress) = &log_progress {
                    progress.log(&String::from_utf8_lossy(&bytes[..count]));
                }
                let mut output = captured.lock().await;
                output.extend_from_slice(&bytes[..count]);
                let excess = output
                    .len()
                    .saturating_sub(MAX_DIAGNOSTIC_BYTES);
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
            capabilities: None,
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
                workspace: Some(self.workspace.clone()),
                version: protocol::VERSION,
            }),
        )
        .await;
        match result {
            Ok(Ok(Response::Hello {
                version: protocol::VERSION,
                capabilities,
                ..
            })) => {
                client.capabilities = Some(capabilities);
                Ok(client)
            }
            Ok(Ok(response)) => bail!("incompatible workspace server: {response:?}"),
            Ok(Err(error)) => Err(anyhow::Error::new(error).context(
                "workspace connection failed; ensure Orbit is \
                installed on the target and available through --server",
            )),
            Err(_) => bail!("workspace connection timed out"),
        }
    }
}

/// Encoding and remote rejections leave framing usable; transport failures require reconnecting.
#[derive(Debug)]
pub enum RequestError {
    Closed,
    Encoding(std::io::Error),
    Remote(String),
    Transport(anyhow::Error),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => formatter.write_str(
                "workspace connection is closed; reconnect before sending another request",
            ),
            Self::Remote(message) => formatter.write_str(message),
            Self::Encoding(error) => write!(formatter, "request could not be encoded: {error}"),
            Self::Transport(error) => write!(formatter, "{error:#}"),
        }
    }
}

impl std::error::Error for RequestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error.as_ref()),
            Self::Encoding(error) => Some(error),
            _ => None,
        }
    }
}

pub struct Client {
    pub capabilities: Option<orbit_protocol::Capabilities>,
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

    pub async fn request(
        &mut self,
        request: Request,
    ) -> std::result::Result<Response, RequestError> {
        if self.failed {
            return Err(RequestError::Closed);
        }
        let frame = orbit_protocol::EncodedFrame::new(&request).map_err(RequestError::Encoding)?;
        // Mark it unusable before awaiting: cancellation halfway through a frame
        // must never let the next request consume the previous response.
        self.failed = true;
        let result = tokio::time::timeout(REQUEST_TIMEOUT, async {
            frame.write(&mut self.stdin).await?;
            protocol::read::<Response>(&mut self.stdout).await
        })
        .await;
        match result {
            Ok(Ok(response)) => {
                self.failed = false;
                if let Response::Error(error) = response {
                    return Err(RequestError::Remote(error));
                }
                Ok(response)
            }
            result => {
                let _ = self.process.start_kill();
                let _ = tokio::time::timeout(Duration::from_secs(1), &mut self.drain).await;
                let log = self.diagnostics.lock().await;
                let diagnostic = String::from_utf8_lossy(&log);
                Err(RequestError::Transport(anyhow::anyhow!(
                    "workspace transport failed: {result:?}\n{diagnostic}"
                )))
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
pub(crate) mod tests {
    use super::*;

    const RESPONSES_PATH_ENV: &str = "ORBIT_CLIENT_TEST_RESPONSES";

    pub(crate) async fn fixture_client(responses: Vec<Response>) -> Client {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("responses.json");
        std::fs::write(&path, serde_json::to_vec(&responses).unwrap()).unwrap();
        let mut process = Command::new(std::env::current_exe().unwrap())
            .args([
                "transport::tests::response_fixture",
                "--exact",
                "--nocapture",
            ])
            .env(RESPONSES_PATH_ENV, path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let stdin = process.stdin.take().unwrap();
        let mut stdout = process.stdout.take().unwrap();
        protocol::read_preamble(&mut stdout)
            .await
            .unwrap();
        Client {
            capabilities: None,
            process,
            stdin,
            stdout,
            diagnostics: Arc::new(Mutex::new(Vec::new())),
            drain: tokio::spawn(async {}),
            failed: false,
        }
    }

    #[tokio::test]
    async fn response_fixture() {
        use tokio::io::AsyncWriteExt;
        let Some(path) = std::env::var_os(RESPONSES_PATH_ENV) else {
            return;
        };
        let responses: Vec<Response> =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        stdout
            .write_all(protocol::PREAMBLE)
            .await
            .unwrap();
        stdout.flush().await.unwrap();
        for response in responses {
            let _: Request = protocol::read(&mut stdin)
                .await
                .unwrap();
            protocol::write(&mut stdout, &response)
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn oversized_attachments_do_not_close_or_write_to_the_transport() {
        let mut client = fixture_client(vec![Response::FileMissing]).await;
        let request = Request::SubmitAttachments {
            session: "sample".into(),
            request_id: "generated-upload".into(),
            prompt: String::new(),
            images: Vec::new(),
            files: (0..2)
                .map(|index| orbit_protocol::FileUpload {
                    name: format!("part-{index}.bin"),
                    data: "A".repeat(orbit_protocol::MAX_FRAME_BYTES / 2),
                })
                .collect(),
            steer: false,
        };
        assert!(matches!(
            client.request(request).await,
            Err(RequestError::Encoding(_))
        ));
        assert!(
            !client.failed(),
            "encoding rejection precedes all transport I/O"
        );
        assert!(matches!(
            client
                .request(Request::ReadFile {
                    session: "sample".into(),
                    path: "absent.txt".into(),
                    allow_missing: true,
                })
                .await
                .unwrap(),
            Response::FileMissing
        ));
    }

    #[tokio::test]
    async fn remote_rejections_preserve_transport_and_disconnection_is_typed() {
        let mut client = fixture_client(vec![
            Response::Error("synthetic denial".into()),
            Response::FileMissing,
        ])
        .await;
        let request = || Request::ReadFile {
            session: "sample".into(),
            path: "absent.txt".into(),
            allow_missing: true,
        };
        assert!(matches!(
            client.request(request()).await,
            Err(RequestError::Remote(message)) if message == "synthetic denial"
        ));
        assert!(!client.failed());
        assert!(matches!(
            client.request(request()).await.unwrap(),
            Response::FileMissing
        ));
        assert!(matches!(
            client.request(request()).await,
            Err(RequestError::Transport(_))
        ));
        assert!(client.failed());
        assert!(matches!(
            client.request(request()).await,
            Err(RequestError::Closed)
        ));
    }

    #[test]
    fn switching_targets_clears_inapplicable_fields_and_rejects_incomplete_options() {
        let mut options = crate::ConnectionOptions::local("workspace".into());
        options.ssh_port = Some(2222);
        assert!(options.target().is_err());
        options
            .set_target(Target::Ssh {
                host: "sample-host".into(),
                options: SshOptions {
                    user: Some("builder".into()),
                    port: Some(2222),
                    identity: Some("sample-key".into()),
                },
            })
            .unwrap();
        options
            .set_target(Target::Wsl {
                distribution: "Sample Linux".into(),
                user: Some("developer".into()),
            })
            .unwrap();
        assert!(
            options.ssh.is_none()
                && options.ssh_user.is_none()
                && options.ssh_port.is_none()
                && options.identity.is_none()
        );
        assert!(matches!(options.target().unwrap(), Target::Wsl { .. }));
        options
            .set_target(Target::Local)
            .unwrap();
        assert!(options.wsl.is_none() && options.wsl_user.is_none());
        assert!(
            Target::Ssh {
                host: "sample-host".into(),
                options: SshOptions {
                    port: Some(0),
                    ..Default::default()
                }
            }
            .validate()
            .is_err()
        );
        assert!(
            Target::Local
                .command(&[], false, None)
                .is_err()
        );
    }

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
    fn session_identity_separates_data_sources_and_workspace_relative_remote_stores() {
        use std::path::Path;
        let mut left = DataSource {
            target: Target::Ssh {
                host: "fixture-host".into(),
                options: Default::default(),
            },
            server: Some("orbit"),
            config: Path::new("settings.toml"),
            auth: Some(Path::new("auth")),
            database: Some(Path::new("state.db")),
            workspace: Path::new("/sample/one"),
        };
        let mut right = DataSource {
            target: left.target.clone(),
            server: left.server,
            config: left.config,
            auth: left.auth,
            database: left.database,
            workspace: Path::new("/sample/two"),
        };
        assert!(!left.matches(&right));
        left.target = Target::Local;
        right.target = Target::Local;
        assert!(!left.matches(&right));
        left.database = Some(Path::new("~/state.db"));
        right.database = left.database;
        assert!(left.matches(&right));
        right.database = Some(Path::new("~/different.db"));
        assert!(!left.matches(&right));
        right.database = left.database;
        right.config = Path::new("another.toml");
        assert!(!left.matches(&right));
        right.config = left.config;
        right.auth = Some(Path::new("other-auth"));
        assert!(!left.matches(&right));
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
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--", "my-jump-host"])
        );
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
        let local = connection(Target::Local)
            .command()
            .unwrap();
        assert_eq!(
            local
                .as_std()
                .get_args()
                .next()
                .unwrap(),
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
