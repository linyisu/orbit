use anyhow::Result;
use clap::{Args, Parser};
use orbit_service::context::instructions;
use orbit_service::{WorkspaceOptions, prepare_workspace, session};
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
    if orbit_service::platform::workers::run()? {
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
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let options = WorkspaceOptions {
        initial_directory: std::env::current_dir()?,
        auth_directory,
        config: std::path::absolute(config_path)?,
        database: session::database_path(database)?,
        allow_mcp_start: args.allow_mcp_start,
        config_home: std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|path| path.join(".config"))),
        home,
        instruction_dir: instructions::default_user_instruction_dir(),
        editor_command: std::env::var("VISUAL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                std::env::var("EDITOR")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_else(|| "vi".into()),
    };
    if !options.config.try_exists()? && !tui::setup_connection(&options).await? {
        return Ok(());
    }

    let mut prepared = prepare_workspace(&options, &options.initial_directory, args.resume).await?;

    while let Some(next) = tui::run(prepared, &options).await? {
        prepared = next;
    }

    Ok(())
}
