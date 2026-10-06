use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use orbit_agent::events::AgentEvent;
use orbit_model::types::StreamEvent;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use orbit_client as workspace;
use orbit_service::models::model_metadata;
use orbit_service::session;

mod auth_cli;
mod connect;
mod connect_args;

#[derive(Debug, Parser)]
#[command(name = "orbit-cli")]
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
    /// Connect to a local, SSH, or WSL workspace service.
    Connect(connect_args::ConnectArgs),
    #[command(hide = true)]
    WorkspaceService(connect_args::ServiceArgs),
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
    /// Automatically approve tool requests.
    #[value(alias = "YOLO")]
    Yolo,
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

#[derive(Serialize)]
struct SwePrediction<'a> {
    instance_id: &'a str,
    model_name_or_path: &'a str,
    model_patch: String,
}

fn main() -> Result<()> {
    workspace::register_server_bundle(include_bytes!(env!("ORBIT_EMBEDDED_SERVER")))?;

    if std::env::args().nth(1).as_deref() == Some("--internal-ssh-askpass") {
        orbit_client::interaction::askpass()?;
        return Ok(());
    }
    if orbit_service::platform::workers::run()? {
        return Ok(());
    }

    run_cli()
}

#[tokio::main]
async fn run_cli() -> Result<()> {
    let cli = Cli::parse();

    let auth_directory = || {
        orbit_service::platform::directory(
            cli.auth_dir.clone(),
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        )
    };

    match cli.command {
        Command::Auth { command } => auth_cli::run(command, auth_directory()?).await,
        Command::Chat(args) => chat(cli.config, cli.database, args, auth_directory()?).await,
        Command::Connect(args) => {
            connect::connect(args, cli.config, cli.database, cli.auth_dir).await
        }
        Command::WorkspaceService(args) => {
            orbit_service::workspace::serve(
                args.into(),
                cli.config,
                cli.database,
                auth_directory()?,
            )
            .await
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

async fn chat(
    config: PathBuf,
    database: Option<PathBuf>,
    args: ChatArgs,
    auth: PathBuf,
) -> Result<()> {
    let options = orbit_service::chat::ChatOptions {
        workspace: std::env::current_dir()?,
        prompt: read_chat_prompt(&args)?,
        model: args.model.clone(),
        mode: args.mode.into(),
        resume: args.resume.clone(),
        allow_mcp_start: args.allow_mcp_start,
        images: args.images.clone(),
        disabled: args.disabled.iter().copied().map(Into::into).collect(),
        export_patch: args.swe_instance_id.is_some(),
    };
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    let signals = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            token.cancel();
            eprintln!("正在取消…");
            if tokio::signal::ctrl_c().await.is_ok() {
                std::process::exit(130);
            }
        }
    });
    let signals = tokio_util::task::AbortOnDropHandle::new(signals);
    let outcome =
        orbit_service::chat::run(config, database, options, auth, cancellation, |update| {
            match update {
                orbit_service::chat::ChatUpdate::Agent(event) => {
                    report_chat_event(event, args.stream)
                }
                orbit_service::chat::ChatUpdate::Diagnostic(message) => {
                    eprintln!("{message}");
                    Ok(())
                }
            }
        })
        .await?;
    drop(signals);
    match outcome {
        orbit_service::chat::ChatOutcome::Cancelled => eprintln!("已取消。"),
        orbit_service::chat::ChatOutcome::Completed {
            answer,
            model_name,
            patch,
        } => {
            if let Some(instance_id) = &args.swe_instance_id {
                println!(
                    "{}",
                    serde_json::to_string(&SwePrediction {
                        instance_id,
                        model_name_or_path: &model_name,
                        model_patch: patch.unwrap_or_default()
                    })?
                );
            } else if args.stream {
                println!();
            } else {
                println!("{answer}");
            }
        }
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

impl From<ChatMode> for orbit_service::chat::ChatMode {
    fn from(value: ChatMode) -> Self {
        match value {
            ChatMode::ReadOnly => Self::ReadOnly,
            ChatMode::Auto => Self::Auto,
            ChatMode::Yolo => Self::Yolo,
        }
    }
}

impl From<ChatCapability> for orbit_service::chat::ChatCapability {
    fn from(value: ChatCapability) -> Self {
        match value {
            ChatCapability::All => Self::All,
            ChatCapability::Subagents => Self::Subagents,
            ChatCapability::Web => Self::Web,
            ChatCapability::Mcp => Self::Mcp,
            ChatCapability::Skills => Self::Skills,
            ChatCapability::Image => Self::Image,
            ChatCapability::Document => Self::Document,
            ChatCapability::Files => Self::Files,
            ChatCapability::Git => Self::Git,
            ChatCapability::Write => Self::Write,
            ChatCapability::Capability => Self::Capability,
            ChatCapability::Command => Self::Command,
            ChatCapability::History => Self::History,
            ChatCapability::ProjectState => Self::ProjectState,
        }
    }
}
