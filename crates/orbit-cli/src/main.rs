use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use orbit_agent::{
    agent::{Agent, AgentConfig},
    events::AgentEvent,
    model::ModelClientBackend,
};
use orbit_model::{
    client::{ApiFormat, ModelClient},
    types::{ChatMessage, ChatRequest, ModelConfig, StreamEvent},
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

mod runtime;
mod session;
mod tui;

#[derive(Debug, Parser)]
#[command(name = "orbit")]
struct Cli {
    #[arg(
        long,
        env = "orbit_config",
        default_value = "config.toml",
        global = true
    )]
    config: PathBuf,

    /// SQLite session database (defaults to the user data directory).
    #[arg(long, env = "orbit_database", global = true)]
    database: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Chat(ChatArgs),
    Tui(TuiArgs),
    /// List or inspect saved sessions without connecting to an API.
    Sessions {
        #[command(subcommand)]
        command: SessionCommand,
    },
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    List,
    Show { id: String },
}

#[derive(Debug, Args)]
struct TuiArgs {
    /// Preview the interface with a simulated model, without an API key.
    #[arg(long)]
    demo: bool,

    /// Restore a saved session in the current workspace and mode.
    #[arg(long)]
    resume: Option<String>,
}

#[derive(Debug, Args)]
struct ChatArgs {
    #[arg(long, short)]
    stream: bool,

    prompt: String,
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    #[serde(default = "default_api_format")]
    api_format: ApiFormat,
    base_url: String,
    api_key: String,
    model: String,
    timeout_seconds: u64,
}

fn default_api_format() -> ApiFormat {
    ApiFormat::OpenaiChatCompletions
}

impl FileConfig {
    fn load(path: &PathBuf) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Self = toml::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;

        if config.api_key.trim().is_empty() {
            bail!("api_key is empty in {}", path.display());
        }

        Ok(config)
    }

    fn model_config(self) -> ModelConfig {
        ModelConfig {
            api_format: self.api_format,
            base_url: self.base_url,
            api_key: Some(self.api_key),
            model: self.model,
            timeout: Duration::from_secs(self.timeout_seconds),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Chat(args) => chat(cli.config, args).await,
        Command::Tui(args) => run_tui(cli.config, cli.database, args).await,
        Command::Sessions { command } => {
            let store =
                orbit_store::store::Store::open(session::database_path(cli.database)?).await?;
            match command {
                SessionCommand::List => println!("{}", session::list_text(&store).await?),
                SessionCommand::Show { id } => {
                    println!("{}", serde_json::to_string_pretty(&store.load(&id).await?)?)
                }
            }
            Ok(())
        }
    }
}

async fn run_tui(config_path: PathBuf, database: Option<PathBuf>, args: TuiArgs) -> Result<()> {
    let database = session::database_path(database)?;
    let (approvals, approval_receiver) = orbit_tools::approval::channel();
    if args.demo {
        let config = ModelConfig {
            model: "演示模式 · 不连接 API".into(),
            ..ModelConfig::default()
        };
        let name = config.model.clone();
        let session =
            session::SessionState::open(database, args.resume, name.clone(), true).await?;
        return tui::run(
            ModelClient::new(config)?,
            name,
            true,
            runtime::workspace_tools(&config_path, Some(session.store.path()), Some(approvals))?,
            session,
            approval_receiver,
        )
        .await;
    }
    let file_config = FileConfig::load(&config_path)?;
    let model_name = file_config.model.clone();
    let client = ModelClient::new(file_config.model_config())?;
    let session =
        session::SessionState::open(database, args.resume, model_name.clone(), false).await?;
    tui::run(
        client,
        model_name,
        false,
        runtime::workspace_tools(&config_path, Some(session.store.path()), Some(approvals))?,
        session,
        approval_receiver,
    )
    .await
}

async fn chat(config_path: PathBuf, args: ChatArgs) -> Result<()> {
    let file_config = FileConfig::load(&config_path)?;
    let client = ModelClient::new(file_config.model_config())?;
    let request = ChatRequest::new(vec![ChatMessage::user(args.prompt)]);

    let agent = Agent::new(
        ModelClientBackend::new(client),
        runtime::workspace_tools(&config_path, None, None)?,
        AgentConfig::default(),
    );

    if args.stream {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(256);
        let cancellation = CancellationToken::new();
        let task = tokio::spawn(async move {
            agent
                .run_with_agent_events(request, cancellation, sender)
                .await
        });

        while let Some(event) = receiver.recv().await {
            match event {
                AgentEvent::Model(StreamEvent::TextDelta(text))
                | AgentEvent::Model(StreamEvent::ReasoningDelta(text)) => {
                    print!("{text}");
                    io::stdout().flush()?;
                }
                AgentEvent::Failed { error } => eprintln!("agent failed: {error}"),
                AgentEvent::ToolStarted { call } => {
                    eprintln!("tool started: {}", call.function.name)
                }
                AgentEvent::ToolFailed { call, error } => {
                    eprintln!("tool failed: {}: {error}", call.function.name)
                }
                _ => {}
            }
        }
        task.await??;
        println!();
    } else {
        let result = agent.run(request, CancellationToken::new()).await?;

        if let ChatMessage::Assistant { content, .. } = result.final_message {
            println!("{}", content.unwrap_or_default());
        }
    }

    Ok(())
}
