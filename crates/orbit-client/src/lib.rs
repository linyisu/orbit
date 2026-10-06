pub mod attachments;
mod bootstrap;
mod local_server;
pub use bootstrap::register_server_bundle;
mod command;
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
pub mod transport;
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
    let target = match (args.ssh.clone(), args.wsl.clone()) {
        (Some(host), None) => Target::Ssh {
            host,
            options: transport::SshOptions {
                user: args.ssh_user.clone(),
                port: args.ssh_port,
                identity: args.identity.clone(),
            },
        },
        (None, Some(distribution)) => Target::Wsl {
            distribution,
            user: args.wsl_user.clone(),
        },
        (None, None) => Target::Local,
        _ => bail!("choose one workspace target"),
    };
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
