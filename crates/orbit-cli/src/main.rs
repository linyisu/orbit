use std::{
    collections::BTreeMap,
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

mod instructions;
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
    Show {
        id: String,
    },
    /// Inspect structured summaries, retained boundaries and confirmed file activity.
    Compactions {
        id: String,
    },
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
    default_provider: String,
    #[serde(default = "default_context_tokens")]
    context_tokens: usize,
    #[serde(default = "default_auto_compact")]
    auto_compact: bool,
    #[serde(default = "default_max_output_tokens")]
    max_output_tokens: u32,
    #[serde(default)]
    max_turns: usize,
    #[serde(default)]
    max_tool_calls: usize,
    #[serde(default)]
    command: orbit_tools::run_command::CommandLimits,
    #[serde(default)]
    web: orbit_tools::web_fetch::WebConfig,
    #[serde(default)]
    providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    models: Vec<NamedModel>,
}

#[derive(Clone, Debug, Deserialize)]
struct ProviderConfig {
    #[serde(default = "default_api_format")]
    api_format: ApiFormat,
    base_url: String,
    api_key: String,
    /// Default model selected by `/provider <name>`.
    model: String,
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NamedModel {
    alias: String,
    provider: String,
    /// Overrides the provider's default model for this alias.
    model: Option<String>,
}

fn default_max_output_tokens() -> u32 {
    32_000
}

fn default_auto_compact() -> bool {
    true
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

        if config.context_tokens < 8192 {
            bail!("context_tokens must be at least 8192");
        }
        if config.max_output_tokens == 0
            || config.max_output_tokens as usize >= config.context_tokens
        {
            bail!("max_output_tokens must be positive and smaller than context_tokens");
        }
        for (name, provider) in &config.providers {
            if name.trim().is_empty()
                || provider.base_url.trim().is_empty()
                || provider.api_key.trim().is_empty()
                || provider.model.trim().is_empty()
                || provider.timeout_seconds == 0
            {
                bail!(
                    "provider {name:?} must set nonempty base_url, api_key, model and a positive timeout_seconds"
                );
            }
        }
        let options = config.model_options()?;
        if !options
            .iter()
            .any(|option| option.provider == config.default_provider && option.default_for_provider)
        {
            bail!(
                "default_provider {:?} does not name a configured provider",
                config.default_provider
            );
        }
        let mut aliases = std::collections::BTreeSet::new();
        for option in options {
            if option.alias.trim().is_empty() || !aliases.insert(option.alias) {
                bail!("configured model aliases must be nonempty and unique");
            }
        }
        config.command.validate().map_err(anyhow::Error::msg)?;
        Ok(config)
    }

    fn agent_config(&self) -> AgentConfig {
        AgentConfig {
            context_tokens: self.context_tokens,
            auto_compact: self.auto_compact,
            max_turns: self.max_turns,
            max_tool_calls: self.max_tool_calls,
        }
    }

    fn model_options(&self) -> Result<Vec<tui::ModelOption>> {
        let mut options = Vec::new();
        for (name, provider) in &self.providers {
            options.push(tui::ModelOption {
                alias: name.clone(),
                provider: name.clone(),
                default_for_provider: true,
                config: ModelConfig {
                    api_format: provider.api_format,
                    base_url: provider.base_url.clone(),
                    api_key: Some(provider.api_key.clone()),
                    model: provider.model.clone(),
                    timeout: Duration::from_secs(provider.timeout_seconds),
                },
            });
        }
        for model in &self.models {
            let provider = self.providers.get(&model.provider).ok_or_else(|| {
                anyhow::anyhow!(
                    "model alias {:?} refers to unknown provider {:?}",
                    model.alias,
                    model.provider
                )
            })?;
            options.push(tui::ModelOption {
                alias: model.alias.clone(),
                provider: model.provider.clone(),
                default_for_provider: false,
                config: ModelConfig {
                    api_format: provider.api_format,
                    base_url: provider.base_url.clone(),
                    api_key: Some(provider.api_key.clone()),
                    model: model
                        .model
                        .clone()
                        .unwrap_or_else(|| provider.model.clone()),
                    timeout: Duration::from_secs(provider.timeout_seconds),
                },
            });
        }
        Ok(options)
    }

    fn default_model_option(&self) -> Result<tui::ModelOption> {
        self.model_options()?
            .into_iter()
            .find(|option| option.provider == self.default_provider && option.default_for_provider)
            .ok_or_else(|| anyhow::anyhow!("default_provider is not configured"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Chat(args) => chat(cli.config, cli.database, args).await,
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
                SessionCommand::Compactions { id } => println!(
                    "{}",
                    serde_json::to_string_pretty(&store.compactions(&id).await?)?
                ),
                SessionCommand::Show { id } => {
                    println!("{}", serde_json::to_string_pretty(&store.load(&id).await?)?)
                }
            }
            Ok(())
        }
    }
}

async fn run_tui(config_path: PathBuf, database: Option<PathBuf>, args: TuiArgs) -> Result<()> {
    let instructions = instructions::load_current();
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
            ModelClient::new(config.clone())?,
            name.clone(),
            true,
            runtime::workspace_tools(
                &config_path,
                Some(session.store.path()),
                Some(approvals),
                Default::default(),
                Default::default(),
                Some(session.scratch.clone()),
            )?,
            session,
            approval_receiver,
            tui::RequestBudget {
                context_tokens: default_context_tokens(),
                auto_compact: true,
                max_output_tokens: default_max_output_tokens(),
                max_turns: 0,
                max_tool_calls: 0,
                instructions,
                model_options: vec![tui::ModelOption {
                    alias: name.clone(),
                    provider: "demo".into(),
                    default_for_provider: true,
                    config: config.clone(),
                }],
            },
        )
        .await;
    }
    let file_config = FileConfig::load(&config_path)?;
    let model_options = file_config.model_options()?;
    let default_model = file_config.default_model_option()?;
    let agent_config = file_config.agent_config();
    let command_limits = file_config.command.clone();
    let web = file_config.web.clone();
    let context_tokens = file_config.context_tokens;
    let max_output_tokens = file_config.max_output_tokens;
    let model_name = default_model.alias.clone();
    let client = ModelClient::new(default_model.config)?;
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
            web,
            Some(session.scratch.clone()),
        )?,
        session,
        approval_receiver,
        tui::RequestBudget {
            context_tokens,
            max_output_tokens,
            auto_compact: agent_config.auto_compact,
            max_turns: agent_config.max_turns,
            max_tool_calls: agent_config.max_tool_calls,
            instructions,
            model_options,
        },
    )
    .await
}

async fn chat(config_path: PathBuf, database: Option<PathBuf>, args: ChatArgs) -> Result<()> {
    let file_config = FileConfig::load(&config_path)?;
    let agent_config = file_config.agent_config();
    let command_limits = file_config.command.clone();
    let web = file_config.web.clone();
    let max_output_tokens = file_config.max_output_tokens;
    let client = ModelClient::new(file_config.default_model_option()?.config)?;
    let instructions = instructions::load_current();
    let mut messages = vec![ChatMessage::user(args.prompt)];
    instructions::inject(&mut messages, &instructions);
    let mut request = ChatRequest::new(messages);
    request.max_tokens = Some(max_output_tokens);

    let database = session::database_path(database)?;
    let agent = Agent::new(
        ModelClientBackend::new(client),
        runtime::workspace_tools(
            &config_path,
            Some(&database),
            None,
            command_limits,
            web,
            None,
        )?,
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
                AgentEvent::CompactionStarted => eprintln!("正在自动整理上下文…"),
                AgentEvent::ContextCompacted {
                    before_tokens,
                    after_tokens,
                } => eprintln!("上下文已自动压缩：{before_tokens} → {after_tokens} tokens"),
                AgentEvent::CompactionFailed { error } => {
                    eprintln!("自动压缩未完成，保留原上下文：{error}")
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
        let provider = r#"[providers.test]
base_url = "http://localhost"
api_key = "test"
model = "test"
"#;
        let input = format!("default_provider = \"test\"\n{provider}");
        let config: FileConfig = toml::from_str(&input).unwrap();
        assert_eq!(config.context_tokens, 128000);
        assert!(config.auto_compact);
        let disabled: FileConfig = toml::from_str(&format!(
            "default_provider = \"test\"\nauto_compact = false\n{provider}"
        ))
        .unwrap();
        assert!(!disabled.agent_config().auto_compact);
        assert_eq!(config.max_output_tokens, 32000);
        assert_eq!(config.providers["test"].timeout_seconds, 600);
        assert_eq!(config.agent_config().max_turns, 0);
        assert_eq!(config.agent_config().max_tool_calls, 0);
        assert_eq!(config.command.default_timeout_seconds, 120);
        assert_eq!(config.command.max_timeout_seconds, 600);
        let config: FileConfig = toml::from_str(&format!("default_provider = \"test\"\ncontext_tokens = 1000000\nmax_output_tokens = 64000\nmax_turns = 75\nmax_tool_calls = 500\n[command]\ndefault_timeout_seconds = 300\nmax_timeout_seconds = 1800\n{provider}")).unwrap();
        assert_eq!(config.agent_config().max_turns, 75);
        assert_eq!(config.agent_config().max_tool_calls, 500);
        assert_eq!(config.max_output_tokens, 64000);
        assert_eq!(config.context_tokens, 1000000);
        assert_eq!(config.command.max_timeout_seconds, 1800);
        assert!(config.command.validate().is_ok());
    }

    #[test]
    fn configured_model_aliases_select_their_provider_connection() {
        let config: FileConfig = toml::from_str(
            r#"default_provider = "deepseek"
[providers.deepseek]
api_format = "openai_chat_completions"
base_url = "https://deepseek.example/v1"
api_key = "deepseek-key"
model = "deepseek-chat"

[[models]]
alias = "reasoner"
provider = "deepseek"
model = "deepseek-reasoner"
"#,
        )
        .unwrap();
        let options = config.model_options().unwrap();
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].alias, "deepseek");
        assert!(options[0].default_for_provider);
        assert_eq!(options[0].config.model, "deepseek-chat");
        assert_eq!(options[1].alias, "reasoner");
        assert_eq!(options[1].config.model, "deepseek-reasoner");
        assert_eq!(options[1].config.base_url, "https://deepseek.example/v1");
    }

    #[test]
    fn aliases_must_target_known_providers_and_be_unique() {
        let unknown: FileConfig = toml::from_str(
            r#"default_provider = "missing"
[[models]]
alias = "other"
provider = "missing"
"#,
        )
        .unwrap();
        assert!(unknown.model_options().is_err());

        let duplicate = r#"default_provider = "other"
[providers.other]
base_url = "https://other.example/v1"
api_key = "other-key"
model = "other-model"
[[models]]
alias = "other"
provider = "other"
"#;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, duplicate).unwrap();
        assert!(FileConfig::load(&path).is_err());
    }
}
