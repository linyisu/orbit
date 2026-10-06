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
use orbit_config::models::model_metadata;

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

    run_cli()
}

#[tokio::main]
async fn run_cli() -> Result<()> {
    let cli = Cli::parse();

    let auth_directory = || {
        orbit_auth::directory(
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
        Command::Projects { command } => {
            use orbit_protocol::admin::Action;
            let action = match command {
                ProjectCommand::List => Action::Projects,
                ProjectCommand::Link { id } => Action::LinkProject { id },
            };
            admin(cli.config, cli.database, cli.auth_dir, action).await
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
            use orbit_protocol::admin::{Action, SessionSection};
            let action = match command {
                SessionCommand::List => Action::Sessions,
                SessionCommand::Storage => Action::SessionStorage,
                SessionCommand::Delete { id, yes } => {
                    anyhow::ensure!(yes, "永久删除会话需要 --yes；请先确认会话 ID。");
                    Action::DeleteSession { id }
                }
                SessionCommand::Show { id } => Action::InspectSession {
                    id,
                    section: SessionSection::History,
                },
                SessionCommand::Compactions { id } => Action::InspectSession {
                    id,
                    section: SessionSection::Compactions,
                },
                SessionCommand::Diagnostics { id } => Action::InspectSession {
                    id,
                    section: SessionSection::Diagnostics,
                },
            };
            admin(cli.config, cli.database, cli.auth_dir, action).await
        }
    }
}

async fn admin(
    config: PathBuf,
    database: Option<PathBuf>,
    auth: Option<PathBuf>,
    action: orbit_protocol::admin::Action,
) -> Result<()> {
    let connection = workspace::prepare_connection(
        &workspace::ConnectionOptions::local(
            std::env::current_dir()?.to_string_lossy().into_owned(),
        ),
        config,
        database,
        auth,
        None,
    )
    .await?;
    let mut client = connection.connect().await?;
    let workspace::protocol::Response::Report(report) = client
        .request(workspace::protocol::Request::Admin { action })
        .await?
    else {
        bail!("unexpected workspace management response");
    };
    println!("{}", report.text);
    Ok(())
}

async fn chat(
    config: PathBuf,
    database: Option<PathBuf>,
    args: ChatArgs,
    auth: PathBuf,
) -> Result<()> {
    use workspace::{
        prompt::{PromptOptions, PromptUpdate},
        protocol::{Mode, Request, Response},
    };
    let prompt = read_chat_prompt(&args)?;
    let policy = orbit_protocol::SessionPolicy {
        read_only: matches!(args.mode, ChatMode::ReadOnly),
        disabled: args.disabled.iter().copied().map(Into::into).collect(),
    };
    anyhow::ensure!(
        args.images.is_empty() || !policy.disabled(orbit_protocol::Capability::Image),
        "--image cannot be used when the image capability is disabled"
    );
    let options = workspace::ConnectionOptions {
        ssh: None,
        ssh_user: None,
        ssh_port: None,
        identity: None,
        wsl: None,
        wsl_user: None,
        workspace: std::env::current_dir()?.to_string_lossy().into_owned(),
        server: None,
        upload_server: None,
        allow_mcp_start: args.allow_mcp_start,
    };
    let connection =
        workspace::prepare_connection(&options, config, database, Some(auth), None).await?;
    let mut local = connection
        .local_configuration
        .clone()
        .ok_or_else(|| anyhow::anyhow!("本地配置不可用"))?;
    local.model = args.model.clone();
    let selected = if args.resume.is_none() {
        Some(local.selected_model().await?.ok_or_else(|| {
            anyhow::anyhow!("首次运行请使用 --model 连接名::模型名，或在 TUI 中选择模型")
        })?)
    } else {
        None
    };
    let mut client = connection.connect().await?;
    let Response::Opened(mut info) = client
        .request(Request::Open {
            model: selected,
            resume: args.resume.clone(),
            configuration: connection.configuration().await?,
            policy: policy.clone(),
        })
        .await?
    else {
        bail!("unexpected workspace session response");
    };
    if let Some(model) = &args.model {
        client
            .request(Request::SetModel {
                session: info.id.clone(),
                model: model.clone(),
                effort: None,
            })
            .await?;
        let Response::Opened(updated) = client
            .request(Request::Open {
                model: None,
                resume: Some(info.id.clone()),
                configuration: None,
                policy,
            })
            .await?
        else {
            bail!("unexpected model response");
        };
        info = updated;
    }
    if args.resume.is_none() || args.model.is_some() {
        local.remember_model(&info.model)?;
    }
    let images = workspace::attachments::paths(args.images.clone())?;
    let images = workspace::attachments::uploads(&images)?;
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
    let outcome = workspace::prompt::execute(
        &mut client,
        &info.id,
        PromptOptions {
            prompt,
            images,
            model: None,
            mode: if matches!(args.mode, ChatMode::Yolo) {
                Mode::Yolo
            } else {
                Mode::Auto
            },
        },
        cancellation,
        |update| match update {
            PromptUpdate::Agent(event) => report_chat_event(event, args.stream),
            PromptUpdate::ApprovalDenied => {
                eprintln!("审批已拒绝：非交互模式需要在 TUI 中确认这项操作。");
                Ok(())
            }
        },
    )
    .await?;
    drop(signals);
    if outcome.cancelled {
        eprintln!("已取消。");
    } else if let Some(instance_id) = &args.swe_instance_id {
        let Response::Report(patch) = client
            .request(Request::SessionAction {
                session: info.id.clone(),
                action: orbit_protocol::SessionAction::Inspect {
                    section: orbit_protocol::Inspection::ExportPatch,
                },
            })
            .await?
        else {
            bail!("unexpected patch response");
        };
        println!(
            "{}",
            serde_json::to_string(&SwePrediction {
                instance_id,
                model_name_or_path: &info.model,
                model_patch: patch.text
            })?
        );
    } else if args.stream {
        println!();
    } else {
        println!("{}", outcome.answer);
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

impl From<ChatCapability> for orbit_protocol::Capability {
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
