use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use orbit_agent::{
    agent::Agent, error::AgentError, events::AgentEvent, memory::FileActivity,
    model::ModelClientBackend,
};
use orbit_model::{
    client::ModelClient,
    types::{ChatMessage, ChatRequest, StreamEvent},
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use self::config::FileConfig;
pub(crate) use workspace::setup::{
    PreparedWorkspace, WorkspaceOptions, WorkspaceResources, prepare_workspace,
    prepare_workspace_resources,
};

mod account_models;
mod auth_cli;
mod config;
mod connections;
mod history;
mod instructions;
mod marketplaces;
mod model_metadata;
mod project_state;
mod runtime;
mod session;
mod session_title;
mod skills;
mod subagents;
mod tui;
mod verification;
mod workspace;

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

    /// Private credentials directory (defaults to $XDG_CONFIG_HOME/orbit/auth).
    #[arg(long, global = true)]
    auth_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage saved API keys and experimental subscription credentials.
    Auth {
        #[command(subcommand)]
        command: auth_cli::AuthCommand,
    },
    /// Export the latest public model specifications for an offline build.
    ModelMetadata {
        output: PathBuf,
    },
    Chat(ChatArgs),
    Tui(TuiArgs),
    /// Connect to a local, SSH, or WSL workspace service.
    Connect(workspace::ConnectArgs),
    #[command(hide = true)]
    WorkspaceService(workspace::ServiceArgs),
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
    /// Show the stored payload size of each session before choosing what to delete.
    Storage,
    /// Delete a closed session, its canonical history, and private sidecar files.
    Delete {
        id: String,
        /// Confirm permanent deletion of this session.
        #[arg(long)]
        yes: bool,
    },
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

    /// Allow enabled local MCP server programs to start for this invocation.
    #[arg(long)]
    allow_mcp_start: bool,
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

    /// Execution policy: auto permits scoped workspace operations; yolo approves all requests.
    #[arg(long, value_enum, default_value_t = ChatMode::Auto)]
    mode: ChatMode,

    /// Restore a saved session in the current workspace.
    #[arg(long)]
    resume: Option<String>,

    /// Allow enabled local MCP server programs to start for this invocation.
    #[arg(long)]
    allow_mcp_start: bool,

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
    /// Automatically run operations inside the workspace.
    #[default]
    Auto,
    /// Automatically run sandbox commands and permit workspace edits.
    #[value(alias = "YOLO")]
    Yolo,
}

impl ChatMode {
    fn allows(
        self,
        preview: &orbit_tools::approval::ApprovalPreview,
        workspace: &std::path::Path,
    ) -> bool {
        match self {
            Self::ReadOnly => false,
            Self::Auto => orbit_tools::approval::auto_allows(preview, workspace),
            Self::Yolo => true,
        }
    }
}

/// Capability categories omitted from both tool registration and model requests.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, ValueEnum)]
enum ChatCapability {
    /// Every optional tool group below.
    All,
    /// Independent read-only delegated sessions.
    Subagents,
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
    /// git_inspect and git_export_patch.
    Git,
    /// write_file and edit_file.
    Write,
    /// request_capability for paths outside the workspace.
    Capability,
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

fn main() -> Result<()> {
    if std::env::args().nth(1).as_deref() == Some("--internal-document-worker") {
        let args: Vec<_> = std::env::args_os().skip(2).collect();
        anyhow::ensure!(
            args.len() == 2,
            "document worker requires input and output paths"
        );
        return orbit_tools::read_document::document_worker(
            Path::new(&args[0]),
            Path::new(&args[1]),
        )
        .map_err(anyhow::Error::msg);
    }

    if std::env::args().nth(1).as_deref() == Some("--internal-git-push-worker") {
        let args: Vec<_> = std::env::args_os().skip(2).collect();
        anyhow::ensure!(args.len() == 1, "push worker requires request path");
        return orbit_tools::git_push::push_worker(Path::new(&args[0])).map_err(anyhow::Error::msg);
    }

    run_cli()
}

#[tokio::main]
async fn run_cli() -> Result<()> {
    let cli = Cli::parse();

    let auth_directory = || {
        auth_cli::directory(
            cli.auth_dir.clone(),
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        )
    };

    match cli.command {
        Command::Auth { command } => auth_cli::run(command, auth_directory()?).await,
        Command::Chat(args) => chat(cli.config, cli.database, args, auth_directory()?).await,
        Command::Tui(args) => run_tui(cli.config, cli.database, args, auth_directory()?).await,
        Command::Connect(args) => {
            workspace::connect(args, cli.config, cli.database, cli.auth_dir).await
        }
        Command::WorkspaceService(args) => {
            workspace::serve(args, cli.config, cli.database, auth_directory()?).await
        }

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

        Command::ModelMetadata { output } => {
            let web = match std::fs::read_to_string(&cli.config) {
                Ok(text) => toml::from_str::<toml::Table>(&text)?
                    .remove("web")
                    .map(toml::Value::try_into)
                    .transpose()?
                    .unwrap_or_default(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Default::default(),
                Err(error) => return Err(error.into()),
            };
            model_metadata::export(&output, &web).await?;
            println!("已更新模型规格快照：{}", output.display());
            Ok(())
        }

        Command::Sessions { command } => {
            let store =
                orbit_store::store::Store::open(session::database_path(cli.database)?).await?;

            match command {
                SessionCommand::List => println!("{}", session::list_text(&store).await?),
                SessionCommand::Storage => {
                    for usage in store.session_storage_usage().await? {
                        println!(
                            "{}  {:>12} B  {}",
                            usage.id,
                            usage.payload_bytes,
                            usage.title.replace(['\n', '\r', '\t'], " ")
                        );
                    }
                }

                SessionCommand::Delete { id, yes } => {
                    anyhow::ensure!(yes, "永久删除会话需要 --yes；请先确认会话 ID。");
                    let deleted = store.delete_session(&id).await?;

                    if let Err(error) = store.reclaim_unused_space().await {
                        eprintln!("会话已删除，但数据库空间回收失败：{error}");
                    }

                    println!(
                        "{}",
                        if deleted {
                            "会话及私有文件已删除。"
                        } else {
                            "会话不存在；同 ID 的私有残留文件已清理。"
                        }
                    );
                }

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

async fn chat(
    config_path: PathBuf,
    database: Option<PathBuf>,
    args: ChatArgs,
    auth_directory: PathBuf,
) -> Result<()> {
    let prompt = read_chat_prompt(&args)?;
    let disabled = args.disabled.iter().copied().collect::<BTreeSet<_>>();

    if capability_is_disabled(&disabled, ChatCapability::Image) && !args.images.is_empty() {
        bail!("--image cannot be used when the image capability is disabled");
    }

    let mut file_config = FileConfig::load(&config_path, &auth_directory)?;
    for diagnostic in account_models::refresh(&mut file_config, false).await {
        eprintln!("{diagnostic}");
    }
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

    if !selected_model.supports_vision && !args.images.is_empty() {
        bail!(
            "--image cannot be used because model {:?} does not support image input",
            selected_model.qualified_name()
        );
    }

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
            let approval_workspace = std::path::PathBuf::from(&session.snapshot.session.workspace);
            let task = tokio::spawn(async move {
                while let Some(request) = receiver.recv().await {
                    let _ = request
                        .decision
                        .send(mode.allows(&request.preview, &approval_workspace));
                }
            });

            (Some(sender), Some(task))
        }
    };

    let runtime = runtime::workspace_tools(
        &std::env::current_dir()?,
        &config_path,
        &auth_directory,
        Some(session.store.path()),
        approvals,
        file_config.command.clone(),
        file_config.web.clone(),
        if capability_is_disabled(&disabled, ChatCapability::Mcp) {
            orbit_tools::mcp::McpConfig::default()
        } else {
            file_config.mcp.clone()
        },
        args.allow_mcp_start,
        Some(session.scratch.clone()),
        Some(session.command_output.clone()),
        Some(file_config.permissions.clone()),
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
        project_state::register(&mut tools, session.store.clone(), memory_scope);
    }

    if !capability_is_disabled(&disabled, ChatCapability::Skills) {
        skills.register_tool(&mut tools);
    }

    disable_chat_capabilities(&mut tools, &disabled);

    if !selected_model.supports_vision {
        tools.remove("read_image");
    }

    if capability_is_disabled(&disabled, ChatCapability::Files) {
        tools.remove("verification_status");
    }

    if session.store.is_subagent(session.id()).await? {
        subagents::restrict_child_tools(&mut tools);
    } else if !capability_is_disabled(&disabled, ChatCapability::Subagents) {
        subagents::register(
            &mut tools,
            session.store.clone(),
            session.id().into(),
            config_path.clone(),
            auth_directory.clone(),
            instructions.clone(),
            file_config.subagents.clone(),
        )
        .await?;
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
    request.max_tokens = Some(selected_model.max_output_tokens);
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
            signal = tokio::signal::ctrl_c() => {
                signal.context("failed to listen for Ctrl+C")?;

                if interrupted {
                    std::process::exit(130);
                }

                interrupted = true;
                cancellation.cancel();
                eprintln!("正在取消…");
                continue;
            }
        };
        let Some(event) = event else { break };

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
    if capability_is_disabled(disabled, ChatCapability::Subagents) {
        for name in [
            "spawn_agent",
            "send_agent_message",
            "wait_agents",
            "read_agent",
            "cancel_agents",
        ] {
            tools.remove(name);
        }
    }

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
        tools.remove("git_export_patch");
    }

    if capability_is_disabled(disabled, ChatCapability::Write) {
        tools.remove("write_file");
        tools.remove("edit_file");
    }

    if capability_is_disabled(disabled, ChatCapability::Capability) {
        tools.remove("request_capability");
    }

    if capability_is_disabled(disabled, ChatCapability::Command) {
        tools.remove("run_command");
        tools.remove("read_command_output");
        tools.remove("get_command_status");
        tools.remove("wait_commands");
        tools.remove("cancel_commands");
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
            AgentEvent::ToolBackgrounded { job_id, .. } => {
                eprintln!("command backgrounded: {job_id}")
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
                orbit_agent::memory::CompactionStrategy::ArchiveProjection => {
                    "旧历史已归档，摘要未完成"
                }
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
mod config_tests;
