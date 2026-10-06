use anyhow::Result;
use clap::{Args, Parser};
use orbit_tui as tui;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "orbit", about = "Orbit terminal workspace")]
struct Cli {
    #[arg(long, env = "orbit_config", default_value = "config.toml")]
    config: PathBuf,
    #[arg(long, env = "orbit_database")]
    database: Option<PathBuf>,
    #[arg(long)]
    auth_dir: Option<PathBuf>,
    #[command(flatten)]
    options: TuiArgs,
}

#[derive(Debug, Args)]
struct TuiArgs {
    /// Restore a saved session in the current workspace.
    #[arg(long)]
    resume: Option<String>,

    /// Allow enabled local MCP server programs to start for this invocation.
    #[arg(long)]
    allow_mcp_start: bool,
}

fn main() -> Result<()> {
    orbit_client::register_server_bundle(include_bytes!(env!("ORBIT_EMBEDDED_SERVER")))?;
    if std::env::args().nth(1).as_deref() == Some("--internal-ssh-askpass") {
        orbit_client::interaction::askpass()?;
        return Ok(());
    }
    run()
}

#[tokio::main]
async fn run() -> Result<()> {
    let cli = Cli::parse();
    let auth = orbit_auth::directory(
        cli.auth_dir,
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )?;
    run_tui(cli.config, cli.database, cli.options, auth).await
}

async fn run_tui(
    config_path: PathBuf,
    database: Option<PathBuf>,
    args: TuiArgs,
    auth_directory: PathBuf,
) -> Result<()> {
    let config = std::path::absolute(config_path)?;
    let settings = tui::ConnectionSettings {
        config: config.clone(),
        auth_directory: auth_directory.clone(),
    };
    if !config.try_exists()? && !tui::setup_connection(&settings).await? {
        return Ok(());
    }
    let mut options = orbit_client::ConnectionOptions::local(
        std::env::current_dir()?.to_string_lossy().into_owned(),
    );
    options.allow_mcp_start = args.allow_mcp_start;
    let connection =
        orbit_client::prepare_connection(&options, config, database, Some(auth_directory), None)
            .await?;
    let (progress, _updates) = orbit_client::interaction::channel();
    let opened =
        orbit_client::connect::Opened::connect_progress(connection, args.resume, progress).await?;
    tui::run_workspace(opened.client, opened.connection, opened.info).await
}
