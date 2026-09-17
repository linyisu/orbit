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
    types::{
        ChatMessage, ChatRequest, ModelConfig, ReasoningAdapter, ReasoningEffort, StreamEvent,
    },
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
    /// Restore a saved session in the current workspace.
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
    providers: BTreeMap<String, ProviderConfig>,
}

#[derive(Clone, Debug, Deserialize)]
struct ProviderConfig {
    #[serde(default = "default_api_format")]
    api_format: ApiFormat,
    base_url: String,
    api_key: String,
    /// The first model is selected when Orbit starts with this provider.
    models: Vec<ModelProfile>,
    #[serde(default = "default_timeout_seconds")]
    timeout_seconds: u64,
}

#[derive(Clone, Debug, Deserialize)]
struct ModelProfile {
    id: String,
    /// Overrides the top-level context window for this model only.
    context_tokens: Option<usize>,
    reasoning: Option<ReasoningProfile>,
}

#[derive(Clone, Debug, Deserialize)]
struct ReasoningProfile {
    adapter: ReasoningAdapter,
    efforts: Vec<ReasoningEffort>,
    default: ReasoningEffort,
    #[serde(default)]
    extra: toml::Table,
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
                || provider.models.is_empty()
                || provider.timeout_seconds == 0
                || provider
                    .models
                    .iter()
                    .any(|model| model.id.trim().is_empty())
            {
                bail!(
                    "provider {name:?} must set nonempty base_url, api_key, models and a positive timeout_seconds"
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
        for (name, provider) in &config.providers {
            let mut models = std::collections::BTreeSet::new();
            for model in &provider.models {
                if !models.insert(&model.id) {
                    bail!("provider {name:?} contains duplicate model {:?}", model.id);
                }
                if let Some(reasoning) = &model.reasoning {
                    let expected = match provider.api_format {
                        ApiFormat::OpenaiChatCompletions => ReasoningAdapter::OpenaiChat,
                        ApiFormat::OpenaiResponses => ReasoningAdapter::OpenaiResponses,
                        ApiFormat::AnthropicMessages => ReasoningAdapter::AnthropicMessages,
                    };
                    if reasoning.adapter != expected {
                        bail!(
                            "model {:?}: reasoning adapter must match its provider API format",
                            model.id
                        );
                    }
                    if reasoning.efforts.is_empty()
                        || !reasoning.efforts.contains(&reasoning.default)
                    {
                        bail!(
                            "model {:?}: reasoning efforts must include its default",
                            model.id
                        );
                    }
                    for (index, effort) in reasoning.efforts.iter().enumerate() {
                        if reasoning.efforts[..index].contains(effort) {
                            bail!(
                                "model {:?}: reasoning effort {effort:?} is repeated",
                                model.id
                            );
                        }
                    }
                }
                let context_tokens = model.context_tokens.unwrap_or(config.context_tokens);
                if context_tokens < 8192 || config.max_output_tokens as usize >= context_tokens {
                    bail!(
                        "model {:?}: context_tokens must be at least 8192 and larger than max_output_tokens",
                        model.id
                    );
                }
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
            for (index, model) in provider.models.iter().enumerate() {
                let reasoning = model.reasoning.as_ref();
                options.push(tui::ModelOption {
                    provider: name.clone(),
                    model: model.id.clone(),
                    context_tokens: model.context_tokens.unwrap_or(self.context_tokens),
                    default_for_provider: index == 0,
                    reasoning_efforts: reasoning
                        .map_or_else(Vec::new, |profile| profile.efforts.clone()),
                    config: ModelConfig {
                        api_format: provider.api_format,
                        base_url: provider.base_url.clone(),
                        api_key: Some(provider.api_key.clone()),
                        model: model.id.clone(),
                        timeout: Duration::from_secs(provider.timeout_seconds),
                        reasoning_effort: reasoning.map(|profile| profile.default),
                        reasoning_adapter: reasoning.map(|profile| profile.adapter),
                        reasoning_extra: reasoning
                            .map(|profile| serde_json::to_value(&profile.extra))
                            .transpose()?
                            .unwrap_or_else(|| serde_json::json!({})),
                    },
                });
            }
        }
        Ok(options)
    }

    fn default_model_option(&self) -> Result<tui::ModelOption> {
        self.model_options()?
            .into_iter()
            .find(|option| option.provider == self.default_provider && option.default_for_provider)
            .ok_or_else(|| anyhow::anyhow!("default_provider is not configured"))
    }

    fn agent_config_for(&self, context_tokens: usize) -> AgentConfig {
        AgentConfig {
            context_tokens,
            auto_compact: self.auto_compact,
            max_turns: self.max_turns,
            max_tool_calls: self.max_tool_calls,
        }
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
    let file_config = FileConfig::load(&config_path)?;
    let model_options = file_config.model_options()?;
    let default_model = file_config.default_model_option()?;
    let agent_config = file_config.agent_config();
    let command_limits = file_config.command.clone();
    let web = file_config.web.clone();
    let max_output_tokens = file_config.max_output_tokens;
    let model_name = default_model.qualified_name();
    let context_tokens = default_model.context_tokens;
    let client = ModelClient::new(default_model.config)?;
    let session = session::SessionState::open(database, args.resume, model_name.clone()).await?;
    tui::run(
        client,
        model_name,
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
    let command_limits = file_config.command.clone();
    let web = file_config.web.clone();
    let max_output_tokens = file_config.max_output_tokens;
    let default_model = file_config.default_model_option()?;
    let agent_config = file_config.agent_config_for(default_model.context_tokens);
    let client = ModelClient::new(default_model.config)?;
    let instructions = instructions::load_current();
    let mut messages = vec![ChatMessage::user(args.prompt)];
    instructions::inject(&mut messages, &instructions);
    let mut request = ChatRequest::new(messages);
    request.max_tokens = Some(max_output_tokens);
    request.reasoning_effort = client.config().reasoning_effort;

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
                    fallback,
                } => eprintln!(
                    "上下文已自动压缩：{before_tokens} → {after_tokens} tokens{}",
                    if fallback { "（本地回退）" } else { "" }
                ),
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
[[providers.test.models]]
id = "test"
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
    fn configured_provider_models_select_their_provider_connection() {
        let config: FileConfig = toml::from_str(
            r#"default_provider = "deepseek"
[providers.deepseek]
api_format = "openai_chat_completions"
base_url = "https://deepseek.example/v1"
api_key = "deepseek-key"
[[providers.deepseek.models]]
id = "deepseek-chat"
[[providers.deepseek.models]]
id = "deepseek-reasoner"
"#,
        )
        .unwrap();
        let options = config.model_options().unwrap();
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].model, "deepseek-chat");
        assert!(options[0].default_for_provider);
        assert_eq!(options[0].config.model, "deepseek-chat");
        assert_eq!(options[1].model, "deepseek-reasoner");
        assert_eq!(options[1].config.model, "deepseek-reasoner");
        assert_eq!(options[1].config.base_url, "https://deepseek.example/v1");
    }

    #[test]
    fn model_reasoning_profile_sets_the_default_request_effort() {
        let config: FileConfig = toml::from_str(
            r#"default_provider = "openai"
[providers.openai]
api_format = "openai_responses"
base_url = "https://openai.example/v1"
api_key = "openai-key"
[[providers.openai.models]]
id = "reasoning-model"
reasoning = { adapter = "openai_responses", efforts = ["low", "high"], default = "high" }
"#,
        )
        .unwrap();
        let option = config.default_model_option().unwrap();
        assert_eq!(
            option.reasoning_efforts,
            vec![ReasoningEffort::Low, ReasoningEffort::High]
        );
        assert_eq!(option.config.reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(
            option.config.reasoning_adapter,
            Some(ReasoningAdapter::OpenaiResponses)
        );
    }

    #[test]
    fn model_context_window_overrides_the_default_budget() {
        let config: FileConfig = toml::from_str(
            r#"default_provider = "test"
context_tokens = 128000
max_output_tokens = 32000
[providers.test]
base_url = "https://test.example/v1"
api_key = "test-key"
[[providers.test.models]]
id = "small-window"
context_tokens = 64000
[[providers.test.models]]
id = "default-window"
"#,
        )
        .unwrap();
        let options = config.model_options().unwrap();
        assert_eq!(options[0].context_tokens, 64_000);
        assert_eq!(options[1].context_tokens, 128_000);
        assert_eq!(
            config
                .agent_config_for(options[0].context_tokens)
                .context_tokens,
            64_000
        );
    }

    #[test]
    fn example_configuration_uses_valid_model_capability_profiles() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, include_str!("../../../config.example.toml")).unwrap();
        let config = FileConfig::load(&path).unwrap();
        let models = config.model_options().unwrap();
        assert_eq!(models.len(), 4);
        assert!(
            models
                .iter()
                .all(|model| !model.reasoning_efforts.is_empty())
        );
    }

    #[test]
    fn models_must_be_present_and_unique_with_a_real_default_provider() {
        let duplicate = r#"default_provider = "other"
[providers.other]
base_url = "https://other.example/v1"
api_key = "other-key"
[[providers.other.models]]
id = "other-model"
[[providers.other.models]]
id = "other-model"
"#;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, duplicate).unwrap();
        assert!(FileConfig::load(&path).is_err());

        fs::write(
            &path,
            r#"default_provider = "missing"
[providers.other]
base_url = "https://other.example/v1"
api_key = "other-key"
[[providers.other.models]]
id = "other-model"
"#,
        )
        .unwrap();
        assert!(FileConfig::load(&path).is_err());

        fs::write(
            &path,
            r#"default_provider = "other"
[providers.other]
base_url = "https://other.example/v1"
api_key = "other-key"
[[providers.other.models]]
id = "other-model"
reasoning = { adapter = "openai_chat", efforts = ["high", "high"], default = "high" }
"#,
        )
        .unwrap();
        assert!(FileConfig::load(&path).is_err());
    }
}
