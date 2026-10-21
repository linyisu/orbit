//! Headless workspace service entry point for native and remote transports.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use orbit_service::{platform, workspace};

#[derive(Parser)]
#[command(name = "orbit-workspace-server")]
struct Cli {
    #[arg(long, default_value = "config.toml", global = true)]
    config: PathBuf,
    #[arg(long, global = true)]
    database: Option<PathBuf>,
    #[arg(long, global = true)]
    auth_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    WorkspaceService(ServiceArgs),
    #[cfg(target_os = "linux")]
    Remote(RemoteArgs),
}

#[cfg(target_os = "linux")]
#[derive(Args)]
struct RemoteArgs {
    #[arg(long)]
    state: PathBuf,
    #[arg(long)]
    workspace: PathBuf,
    #[command(subcommand)]
    action: RemoteAction,
}

#[cfg(target_os = "linux")]
#[derive(Subcommand)]
enum RemoteAction {
    Authorize {
        #[arg(long)]
        address: String,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        control: bool,
        #[arg(long)]
        approve: bool,
    },
    Listen {
        #[arg(long)]
        bind: std::net::SocketAddr,
    },
    Revoke {
        device: String,
    },
}

#[derive(Args)]
struct ServiceArgs {
    #[arg(long)]
    workspace: String,
    #[arg(long)]
    daemon: bool,
    #[arg(long)]
    allow_mcp_start: bool,
}

fn main() -> Result<()> {
    if platform::workers::run()? {
        return Ok(());
    }
    run()
}

#[tokio::main]
async fn run() -> Result<()> {
    let cli = Cli::parse();
    let auth = platform::auth_directory(cli.auth_dir)?;
    match cli.command {
        #[cfg(target_os = "linux")]
        Command::Remote(args) => {
            if let RemoteAction::Revoke { device } = &args.action {
                return workspace::remote::revoke(args.state, device);
            }
            let options = workspace::remote::HostOptions {
                state: args.state,
                workspace: args.workspace.canonicalize()?,
                database: orbit_service::session::database_path(cli.database)?,
                config: std::path::absolute(cli.config)?,
                auth,
            };
            match args.action {
                RemoteAction::Authorize {
                    address,
                    output,
                    control,
                    approve,
                } => {
                    let device = workspace::remote::authorize(
                        &options,
                        address,
                        output,
                        workspace::remote::Permissions { control, approve },
                    )?;
                    println!("Authorized device: {device}");
                    Ok(())
                }
                RemoteAction::Listen { bind } => workspace::remote::serve(options, bind).await,
                RemoteAction::Revoke { device } => {
                    workspace::remote::revoke(options.state, &device)
                }
            }
        }
        Command::WorkspaceService(args) => {
            workspace::serve(
                workspace::ServiceOptions {
                    workspace: args.workspace,
                    daemon: args.daemon,
                    allow_mcp_start: args.allow_mcp_start,
                },
                cli.config,
                cli.database,
                auth,
            )
            .await
        }
    }
}
