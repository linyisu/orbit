//! The Tokio transport lives off the GPUI thread. Dropping a window disconnects
//! its client; it never sends Cancel to a running agent.
use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use orbit_client::ConnectionOptions;
use tokio::sync::mpsc;

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, clap::Parser)]
#[command(name = "orbit-gui", about = "Orbit desktop client")]
pub(crate) struct Options {
    #[arg(long, default_value_os_t = orbit_client::configuration::default_path(), env = "orbit_config")]
    pub(crate) config: PathBuf,
    #[arg(long)]
    pub(crate) workspace: Option<PathBuf>,
    #[arg(long)]
    pub(crate) server: Option<String>,
    #[arg(long)]
    pub(crate) database: Option<PathBuf>,
    #[arg(long)]
    pub(crate) auth_dir: Option<PathBuf>,
    #[arg(long)]
    pub(crate) model: Option<String>,
    /// Allow enabled MCP programs to start for this invocation.
    #[arg(long)]
    #[serde(skip)]
    pub(crate) allow_mcp_start: bool,
    #[arg(long)]
    pub(crate) resume: Option<String>,
    /// One-shot intent from selecting a replacement directory for a saved session.
    #[arg(skip)]
    #[serde(skip)]
    pub(crate) relink_on_resume: bool,
    /// A first send may have lost its acknowledgement before persistence was
    /// confirmed. Only this case may recover as a fresh draft if absent.
    #[arg(skip)]
    #[serde(default)]
    pub(crate) recover_draft: bool,
    /// Directory containing the Shell appearance panel; watched in debug builds.
    #[arg(long)]
    pub(crate) appearance: Option<PathBuf>,
    /// GUI preferences file; separate from agent configuration and credentials.
    #[arg(long)]
    pub(crate) preferences: Option<PathBuf>,
    /// Write a readiness receipt after real window rendering and exit; used by native CI.
    #[arg(long, hide = true)]
    #[serde(skip)]
    pub(crate) smoke_test: Option<PathBuf>,
    /// Exercise account discovery and save against the isolated acceptance configuration.
    #[arg(long, hide = true, requires = "smoke_test")]
    #[serde(skip)]
    pub(crate) smoke_account_edit: bool,
    #[arg(long, conflicts_with = "wsl")]
    pub(crate) ssh: Option<String>,
    #[arg(long, requires = "ssh")]
    #[serde(default)]
    pub(crate) ssh_user: Option<String>,
    #[arg(long, requires = "ssh", value_parser = clap::value_parser!(u16).range(1..))]
    #[serde(default)]
    pub(crate) ssh_port: Option<u16>,
    #[arg(long, requires = "ssh")]
    #[serde(default)]
    pub(crate) identity: Option<String>,
    #[arg(long, conflicts_with = "ssh")]
    pub(crate) wsl: Option<String>,
    #[arg(long, requires = "wsl")]
    #[serde(default)]
    pub(crate) wsl_user: Option<String>,
}

impl Options {
    pub(crate) fn resolve_paths(&mut self) -> Result<()> {
        let directory = std::env::current_dir()?;
        for path in std::iter::once(&mut self.config)
            .chain(self.auth_dir.iter_mut())
            .chain(self.preferences.iter_mut())
            .chain(self.appearance.iter_mut())
        {
            if path.is_relative() {
                *path = directory.join(&*path);
            }
        }
        // Workspace and database paths are interpreted by the target service.
        // Prefixing ~/project with the desktop's cwd corrupts a remote target.
        if self.ssh.is_none() && self.wsl.is_none() {
            for path in self.workspace.iter_mut() {
                if path.is_relative() {
                    *path = directory.join(&*path);
                }
            }
        }
        if let Some(identity) = &mut self.identity {
            let path = PathBuf::from(&*identity);
            if path.is_relative() && !identity.starts_with('~') {
                *identity = directory
                    .join(path)
                    .to_string_lossy()
                    .into_owned();
            }
        }
        Ok(())
    }

    /// Session IDs and paths are meaningful only within the same service/data
    /// source. Never activate a local tab for an identically named remote one.
    /// The shared client resolves data-source identity, including workspace-relative databases.
    pub(crate) fn same_host(&self, other: &Self) -> bool {
        let Ok(left) = self
            .connection_options()
            .and_then(|options| options.target())
        else {
            return false;
        };
        let Ok(right) = other
            .connection_options()
            .and_then(|options| options.target())
        else {
            return false;
        };
        fn source(
            options: &Options,
            target: orbit_client::transport::Target,
        ) -> orbit_client::transport::DataSource<'_> {
            orbit_client::transport::DataSource {
                target,
                server: options.server.as_deref(),
                config: options.config.as_path(),
                auth: options.auth_dir.as_deref(),
                database: options.database.as_deref(),
                workspace: options
                    .workspace
                    .as_deref()
                    .unwrap_or(std::path::Path::new("")),
            }
        }
        source(self, left).matches(&source(other, right))
    }

    pub(crate) fn connection_options(&self) -> Result<ConnectionOptions> {
        let workspace = match &self.workspace {
            Some(path) => path.clone(),
            None if self.ssh.is_some() || self.wsl.is_some() => PathBuf::from("~"),
            None => orbit_config::directories::home()
                .ok_or_else(|| anyhow::anyhow!("无法读取用户目录，请选择项目目录"))?,
        };
        let mut connection = ConnectionOptions::local(workspace.to_string_lossy().into_owned());
        connection.server = self.server.clone();
        connection.ssh = self.ssh.clone();
        connection.ssh_user = self.ssh_user.clone();
        connection.ssh_port = self.ssh_port;
        connection.identity = self.identity.clone();
        connection.wsl = self.wsl.clone();
        connection.wsl_user = self.wsl_user.clone();
        connection.allow_mcp_start = self.allow_mcp_start;
        connection.target()?;
        Ok(connection)
    }
}

pub(crate) use orbit_client::pump::{Command, Event};

pub(crate) struct Backend {
    sender: mpsc::Sender<Command>,
    task: tokio::task::JoinHandle<()>,
}

impl Backend {
    pub(crate) fn start(
        options: Options,
        runtime: &tokio::runtime::Runtime,
    ) -> (Self, mpsc::Receiver<Event>) {
        let (sender, commands) = mpsc::channel(32);
        let (events, receiver) = mpsc::channel(64);
        let task = runtime.spawn(async move {
            if let Err(error) = Box::pin(run(options, commands, &events)).await {
                let _ = events
                    .send(Event::Disconnected(format!("{error:#}")))
                    .await;
            }
        });
        (Self { sender, task }, receiver)
    }

    pub(crate) fn send(&self, command: Command) -> Result<()> {
        self.sender
            .try_send(command)
            .map_err(|_| anyhow::anyhow!("连接繁忙或已断开，请稍后重试。"))
    }

    /// Release the local observer and its transports without cancelling persistent server work.
    pub(crate) async fn shutdown(mut self) {
        self.task.abort();
        let _ = (&mut self.task).await;
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run(
    options: Options,
    commands: mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
) -> Result<()> {
    let (progress, mut updates) = orbit_client::interaction::channel();
    let logs = updates.logs.clone();
    let initial_stage = updates
        .stage
        .borrow_and_update()
        .clone();
    events
        .send(Event::ConnectionStage(initial_stage))
        .await?;
    let prompts = events.clone();
    let mut forwarding = AbortTask(tokio::spawn(async move {
        let mut last_logs = Vec::new();
        let mut interval = tokio::time::interval(Duration::from_millis(200));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let logs = log_snapshot(&updates.logs);
                    if logs != last_logs {
                        last_logs.clone_from(&logs);
                        if prompts
                            .send(Event::ConnectionLogs(logs))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
                changed = updates.stage.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let stage = updates
                        .stage
                        .borrow_and_update()
                        .clone();
                    if prompts
                        .send(Event::ConnectionStage(stage))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                challenge = updates.prompts.recv() => {
                    let Some(challenge) = challenge else {
                        break;
                    };
                    if prompts
                        .send(Event::Challenge(challenge))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
    }));
    // The transport loop's future is large in debug builds. Do not embed its
    // state machine in this wrapper and again in the runtime's spawned future.
    let result = Box::pin(run_connection(options, commands, events, progress)).await;
    forwarding.0.abort();
    let _ = (&mut forwarding.0).await;
    // The diagnostics reader may append immediately before a connect error.
    // Flush after the forwarding task stops, before Disconnected is delivered.
    let _ = events
        .send(Event::ConnectionLogs(log_snapshot(&logs)))
        .await;
    result
}

fn log_snapshot(logs: &std::sync::Mutex<std::collections::VecDeque<String>>) -> Vec<String> {
    logs.lock()
        .map(|logs| logs.iter().cloned().collect())
        .unwrap_or_default()
}

async fn run_connection(
    options: Options,
    commands: mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
    progress: orbit_client::interaction::Progress,
) -> Result<()> {
    let attachment_cache =
        orbit_client::attachments::cache_database(options.preferences.as_deref())?;
    let connection = orbit_client::prepare_connection(
        &options.connection_options()?,
        options.config,
        options.database,
        options.auth_dir,
        Some(progress.clone()),
    )
    .await?;
    orbit_client::pump::observe(
        orbit_client::pump::SessionOptions {
            connection,
            resume: options.resume,
            model: options.model,
            relink_on_resume: options.relink_on_resume,
            recover_draft: options.recover_draft,
            attachment_cache,
        },
        commands,
        events,
        progress,
    )
    .await
}

struct AbortTask(tokio::task::JoinHandle<()>);
impl Drop for AbortTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}
