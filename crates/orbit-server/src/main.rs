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
    let auth = platform::directory(
        cli.auth_dir,
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )?;
    match cli.command {
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
