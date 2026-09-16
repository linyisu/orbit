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
    /// List projects or reconnect a moved directory without connecting to an API.
    Projects {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// List or inspect saved sessions without connecting to an API.
    Sessions {
        #[command(subcommand)]
        command: SessionCommand,
    },
}

#[derive(Debug, Subcommand)]
enum ProjectCommand {
    /// List stable project IDs and their last known directory.
    List,
    /// Associate a moved project with the current directory. Close its TUI first.
    Link { id: String },
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
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u64,
    #[serde(default = "default_context_tokens")]
    context_tokens: usize,
    #[serde(default = "default_max_output_tokens")]
    max_output_tokens: u32,
    #[serde(default)]
    max_turns: usize,
    #[serde(default)]
    max_tool_calls: usize,
    #[serde(default)]
    command: orbit_tools::run_command::CommandLimits,
}

fn default_max_output_tokens() -> u32 {
    32_000
}

fn default_context_tokens() -> usize {
    128_000
}

fn default_timeout_seconds() -> u64 {
    600
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

        if config.context_tokens < 8192 {
            bail!("context_tokens must be at least 8192");
        }
        if config.max_output_tokens == 0
            || config.max_output_tokens as usize >= config.context_tokens
        {
            bail!("max_output_tokens must be positive and smaller than context_tokens");
        }
        if config.timeout_seconds == 0 {
            bail!("timeout_seconds must be positive");
        }
        config.command.validate().map_err(anyhow::Error::msg)?;
        Ok(config)
    }

    fn agent_config(&self) -> AgentConfig {
        AgentConfig {
            context_tokens: self.context_tokens,
            max_turns: self.max_turns,
            max_tool_calls: self.max_tool_calls,
        }
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
        Command::Projects { command } => {
            let store =
                orbit_store::store::Store::open(session::database_path(cli.database)?).await?;
            match command {
                ProjectCommand::List => {
                    for project in store.projects().await? {
                        println!(
                            "{}  {}",
                            project.id,
                            project.path.as_deref().unwrap_or("（未关联目录）")
                        );
                    }
                }
                ProjectCommand::Link { id } => {
                    let project = store.link_project(&id, std::env::current_dir()?).await?;
                    println!(
                        "已关联项目 {} → {}。重新打开 TUI 后用 /resume 恢复会话。",
                        project.id,
                        project.path.unwrap_or_default()
                    );
                }
            }
            Ok(())
        }
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
            runtime::workspace_tools(
                &config_path,
                Some(session.store.path()),
                Some(approvals),
                Default::default(),
            )?,
            session,
            approval_receiver,
            tui::RequestBudget {
                context_tokens: default_context_tokens(),
                max_output_tokens: default_max_output_tokens(),
                max_turns: 0,
                max_tool_calls: 0,
            },
        )
        .await;
    }
    let file_config = FileConfig::load(&config_path)?;
    let agent_config = file_config.agent_config();
    let command_limits = file_config.command;
    let context_tokens = file_config.context_tokens;
    let max_output_tokens = file_config.max_output_tokens;
    let model_name = file_config.model.clone();
    let client = ModelClient::new(file_config.model_config())?;
    let session =
        session::SessionState::open(database, args.resume, model_name.clone(), false).await?;
    tui::run(
        client,
        model_name,
        false,
        runtime::workspace_tools(
            &config_path,
            Some(session.store.path()),
            Some(approvals),
            command_limits,
        )?,
        session,
        approval_receiver,
        tui::RequestBudget {
            context_tokens,
            max_output_tokens,
            max_turns: agent_config.max_turns,
            max_tool_calls: agent_config.max_tool_calls,
        },
    )
    .await
}

async fn chat(config_path: PathBuf, args: ChatArgs) -> Result<()> {
    let file_config = FileConfig::load(&config_path)?;
    let agent_config = file_config.agent_config();
    let command_limits = file_config.command;
    let max_output_tokens = file_config.max_output_tokens;
    let client = ModelClient::new(file_config.model_config())?;
    let mut request = ChatRequest::new(vec![ChatMessage::user(args.prompt)]);
    request.max_tokens = Some(max_output_tokens);

    let agent = Agent::new(
        ModelClientBackend::new(client),
        runtime::workspace_tools(&config_path, None, None, command_limits)?,
        agent_config,
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

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn omitted_limits_use_working_defaults_and_explicit_limits_survive() {
        let input = r#"base_url = "http://localhost"
api_key = "test"
model = "test"
"#;
        let config: FileConfig = toml::from_str(input).unwrap();
        assert_eq!(config.context_tokens, 128000);
        assert_eq!(config.max_output_tokens, 32000);
        assert_eq!(config.timeout_seconds, 600);
        assert_eq!(config.agent_config().max_turns, 0);
        assert_eq!(config.agent_config().max_tool_calls, 0);
        assert_eq!(config.command.default_timeout_seconds, 120);
        assert_eq!(config.command.max_timeout_seconds, 600);
        let config: FileConfig = toml::from_str(&format!("{input}context_tokens = 1000000\nmax_output_tokens = 64000\nmax_turns = 75\nmax_tool_calls = 500\n[command]\ndefault_timeout_seconds = 300\nmax_timeout_seconds = 1800\n")).unwrap();
        assert_eq!(config.agent_config().max_turns, 75);
        assert_eq!(config.agent_config().max_tool_calls, 500);
        assert_eq!(config.max_output_tokens, 64000);
        assert_eq!(config.context_tokens, 1000000);
        assert_eq!(config.command.max_timeout_seconds, 1800);
        assert!(config.command.validate().is_ok());
    }
}
