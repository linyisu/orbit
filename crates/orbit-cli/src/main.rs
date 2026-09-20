use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use orbit_agent::{
    agent::{Agent, AgentConfig},
    error::AgentError,
    events::AgentEvent,
    memory::FileActivity,
    model::ModelClientBackend,
};
use orbit_model::{
    client::{ApiFormat, ModelClient},
    types::{
        ChatMessage, ChatRequest, ModelConfig, ReasoningAdapter, ReasoningEffort, StreamEvent,
    },
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

mod history;
mod instructions;
mod marketplaces;
mod project_state;
mod runtime;
mod session;
mod skills;
mod tui;
mod verification;

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
    /// Summarize durable model, tool, cancellation, and compaction events.
    Diagnostics {
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
    /// Stream model text and lifecycle diagnostics while the run is active.
    #[arg(long, short)]
    stream: bool,

    /// Read the prompt from a UTF-8 file. Use - to read standard input.
    #[arg(long, value_name = "FILE", conflicts_with = "prompt")]
    prompt_file: Option<PathBuf>,

    /// Select a configured model. Use provider::model when names overlap.
    #[arg(long, value_name = "PROVIDER::MODEL")]
    model: Option<String>,

    /// Execution permission policy. auto runs sandbox commands; yolo also permits workspace edits.
    #[arg(long, value_enum, default_value_t = ChatMode::Auto)]
    mode: ChatMode,

    /// Restore a saved session in the current workspace.
    #[arg(long)]
    resume: Option<String>,

    /// Attach an image to this user message. May be supplied more than once.
    #[arg(long = "image", value_name = "PATH")]
    images: Vec<PathBuf>,

    /// Hide one capability category from this run. May be supplied more than once.
    #[arg(long = "disable", value_enum, value_name = "CAPABILITY")]
    disabled: Vec<ChatCapability>,

    /// Emit one SWE-bench prediction JSON object for this instance instead of prose.
    #[arg(long, value_name = "INSTANCE_ID", conflicts_with = "stream")]
    swe_instance_id: Option<String>,

    /// The task prompt. Omit it only when --prompt-file is supplied.
    prompt: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum ChatMode {
    /// Register only read-only tools.
    #[value(name = "read-only")]
    ReadOnly,
    /// Automatically run sandbox commands; reject workspace edits.
    #[default]
    Auto,
    /// Automatically run sandbox commands and permit workspace edits.
    #[value(alias = "YOLO")]
    Yolo,
}

impl ChatMode {
    fn allows(self, preview: &orbit_tools::approval::ApprovalPreview) -> bool {
        match self {
            Self::ReadOnly => false,
            Self::Auto => matches!(preview, orbit_tools::approval::ApprovalPreview::Command(_)),
            Self::Yolo => true,
        }
    }
}

/// A capability group that can be removed before Orbit creates the model
/// request. Names are intentionally broad, so a CI caller can make a simple,
/// auditable policy without needing to track individual tool names.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, ValueEnum)]
enum ChatCapability {
    /// Every optional tool group below.
    All,
    /// web_search and web_fetch.
    Web,
    /// Configured local MCP processes and their tools.
    Mcp,
    /// Installed skill catalogue and load_skill.
    Skills,
    /// Native image attachments and read_image.
    Image,
    /// read_document.
    Document,
    /// read_file, list_files, search_text and project_overview.
    Files,
    /// git_inspect.
    Git,
    /// write_file and edit_file.
    Write,
    /// run_command, read_command_output, verification_status and export_artifact.
    Command,
    /// Archived context retrieval and tool-output pruning.
    History,
    /// task and durable-memory tools.
    #[value(name = "project-state")]
    ProjectState,
}

fn capability_is_disabled(disabled: &BTreeSet<ChatCapability>, capability: ChatCapability) -> bool {
    disabled.contains(&ChatCapability::All) || disabled.contains(&capability)
}

#[derive(Serialize)]
struct SwePrediction<'a> {
    instance_id: &'a str,
    model_name_or_path: &'a str,
    model_patch: String,
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
    #[serde(default = "default_max_turns")]
    max_turns: usize,
    #[serde(default = "default_max_tool_calls")]
    max_tool_calls: usize,
    #[serde(default)]
    command: orbit_tools::run_command::CommandLimits,
    #[serde(default)]
    compaction: orbit_agent::compaction::CompactionSettings,
    #[serde(default)]
    web: orbit_tools::web_fetch::WebConfig,
    #[serde(default)]
    mcp: orbit_tools::mcp::McpConfig,
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
    /// Number of retries after a failed request that has not produced headers.
    #[serde(default = "orbit_model::types::default_max_retries")]
    max_retries: u8,
}

#[derive(Clone, Debug, Deserialize)]
struct ModelProfile {
    id: String,
    /// Overrides the top-level context window for this model only.
    context_tokens: Option<usize>,
    /// Full usable input window for providers that allow output outside this
    /// input limit. When omitted, `context_tokens` keeps its total-budget
    /// meaning and Orbit reserves the configured output upper bound from it.
    input_context_tokens: Option<usize>,
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

fn default_max_turns() -> usize {
    75
}

fn default_max_tool_calls() -> usize {
    500
}

fn validate_reasoning_extra(model: &str, api_format: ApiFormat, extra: &toml::Table) -> Result<()> {
    // `extra` is for provider extensions. These fields are owned by Orbit's
    // request encoder; accepting them here would silently replace the model,
    // transport shape, output budget, or tool contract.
    for key in [
        "model",
        "messages",
        "input",
        "stream",
        "tools",
        "tool_choice",
        "max_tokens",
        "max_output_tokens",
        "temperature",
    ] {
        if extra.contains_key(key) {
            bail!(
                "model {model:?}: reasoning.extra may not override the standard request field {key:?}"
            );
        }
    }
    let conflicts_effort = match api_format {
        ApiFormat::OpenaiChatCompletions => extra.contains_key("reasoning_effort"),
        ApiFormat::OpenaiResponses => extra
            .get("reasoning")
            .and_then(toml::Value::as_table)
            .is_some_and(|reasoning| reasoning.contains_key("effort")),
        ApiFormat::AnthropicMessages => extra
            .get("output_config")
            .and_then(toml::Value::as_table)
            .is_some_and(|output| output.contains_key("effort")),
    };
    if conflicts_effort {
        bail!("model {model:?}: reasoning.extra may not override the configured reasoning effort");
    }
    Ok(())
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
                || provider.max_retries > 5
                || provider
                    .models
                    .iter()
                    .any(|model| model.id.trim().is_empty())
            {
                bail!(
                    "provider {name:?} must set nonempty base_url, api_key, models, a positive timeout_seconds, and max_retries from 0 to 5"
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
                    validate_reasoning_extra(&model.id, provider.api_format, &reasoning.extra)?;
                }
                let input_context_tokens = model.input_context_tokens.unwrap_or_else(|| {
                    model
                        .context_tokens
                        .unwrap_or(config.context_tokens)
                        .saturating_sub(config.max_output_tokens as usize)
                });
                if input_context_tokens < 8192 {
                    bail!(
                        "model {:?}: input context window must be at least 8192 tokens",
                        model.id
                    );
                }
            }
        }
        config.compaction.validate().map_err(anyhow::Error::msg)?;
        if let Some(name) = &config.compaction.model {
            anyhow::ensure!(
                config
                    .model_options()?
                    .iter()
                    .any(|m| &m.qualified_name() == name),
                "compaction.model must name a configured provider::model"
            );
        }
        config.command.validate().map_err(anyhow::Error::msg)?;
        config.mcp.validate()?;
        Ok(config)
    }

    fn compaction_runtime(&self) -> Result<orbit_agent::compaction::CompactionRuntime> {
        let mut runtime = orbit_agent::compaction::CompactionRuntime {
            settings: self.compaction.clone(),
            backend: None,
        };
        if let Some(name) = &self.compaction.model {
            let choice = self
                .model_options()?
                .into_iter()
                .find(|m| &m.qualified_name() == name)
                .ok_or_else(|| {
                    anyhow::anyhow!("compaction.model must name a configured provider::model")
                })?;
            let entry = self.providers[&choice.provider]
                .models
                .iter()
                .find(|m| m.id == choice.model)
                .unwrap();
            let input = entry.input_context_tokens.unwrap_or_else(|| {
                entry
                    .context_tokens
                    .unwrap_or(self.context_tokens)
                    .saturating_sub(runtime.settings.max_output_tokens)
            });
            runtime.settings.max_input_tokens = runtime.settings.max_input_tokens.min(input);
            runtime.backend = Some(std::sync::Arc::new(ModelClientBackend::new(
                ModelClient::new(choice.config)?,
            )));
        }
        Ok(runtime)
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
                let input_context_tokens = model.input_context_tokens.unwrap_or_else(|| {
                    model
                        .context_tokens
                        .unwrap_or(self.context_tokens)
                        .saturating_sub(self.max_output_tokens as usize)
                });
                let context_tokens = model.input_context_tokens.map_or_else(
                    || model.context_tokens.unwrap_or(self.context_tokens),
                    |input| input.saturating_add(self.max_output_tokens as usize),
                );
                let reasoning = model.reasoning.as_ref();
                options.push(tui::ModelOption {
                    provider: name.clone(),
                    model: model.id.clone(),
                    context_tokens,
                    input_context_tokens,
                    default_for_provider: index == 0,
                    reasoning_efforts: reasoning
                        .map_or_else(Vec::new, |profile| profile.efforts.clone()),
                    config: ModelConfig {
                        api_format: provider.api_format,
                        base_url: provider.base_url.clone(),
                        api_key: Some(provider.api_key.clone()),
                        model: model.id.clone(),
                        timeout: Duration::from_secs(provider.timeout_seconds),
                        max_retries: provider.max_retries,
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
                SessionCommand::Diagnostics { id } => println!(
                    "{}",
                    orbit_store::diagnostics::text(&store.diagnostics(&id).await?)
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
    let mcp = file_config.mcp.clone();
    let max_output_tokens = file_config.max_output_tokens;
    let model_name = default_model.qualified_name();
    let context_tokens = default_model.context_tokens;
    let input_context_tokens = default_model.input_context_tokens;
    let client = ModelClient::new(default_model.config)?;
    let mut session =
        session::SessionState::open(database, args.resume, model_name.clone()).await?;
    let enabled_plugins = session
        .store
        .project_plugins(&session.snapshot.session.project_id)
        .await?;
    let plugin_skill_roots = crate::marketplaces::enabled_skill_roots(&enabled_plugins);
    let skills = skills::Skills::discover_current_with_plugin_roots(plugin_skill_roots.roots);
    let undo_writer =
        runtime::workspace_writer(&config_path, session.store.path(), approvals.clone())?;
    let runtime = runtime::workspace_tools(
        &config_path,
        Some(session.store.path()),
        Some(approvals),
        command_limits,
        web,
        mcp,
        Some(session.scratch.clone()),
        Some(session.command_output.clone()),
    )
    .await?;
    session
        .initialize_audit(runtime.workspace.clone(), runtime.command_limits.clone())
        .await?;
    let audit_workspace = runtime.workspace;
    let mut tools = runtime.tools;
    let memory_scope = project_state::MemoryScope::new(session.id().into());
    verification::register(
        &mut tools,
        session.store.clone(),
        memory_scope.clone(),
        audit_workspace,
        runtime.command_limits,
    );
    history::register(&mut tools, session.store.clone(), memory_scope.clone());
    project_state::register(
        &mut tools,
        session.store.clone(),
        session.snapshot.session.project_id.clone(),
        memory_scope.clone(),
    );
    skills.register_tool(&mut tools);
    tui::run(
        client,
        model_name,
        tools,
        session,
        undo_writer,
        approval_receiver,
        tui::RequestBudget {
            context_tokens,
            input_context_tokens,
            max_output_tokens,
            auto_compact: agent_config.auto_compact,
            max_turns: agent_config.max_turns,
            max_tool_calls: agent_config.max_tool_calls,
            compaction: file_config.compaction_runtime()?,
            instructions,
            model_options,
            skills,
            plugin_skill_diagnostics: plugin_skill_roots.diagnostics,
            mcp_statuses: runtime.mcp_statuses,
            memory_scope,
        },
    )
    .await
}

async fn chat(config_path: PathBuf, database: Option<PathBuf>, args: ChatArgs) -> Result<()> {
    let prompt = read_chat_prompt(&args)?;
    let disabled = args.disabled.iter().copied().collect::<BTreeSet<_>>();
    if capability_is_disabled(&disabled, ChatCapability::Image) && !args.images.is_empty() {
        bail!("--image cannot be used when the image capability is disabled");
    }
    let file_config = FileConfig::load(&config_path)?;
    let database = session::database_path(database)?;

    // Open the persistent conversation before choosing a resumed model. A new
    // run starts with the configured default, while a resumed run keeps its
    // prior configured provider/model unless the caller explicitly overrides it.
    let default_model = file_config.default_model_option()?;
    let mut session = session::SessionState::open(
        database,
        args.resume.clone(),
        default_model.qualified_name(),
    )
    .await?;
    let saved_model = args
        .resume
        .as_ref()
        .map(|_| session.snapshot.session.model.clone());
    let selected_model = select_chat_model(
        &file_config,
        args.model.as_deref().or(saved_model.as_deref()),
    )?;
    let model_name = selected_model.qualified_name();
    let agent_config = file_config.agent_config_for(selected_model.context_tokens);
    let client = ModelClient::new(selected_model.config.clone())?;
    session
        .set_model_profile(model_name.clone(), client.config().reasoning_effort)
        .await?;

    let skills = if capability_is_disabled(&disabled, ChatCapability::Skills) {
        skills::Skills::default()
    } else {
        let enabled_plugins = session
            .store
            .project_plugins(&session.snapshot.session.project_id)
            .await?;
        let plugin_skill_roots = marketplaces::enabled_skill_roots(&enabled_plugins);
        skills::Skills::discover_current_with_plugin_roots(plugin_skill_roots.roots)
    };
    let instructions = instructions::load_current();
    let attachments = args
        .images
        .iter()
        .map(|path| session.import_image(path))
        .collect::<Result<Vec<_>>>()?;

    let (approvals, approval_task) = match args.mode {
        ChatMode::ReadOnly => (None, None),
        mode => {
            let (sender, mut receiver) = orbit_tools::approval::channel();
            let task = tokio::spawn(async move {
                while let Some(request) = receiver.recv().await {
                    let _ = request.decision.send(mode.allows(&request.preview));
                }
            });
            (Some(sender), Some(task))
        }
    };
    let runtime = runtime::workspace_tools(
        &config_path,
        Some(session.store.path()),
        approvals,
        file_config.command.clone(),
        file_config.web.clone(),
        if capability_is_disabled(&disabled, ChatCapability::Mcp) {
            orbit_tools::mcp::McpConfig::default()
        } else {
            file_config.mcp.clone()
        },
        Some(session.scratch.clone()),
        Some(session.command_output.clone()),
    )
    .await?;
    if !capability_is_disabled(&disabled, ChatCapability::Files)
        && !capability_is_disabled(&disabled, ChatCapability::Command)
    {
        session
            .initialize_audit(runtime.workspace.clone(), runtime.command_limits.clone())
            .await?;
    }
    let audit_workspace = runtime.workspace;
    let mut tools = runtime.tools;
    verification::register(
        &mut tools,
        session.store.clone(),
        project_state::MemoryScope::new(session.id().into()),
        audit_workspace,
        runtime.command_limits,
    );
    if !capability_is_disabled(&disabled, ChatCapability::History) {
        history::register(
            &mut tools,
            session.store.clone(),
            project_state::MemoryScope::new(session.id().into()),
        );
    }
    if !capability_is_disabled(&disabled, ChatCapability::ProjectState) {
        let memory_scope = project_state::MemoryScope::new(session.id().into());
        project_state::register(
            &mut tools,
            session.store.clone(),
            session.snapshot.session.project_id.clone(),
            memory_scope,
        );
    }
    if !capability_is_disabled(&disabled, ChatCapability::Skills) {
        skills.register_tool(&mut tools);
    }
    disable_chat_capabilities(&mut tools, &disabled);
    if capability_is_disabled(&disabled, ChatCapability::Files) {
        tools.remove("verification_status");
    }

    let mut messages = session.snapshot.history.clone();
    instructions::inject(&mut messages, &instructions);
    if !capability_is_disabled(&disabled, ChatCapability::Skills) {
        skills.inject(&mut messages);
    }
    if !capability_is_disabled(&disabled, ChatCapability::ProjectState) {
        project_state::inject(
            &mut messages,
            &session.store,
            &session.snapshot.session.project_id,
            session.id(),
        )
        .await?;
    }
    messages.push(ChatMessage::user_with_attachments(
        prompt.clone(),
        attachments,
    ));
    let mut request = ChatRequest::new(messages);
    request.max_tokens = Some(file_config.max_output_tokens);
    request.reasoning_effort = client.config().reasoning_effort;

    let files = FileActivity::from_events(
        session
            .snapshot
            .runs
            .iter()
            .flat_map(|run| run.events.iter()),
    );
    let archive_enabled = tools.contains("read_history");
    let mut agent = Agent::new(ModelClientBackend::new(client), tools, agent_config)
        .with_file_activity(files)
        .with_compaction(file_config.compaction_runtime()?);
    if archive_enabled {
        agent = agent.with_context_archive(std::sync::Arc::new(history::SessionArchive::new(
            session.store.clone(),
            session.id().into(),
        )));
    }
    session.begin(prompt, model_name.clone()).await?;
    agent = agent
        .with_calibration(model_name.clone(), session.calibration(&model_name))
        .with_execution_journal(session.execution_journal()?);

    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(256);
    let task = tokio::spawn(async move {
        agent
            .run_with_agent_events(request, task_cancellation, sender)
            .await
    });

    let mut interrupted = false;
    loop {
        let event = tokio::select! {
            event = receiver.recv() => event,
            signal = tokio::signal::ctrl_c(), if !interrupted => {
                signal.context("failed to listen for Ctrl+C")?;
                interrupted = true;
                cancellation.cancel();
                eprintln!("正在取消…");
                continue;
            }
        };
        let Some(event) = event else { break };
        session.record(&event);
        session.flush_due().await?;
        report_chat_event(&event, args.stream)?;
    }

    let outcome = match task.await {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some(task) = approval_task {
                task.abort();
            }
            session.finish("failed", None).await?;
            return Err(error.into());
        }
    };
    if let Some(task) = approval_task {
        task.abort();
    }
    let result = match outcome {
        Ok(result) => result,
        Err(AgentError::Cancelled) if interrupted => {
            session.finish("cancelled", None).await?;
            eprintln!("已取消。");
            return Ok(());
        }
        Err(error) => {
            session.finish("failed", None).await?;
            return Err(error.into());
        }
    };
    let answer = match &result.final_message {
        ChatMessage::Assistant { content, .. } => content.clone().unwrap_or_default(),
        _ => String::new(),
    };
    session.finish("completed", Some(result.history)).await?;

    if let Some(instance_id) = &args.swe_instance_id {
        let patch = orbit_tools::git_inspect::working_tree_patch(Path::new(
            &session.snapshot.session.workspace,
        ))?;
        println!(
            "{}",
            serde_json::to_string(&SwePrediction {
                instance_id,
                model_name_or_path: &model_name,
                model_patch: patch,
            })?
        );
    } else if args.stream {
        println!();
    } else {
        println!("{answer}");
    }

    Ok(())
}

fn read_chat_prompt(args: &ChatArgs) -> Result<String> {
    let prompt = match (&args.prompt, &args.prompt_file) {
        (Some(_), Some(_)) => bail!("provide either a prompt or --prompt-file, not both"),
        (Some(prompt), None) => prompt.clone(),
        (None, Some(path)) if path == Path::new("-") => {
            let mut prompt = String::new();
            io::stdin()
                .read_to_string(&mut prompt)
                .context("failed to read prompt from standard input")?;
            prompt
        }
        (None, Some(path)) => fs::read_to_string(path)
            .with_context(|| format!("failed to read prompt file {}", path.display()))?,
        (None, None) => bail!("provide a prompt or --prompt-file"),
    };
    if prompt.trim().is_empty() {
        bail!("prompt must not be empty");
    }
    Ok(prompt)
}

fn select_chat_model(config: &FileConfig, requested: Option<&str>) -> Result<tui::ModelOption> {
    let Some(requested) = requested else {
        return config.default_model_option();
    };
    let requested = requested.trim();
    if requested.is_empty() {
        bail!("model must not be empty");
    }
    let options = config.model_options()?;
    if requested.contains("::") {
        return options
            .into_iter()
            .find(|option| option.qualified_name() == requested)
            .ok_or_else(|| anyhow::anyhow!("model {requested:?} is not configured"));
    }
    let matches = options
        .into_iter()
        .filter(|option| option.model == requested)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [option] => Ok(option.clone()),
        [] => bail!("model {requested:?} is not configured"),
        _ => bail!("model {requested:?} is configured by multiple providers; use provider::model"),
    }
}

fn disable_chat_capabilities(
    tools: &mut orbit_agent::tool::ToolRegistry,
    disabled: &BTreeSet<ChatCapability>,
) {
    if capability_is_disabled(disabled, ChatCapability::History) {
        tools.remove("read_history");
    }
    if capability_is_disabled(disabled, ChatCapability::Web) {
        tools.remove("web_search");
        tools.remove("web_fetch");
    }
    if capability_is_disabled(disabled, ChatCapability::Mcp) {
        tools.remove_prefix("mcp__");
    }
    if capability_is_disabled(disabled, ChatCapability::Skills) {
        tools.remove("load_skill");
    }
    if capability_is_disabled(disabled, ChatCapability::Image) {
        tools.remove("read_image");
    }
    if capability_is_disabled(disabled, ChatCapability::Document) {
        tools.remove("read_document");
    }
    if capability_is_disabled(disabled, ChatCapability::Files) {
        for name in [
            "read_file",
            "list_files",
            "search_text",
            "project_overview",
            "verification_status",
        ] {
            tools.remove(name);
        }
    }
    if capability_is_disabled(disabled, ChatCapability::Git) {
        tools.remove("git_inspect");
    }
    if capability_is_disabled(disabled, ChatCapability::Write) {
        tools.remove("write_file");
        tools.remove("edit_file");
    }
    if capability_is_disabled(disabled, ChatCapability::Command) {
        tools.remove("run_command");
        tools.remove("read_command_output");
        tools.remove("export_artifact");
        tools.remove("verification_status");
    }
    if capability_is_disabled(disabled, ChatCapability::ProjectState) {
        for name in [
            "task_list",
            "task_update",
            "memory_list",
            "memory_search",
            "memory_save",
            "memory_forget",
        ] {
            tools.remove(name);
        }
    }
}

fn report_chat_event(event: &AgentEvent, stream: bool) -> Result<()> {
    if stream {
        match event {
            AgentEvent::Model(StreamEvent::TextDelta(text))
            | AgentEvent::Model(StreamEvent::ReasoningDelta(text)) => {
                print!("{text}");
                io::stdout().flush()?;
            }
            AgentEvent::ToolStarted { call } => {
                eprintln!("tool started: {}", call.function.name)
            }
            AgentEvent::ToolFailed { call, error } => {
                eprintln!("tool failed: {}: {error}", call.function.name)
            }
            _ => {}
        }
    }
    match event {
        AgentEvent::CompactionStarted => {}
        AgentEvent::ContextCompacted {
            before_tokens,
            after_tokens,
            strategy,
            fallback,
            ..
        } => eprintln!(
            "{}：{before_tokens} → {after_tokens} tokens{}",
            match strategy {
                orbit_agent::memory::CompactionStrategy::ToolPruning => "旧工具输出已精简",
                orbit_agent::memory::CompactionStrategy::Summary => "上下文已整理",
            },
            if *fallback {
                "；保留最近工作内容"
            } else {
                ""
            }
        ),
        AgentEvent::CompactionFailed { .. } => eprintln!("上下文整理未完成，原上下文已保留"),
        AgentEvent::Failed { error } => eprintln!("agent failed: {error}"),
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod config_tests {
    use super::*;
    use orbit_agent::tool::{ClosureTool, ToolRegistry};

    #[test]
    fn chat_capability_filter_removes_groups_before_request_assembly() {
        let mut tools = ToolRegistry::new();
        for name in [
            "read_file",
            "project_overview",
            "write_file",
            "web_search",
            "web_fetch",
            "mcp__test__lookup",
            "task_list",
            "read_history",
        ] {
            tools.register(ClosureTool::new(
                name,
                "test",
                serde_json::json!({}),
                |_, _| async { Ok::<_, orbit_agent::tool::ToolError>("ok".into()) },
            ));
        }
        let disabled = [
            ChatCapability::Files,
            ChatCapability::Web,
            ChatCapability::Mcp,
            ChatCapability::ProjectState,
            ChatCapability::History,
        ]
        .into_iter()
        .collect();
        disable_chat_capabilities(&mut tools, &disabled);
        assert!(!tools.contains("read_file"));
        assert!(!tools.contains("project_overview"));
        assert!(!tools.contains("web_search"));
        assert!(!tools.contains("web_fetch"));
        assert!(!tools.contains("mcp__test__lookup"));
        assert!(!tools.contains("task_list"));
        assert!(!tools.contains("read_history"));
        assert!(tools.contains("write_file"));
    }

    #[test]
    fn disabled_commands_hide_audit_and_export_tools() {
        let mut tools = ToolRegistry::new();
        for name in [
            "run_command",
            "read_command_output",
            "verification_status",
            "export_artifact",
        ] {
            tools.register(ClosureTool::new(
                name,
                "",
                serde_json::json!({}),
                |_, _| async { Ok::<_, orbit_agent::tool::ToolError>("unused".into()) },
            ));
        }
        disable_chat_capabilities(&mut tools, &[ChatCapability::Command].into_iter().collect());
        assert!(tools.definitions().is_empty());
    }

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
        assert_eq!(config.providers["test"].max_retries, 2);
        assert_eq!(config.agent_config().max_turns, 75);
        assert_eq!(config.agent_config().max_tool_calls, 500);
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
    fn batch_model_selection_accepts_qualified_names_and_rejects_ambiguity() {
        let config: FileConfig = toml::from_str(
            r#"default_provider = "one"
[providers.one]
base_url = "https://one.example/v1"
api_key = "one-key"
[[providers.one.models]]
id = "shared"
[[providers.one.models]]
id = "one-only"

[providers.two]
base_url = "https://two.example/v1"
api_key = "two-key"
[[providers.two.models]]
id = "shared"
"#,
        )
        .unwrap();
        assert_eq!(
            select_chat_model(&config, Some("two::shared"))
                .unwrap()
                .qualified_name(),
            "two::shared"
        );
        assert_eq!(
            select_chat_model(&config, Some("one-only"))
                .unwrap()
                .qualified_name(),
            "one::one-only"
        );
        assert!(select_chat_model(&config, Some("shared")).is_err());
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
    fn reasoning_extra_cannot_replace_standard_request_fields() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(
            &path,
            r#"default_provider = "openai"
[providers.openai]
api_format = "openai_responses"
base_url = "https://openai.example/v1"
api_key = "openai-key"
[[providers.openai.models]]
id = "reasoning-model"
reasoning = { adapter = "openai_responses", efforts = ["low", "high"], default = "high", extra = { stream = false } }
"#,
        )
        .unwrap();
        let error = FileConfig::load(&path).unwrap_err().to_string();
        assert!(error.contains("reasoning.extra") && error.contains("stream"));
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
[[providers.test.models]]
id = "separate-input-window"
input_context_tokens = 224000
"#,
        )
        .unwrap();
        let options = config.model_options().unwrap();
        assert_eq!(options[0].context_tokens, 64_000);
        assert_eq!(options[0].input_context_tokens, 32_000);
        assert_eq!(options[1].context_tokens, 128_000);
        assert_eq!(options[1].input_context_tokens, 96_000);
        assert_eq!(options[2].input_context_tokens, 224_000);
        assert_eq!(options[2].context_tokens, 256_000);
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
