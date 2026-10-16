//! Local, SSH and WSL workspace transports and client-owned preferences.
//! Protocol and service operations remain independent of the presentation layer.

pub mod accounts;
pub mod attachments;
mod bootstrap;
pub mod library;
mod local_server;
#[cfg(feature = "test-support")]
pub mod test_support;
pub use bootstrap::register_server_bundle;
mod command;
pub mod completion;
pub mod configuration;
pub mod connect;
pub mod directories;
pub mod discovery;
pub mod interaction;
pub mod platform;
pub mod prompt;
pub mod protocol;
pub mod pump;
mod selection;
pub use selection::requested_model;
pub mod transport;
pub mod usage;
pub mod windows;
use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result, bail};
use base64::Engine;

use protocol::{Request, Response};
use transport::{Connection, Target};

#[derive(Clone, Debug)]
pub struct ConnectionOptions {
    /// System SSH destination or a Host alias from ~/.ssh/config.
    pub ssh: Option<String>,
    pub ssh_user: Option<String>,
    pub ssh_port: Option<u16>,
    pub identity: Option<String>,
    /// WSL distribution. Omit both --ssh and --wsl for a local workspace.
    pub wsl: Option<String>,
    pub wsl_user: Option<String>,
    /// Directory on the target; ~/ is expanded by the target service.
    pub workspace: String,
    /// Use an installed target executable instead of automatically deploying the server.
    pub server: Option<String>,
    /// Upload a Linux Orbit binary matching the target architecture.
    pub upload_server: Option<PathBuf>,
    pub allow_mcp_start: bool,
}

impl ConnectionOptions {
    /// Resolve exactly one workspace transport, rejecting contradictory target options.
    pub fn target(&self) -> Result<Target> {
        anyhow::ensure!(
            self.ssh.is_some()
                || (self.ssh_user.is_none() && self.ssh_port.is_none() && self.identity.is_none()),
            "SSH options require an SSH target"
        );
        anyhow::ensure!(
            self.wsl.is_some() || self.wsl_user.is_none(),
            "WSL user requires a WSL target"
        );
        let target = match (self.ssh.clone(), self.wsl.clone()) {
            (Some(host), None) => Target::Ssh {
                host,
                options: transport::SshOptions {
                    user: self.ssh_user.clone(),
                    port: self.ssh_port,
                    identity: self.identity.clone(),
                },
            },
            (None, Some(distribution)) => Target::Wsl {
                distribution,
                user: self.wsl_user.clone(),
            },
            (None, None) => Target::Local,
            _ => bail!("choose one workspace target"),
        };
        target.validate()?;
        Ok(target)
    }

    /// Replace the transport as one unit, dropping fields that belong to the previous target.
    pub fn set_target(&mut self, target: Target) -> Result<()> {
        target.validate()?;
        self.ssh = None;
        self.ssh_user = None;
        self.ssh_port = None;
        self.identity = None;
        self.wsl = None;
        self.wsl_user = None;
        match target {
            Target::Local => {}
            Target::Ssh { host, options } => {
                self.ssh = Some(host);
                self.ssh_user = options.user;
                self.ssh_port = options.port;
                self.identity = options.identity;
            }
            Target::Wsl { distribution, user } => {
                self.wsl = Some(distribution);
                self.wsl_user = user;
            }
        }
        Ok(())
    }

    pub fn local(workspace: String) -> Self {
        Self {
            ssh: None,
            ssh_user: None,
            ssh_port: None,
            identity: None,
            wsl: None,
            wsl_user: None,
            workspace,
            server: None,
            upload_server: None,
            allow_mcp_start: false,
        }
    }
}

pub async fn prepare_connection(
    args: &ConnectionOptions,
    config: PathBuf,
    database: Option<PathBuf>,
    auth_dir: Option<PathBuf>,
    progress: Option<interaction::Progress>,
) -> Result<Arc<Connection>> {
    let target = args.target()?;
    anyhow::ensure!(
        args.server.is_none() || args.upload_server.is_none(),
        "choose a server command or an uploaded server, not both"
    );
    let unattended = progress.is_some();
    let interactive = if matches!(target, Target::Ssh { .. }) {
        match &progress {
            Some(progress) => Some(Arc::new(
                interaction::Interaction::new(progress.clone(), None).await?,
            )),
            None => None,
        }
    } else {
        None
    };
    let executable = match args.server.clone() {
        Some(executable) => {
            bootstrap::verify(
                &target,
                &executable,
                unattended,
                interactive.as_deref(),
                progress.as_ref(),
            )
            .await?;
            executable
        }
        None => {
            bootstrap::executable(
                &target,
                args.upload_server.clone(),
                unattended,
                interactive.as_deref(),
                progress.as_ref(),
            )
            .await?
        }
    };
    let connection = Arc::new(Connection {
        local_configuration: Some(configuration::Local {
            config: std::path::absolute(&config)?,
            auth: orbit_auth::directory(
                auth_dir.clone(),
                std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
                std::env::var_os("HOME").map(PathBuf::from),
            )?,
            model: None,
        }),
        target,
        workspace: args.workspace.clone(),
        executable,
        config,
        database,
        auth_dir: if args.ssh.is_none() && args.wsl.is_none() {
            Some(orbit_auth::directory(
                auth_dir,
                std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
                std::env::var_os("HOME").map(PathBuf::from),
            )?)
        } else {
            auth_dir
        },
        allow_mcp_start: args.allow_mcp_start,
        unattended,
        interactive,
    });
    Ok(connection)
}

pub use orbit_protocol::request_id;

pub struct Call {
    pub request: Request,
    pub reply: Option<tokio::sync::oneshot::Sender<Result<Response>>>,
}

#[derive(Clone, Debug)]
pub struct Files {
    pub requests: Arc<tokio::sync::mpsc::Sender<Call>>,
    pub session: String,
}

impl PartialEq for Files {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.requests, &other.requests) && self.session == other.session
    }
}

impl Eq for Files {}

impl std::hash::Hash for Files {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&Arc::as_ptr(&self.requests), state);
        std::hash::Hash::hash(&self.session, state);
    }
}

impl Files {
    pub async fn read(&self, path: &str) -> Result<Vec<u8>> {
        let (reply, response) = tokio::sync::oneshot::channel();
        self.requests
            .send(Call {
                request: Request::ReadFile {
                    session: self.session.clone(),
                    path: path.into(),
                    allow_missing: false,
                },
                reply: Some(reply),
            })
            .await
            .map_err(|_| anyhow::anyhow!("workspace connection closed"))?;
        match response
            .await
            .context("workspace file transfer interrupted")??
        {
            Response::File(bytes) => Ok(base64::engine::general_purpose::STANDARD.decode(bytes)?),
            Response::Error(error) => bail!("{error}"),
            _ => bail!("unexpected workspace file response"),
        }
    }
}
