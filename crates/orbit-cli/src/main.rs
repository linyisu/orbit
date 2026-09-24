use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

mod runtime;

use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand};
use orbit_agent::{
    agent::{Agent, AgentConfig},
    error::AgentError,
    events::AgentEvent,
    model::ModelClientBackend,
};
use orbit_model::{
    client::{ApiFormat, ModelClient},
    types::{ChatMessage, ChatRequest, ModelConfig, StreamEvent},
};
use orbit_store::Store;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

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

    #[arg(long, global = true)]
    database: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Chat(ChatArgs),
    Session(SessionArgs),
}

#[derive(Debug, Args)]
struct SessionArgs {
    #[command(subcommand)]
    command: SessionCommand,
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    List,
}

#[derive(Debug, Args)]
struct ChatArgs {
    #[arg(long, short)]
    stream: bool,

    #[arg(long)]
    resume: Option<String>,

    prompt: String,
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    #[serde(default)]
    api_format: ApiFormat,
    base_url: String,
    api_key: String,
    model: String,
    timeout_seconds: u64,
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
    let database = database_path(cli.database)?;

    match cli.command {
        Command::Chat(args) => chat(cli.config, database, args).await,
        Command::Session(SessionArgs {
            command: SessionCommand::List,
        }) => list_sessions(database).await,
    }
}

fn database_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }

    let data_home = match std::env::var_os("XDG_DATA_HOME") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => {
            let home = std::env::var_os("HOME")
                .context("HOME is not set; pass --database to select a session database")?;
            PathBuf::from(home).join(".local/share")
        }
    };
    Ok(data_home.join("orbit-main/sessions.db"))
}

async fn list_sessions(database: PathBuf) -> Result<()> {
    let workspace = std::env::current_dir()?;
    let store = Store::open(database).await?;

    for session in store.list(&workspace).await? {
        println!("{}  {}  {}", session.id, session.model, session.title);
    }
    Ok(())
}

async fn chat(config_path: PathBuf, database: PathBuf, args: ChatArgs) -> Result<()> {
    let workspace = std::env::current_dir()?.canonicalize()?;
    let file_config = FileConfig::load(&config_path)?;
    let model_name = file_config.model.clone();
    let client = ModelClient::new(file_config.model_config())?;
    let store = Store::open(&database).await?;
    let tools = runtime::read_only_workspace_tools(&workspace, &config_path, store.path())?;

    let (mut writer, mut history) = if let Some(id) = args.resume.as_deref() {
        let session = store.session(id).await?;
        ensure!(
            Path::new(&session.workspace) == workspace,
            "session belongs to a different workspace: {}",
            session.workspace
        );
        let writer = store.acquire(id).await?;
        let history = store.load(id).await?.projected_history();
        (writer, history)
    } else {
        (store.create(&workspace, &model_name).await?, Vec::new())
    };

    writer.begin(args.prompt.clone(), model_name).await?;
    eprintln!("session {}", writer.id());
    history.push(ChatMessage::user(args.prompt));
    let request = ChatRequest::new(history);
    let agent = Agent::new(
        ModelClientBackend::new(client),
        tools,
        AgentConfig::default(),
    );

    let streaming = args.stream;
    let result = agent
        .run_with_agent_events(request, CancellationToken::new(), |event| {
            let writer = &writer;
            async move {
                match event {
                    AgentEvent::MessageCommitted { message } => writer
                        .append(message)
                        .await
                        .map_err(|error| AgentError::EventHandler(error.to_string()))?,
                    AgentEvent::Model(StreamEvent::TextDelta(text)) if streaming => {
                        print!("{text}");
                        std::io::stdout()
                            .flush()
                            .map_err(|error| AgentError::EventHandler(error.to_string()))?;
                    }
                    _ => {}
                }
                Ok(())
            }
        })
        .await;

    let status = match &result {
        Ok(_) => "completed",
        Err(AgentError::Cancelled) => "cancelled",
        Err(_) => "failed",
    };
    writer.finish(status).await?;

    let result = result?;
    if streaming {
        println!();
    } else if let ChatMessage::Assistant { content, .. } = result.final_message {
        println!("{}", content.unwrap_or_default());
    }

    Ok(())
}
