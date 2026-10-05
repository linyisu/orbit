mod bootstrap;
mod command;
pub(crate) mod configuration;
pub(crate) mod connect;
pub(crate) mod directories;
pub(crate) mod discovery;
pub(crate) mod interaction;
pub(crate) mod protocol;
mod run;
mod server;
mod session;
pub(crate) mod setup;
pub(crate) mod transport;

use std::{io::Write, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, bail};
use base64::Engine;
use clap::Args;
use orbit_agent::events::AgentEvent;
use orbit_model::types::StreamEvent;

use protocol::{Mode, Request, Response, Update};
use transport::{Connection, Target};

pub(crate) use server::runtime_directory;
pub use server::serve;

#[derive(Clone, Debug, Args)]
pub struct ConnectArgs {
    /// System SSH destination or a Host alias from ~/.ssh/config.
    #[arg(long, conflicts_with = "wsl")]
    ssh: Option<String>,
    #[arg(long, requires = "ssh")]
    ssh_user: Option<String>,
    #[arg(long, requires = "ssh")]
    ssh_port: Option<u16>,
    #[arg(long, requires = "ssh")]
    identity: Option<String>,
    /// WSL distribution. Omit both --ssh and --wsl for a local workspace.
    #[arg(long)]
    wsl: Option<String>,
    #[arg(long, requires = "wsl")]
    wsl_user: Option<String>,
    /// Directory on the target; ~/ is expanded by the target service.
    #[arg(long, default_value = ".")]
    workspace: String,
    /// Use an installed target executable instead of automatically deploying the server.
    #[arg(long)]
    server: Option<String>,
    /// Upload a Linux Orbit binary matching the target architecture.
    #[arg(long, conflicts_with = "server")]
    upload_server: Option<PathBuf>,
    #[arg(long)]
    resume: Option<String>,
    /// Run a prompt without entering the TUI. Pending approvals are denied.
    #[arg(long, conflicts_with_all = ["list", "stop"])]
    prompt: Option<String>,
    /// List this workspace's sessions without starting an agent.
    #[arg(long, conflicts_with = "stop")]
    list: bool,
    /// Stop an idle workspace service. Active tasks must be cancelled first.
    #[arg(long)]
    stop: bool,
    #[arg(long)]
    allow_mcp_start: bool,
}

#[derive(Debug, Args)]
pub struct ServiceArgs {
    #[arg(long)]
    workspace: String,
    #[arg(long)]
    daemon: bool,
    #[arg(long)]
    allow_mcp_start: bool,
}

pub async fn connect(
    args: ConnectArgs,
    config: PathBuf,
    database: Option<PathBuf>,
    auth_dir: Option<PathBuf>,
) -> Result<()> {
    let connection = prepare_connection(&args, config, database, auth_dir, None).await?;
    let mut client = connection.connect().await?;
    if args.stop {
        client.request(Request::Shutdown).await?;
        println!("工作区服务已停止。");
        return Ok(());
    }
    if args.list {
        let Response::Sessions(sessions) = client.request(Request::Sessions).await? else {
            bail!("unexpected session list response");
        };
        for session in sessions {
            println!(
                "{}  {}  {}",
                session.id,
                session.model,
                session.title.replace(['\n', '\r'], " ")
            );
        }
        return Ok(());
    }
    let Response::Opened(info) = client
        .request(Request::Open {
            resume: args.resume,
            configuration: connection.configuration().await?,
        })
        .await?
    else {
        bail!("unexpected workspace response");
    };
    let id = info.id.clone();
    let result = if let Some(prompt) = args.prompt {
        run_prompt(&mut client, &id, prompt).await
    } else {
        crate::tui::run_workspace(client, connection, info).await
    };
    result.with_context(|| {
        format!(
            "工作区会话 {id}；可使用相同连接参数加 --resume {id} 重新接入，断开连接不会取消任务"
        )
    })
}

async fn prepare_connection(
    args: &ConnectArgs,
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
        local_configuration: if matches!(target, Target::Local) {
            None
        } else {
            Some(configuration::Local {
                config: std::path::absolute(&config)?,
                auth: crate::auth_cli::directory(
                    auth_dir.clone(),
                    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
                    std::env::var_os("HOME").map(PathBuf::from),
                )?,
                model: None,
            })
        },
        target,
        workspace: args.workspace.clone(),
        executable,
        config,
        database,
        auth_dir,
        allow_mcp_start: args.allow_mcp_start,
        unattended,
        interactive,
    });
    Ok(connection)
}

pub(crate) fn request_id() -> String {
    use std::hash::BuildHasher;
    let state = std::collections::hash_map::RandomState::new();
    format!(
        "{:016x}{:016x}",
        state.hash_one(std::time::SystemTime::now()),
        state.hash_one(std::process::id())
    )
}

async fn run_prompt(client: &mut transport::Client, session: &str, prompt: String) -> Result<()> {
    eprintln!("会话：{session}");
    let mut cursor = 0;
    loop {
        let Response::Page(page) = client
            .request(Request::Poll {
                session: session.into(),
                after: cursor,
            })
            .await?
        else {
            bail!("unexpected event response");
        };
        cursor = page.next;
        if !page.more {
            break;
        }
    }
    client
        .request(Request::Start {
            session: session.into(),
            request_id: request_id(),
            prompt,
            mode: Mode::Auto,
            model: None,
        })
        .await?;
    let mut failed = false;
    loop {
        let Response::Page(page) = client
            .request(Request::Poll {
                session: session.into(),
                after: cursor,
            })
            .await?
        else {
            bail!("unexpected event response");
        };
        cursor = page.next;
        for update in page.updates {
            match update {
                Update::Agent(event) => match *event {
                    AgentEvent::Model(StreamEvent::TextDelta(text)) => {
                        print!("{text}");
                        std::io::stdout().flush()?;
                    }
                    AgentEvent::Failed { error } => {
                        eprintln!("{error}");
                        failed = true;
                    }
                    _ => {}
                },
                Update::Finished { status } if status != "completed" => failed = true,
                _ => {}
            }
        }
        for approval in page.approvals {
            eprintln!("审批已拒绝：无交互模式；使用 connect --resume {session} 交互执行。");
            client
                .request(Request::Approve {
                    session: session.into(),
                    approval: approval.id,
                    allow: false,
                    remember: false,
                })
                .await?;
        }
        if !page.busy && !page.more {
            break;
        }
        if !page.more {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {},
                signal = tokio::signal::ctrl_c() => { signal?; client.request(Request::Cancel { session: session.into() }).await?; }
            }
        }
    }
    println!();
    if failed {
        bail!("workspace run did not complete successfully");
    }
    Ok(())
}

pub(crate) struct Call {
    pub request: Request,
    pub reply: Option<tokio::sync::oneshot::Sender<Result<Response>>>,
}

#[derive(Clone, Debug)]
pub(crate) struct Files {
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
