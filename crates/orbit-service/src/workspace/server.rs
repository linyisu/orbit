use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{Mutex, Semaphore},
};

use super::{
    ServiceOptions,
    protocol::{self, Request, Response},
    session::{self, Handle},
};

const IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const START_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_SESSIONS: usize = 32;
const MAX_CLIENTS: usize = 32;
const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;

pub async fn serve(
    args: ServiceOptions,
    config: PathBuf,
    database: Option<PathBuf>,
    auth: PathBuf,
) -> Result<()> {
    let directory = expand_home(&args.workspace)?
        .canonicalize()
        .context("target workspace does not exist")?;
    let config = if config.is_absolute() {
        config
    } else {
        directory.join(config)
    };
    let database = crate::session::database_path(database.map(|path| {
        if path.is_absolute() {
            path
        } else {
            directory.join(path)
        }
    }))?;
    let auth = std::path::absolute(auth)?;
    let root = runtime_directory()?;
    let identity = serde_json::to_vec(&(
        &config,
        &database,
        &auth,
        args.allow_mcp_start,
        std::env::current_exe()?,
        protocol::VERSION,
    ))?;
    let key = Sha256::digest(identity)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let socket = root.join(format!("{}.sock", &key[..24]));
    if !args.daemon {
        let mut child = connect_or_start(&socket, &args, &config, &database, &auth).await?;
        let (mut reader, mut writer) = child.split();
        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        stdout.write_all(protocol::PREAMBLE).await?;
        stdout.flush().await?;
        tokio::select! {
            result = tokio::io::copy(&mut stdin, &mut writer) => { result?; }
            result = tokio::io::copy(&mut reader, &mut stdout) => { result?; }
        }
        return Ok(());
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(socket.with_extension("lock"))?;
    if lock.try_lock().is_err() {
        return Ok(());
    }
    match fs::remove_file(&socket) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let options = crate::WorkspaceOptions {
        initial_directory: directory.clone(),
        config,
        database,
        auth_directory: auth,
        allow_mcp_start: args.allow_mcp_start,
        config_home: std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|home| home.join(".config"))),
        home,
        instruction_dir: crate::instructions::default_user_instruction_dir(),
        editor_command: "vi".into(),
    };
    let service = Arc::new(Service {
        options,
        sessions: Mutex::new(HashMap::new()),
        opening: Mutex::new(()),
        last_activity: Mutex::new(tokio::time::Instant::now()),
        stop: tokio_util::sync::CancellationToken::new(),
        closing: AtomicBool::new(false),
    });
    let permits = Arc::new(Semaphore::new(MAX_CLIENTS));
    let mut idle = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            connection = listener.accept() => {
                let (socket, _) = connection?;
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                let service = service.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let (reader, writer) = socket.into_split();
                    if let Err(error) = service.client(reader, writer).await
                        && !error.downcast_ref::<std::io::Error>().is_some_and(|error| error.kind() == std::io::ErrorKind::UnexpectedEof) {
                        eprintln!("workspace connection closed: {error}");
                    }
                });
            }
            _ = service.stop.cancelled() => break,
            _ = idle.tick() => {
                if service.last_activity.lock().await.elapsed() >= IDLE_TIMEOUT && !service.busy().await { break; }
            }
        }
    }
    fs::remove_file(socket)?;
    drop(lock);
    Ok(())
}

pub fn runtime_directory() -> Result<PathBuf> {
    let uid = unsafe { libc::geteuid() };
    let directory = std::env::temp_dir().join(format!("orbit-workspace-{uid}"));
    match fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(&directory)?;
    ensure!(
        metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o077 == 0,
        "workspace runtime directory must be private and owned by the current user"
    );
    Ok(directory)
}

async fn connect_or_start(
    socket: &Path,
    args: &ServiceOptions,
    config: &Path,
    database: &Path,
    auth: &Path,
) -> Result<UnixStream> {
    if let Ok(stream) = UnixStream::connect(socket).await {
        return Ok(stream);
    }
    let log = OpenOptions::new()
        .create(true)
        .truncate(false)
        .append(true)
        .mode(0o600)
        .open(socket.with_extension("log"))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["workspace-service", "--daemon", "--workspace"])
        .arg(&args.workspace)
        .arg("--config")
        .arg(config)
        .arg("--database")
        .arg(database)
        .arg("--auth-dir")
        .arg(auth)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    if args.allow_mcp_start {
        command.arg("--allow-mcp-start");
    }
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    // Reap a failed launcher without tying the daemon's lifetime to the proxy.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    tokio::time::timeout(START_TIMEOUT, async {
        loop {
            if let Ok(stream) = UnixStream::connect(socket).await {
                return stream;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .with_context(|| {
        format!(
            "workspace server did not start; see {}",
            socket.with_extension("log").display()
        )
    })
}

fn expand_home(path: &str) -> Result<PathBuf> {
    if path == "~" || path.starts_with("~/") {
        let home = std::env::var_os("HOME").context("HOME is unavailable on target")?;
        return Ok(PathBuf::from(home).join(path.strip_prefix("~/").unwrap_or("")));
    }
    Ok(path.into())
}

struct Service {
    options: crate::WorkspaceOptions,
    sessions: Mutex<HashMap<String, Arc<Handle>>>,
    opening: Mutex<()>,
    last_activity: Mutex<tokio::time::Instant>,
    stop: tokio_util::sync::CancellationToken,
    closing: AtomicBool,
}

impl Service {
    async fn busy(&self) -> bool {
        for session in self.sessions.lock().await.values() {
            if session.view.lock().await.busy || session.questions.running().await {
                return true;
            }
        }
        false
    }

    async fn session(&self, id: &str) -> Result<Arc<Handle>> {
        self.sessions
            .lock()
            .await
            .get(id)
            .cloned()
            .context("session is not open on this workspace service")
    }

    async fn client(
        &self,
        mut reader: impl AsyncRead + Unpin,
        mut writer: impl AsyncWrite + Unpin,
    ) -> Result<()> {
        let request = tokio::time::timeout(
            Duration::from_secs(10),
            protocol::read::<Request>(&mut reader),
        )
        .await??;
        if !matches!(
            request,
            Request::Hello {
                version: protocol::VERSION,
                ..
            }
        ) {
            protocol::write(
                &mut writer,
                &Response::Error(format!(
                    "workspace protocol mismatch; expected {}",
                    protocol::VERSION
                )),
            )
            .await?;
            return Ok(());
        }
        let mut options = self.options.clone();
        if let Request::Hello {
            workspace: Some(path),
            ..
        } = request
        {
            options.initial_directory = expand_home(&path)?
                .canonicalize()
                .context("target workspace does not exist")?;
        }
        protocol::write(
            &mut writer,
            &Response::Hello {
                version: protocol::VERSION,
                capabilities: capabilities().await,
                workspace: options.initial_directory.to_string_lossy().into_owned(),
            },
        )
        .await?;
        loop {
            let request = protocol::read::<Request>(&mut reader).await?;
            *self.last_activity.lock().await = tokio::time::Instant::now();
            let shutdown = matches!(request, Request::Shutdown);
            let response = match self.dispatch(request, &options).await {
                Ok(response) => response,
                Err(error) => Response::Error(format!("{error:#}")),
            };
            let stop = shutdown && matches!(response, Response::Ok);
            protocol::write(&mut writer, &response).await?;
            if stop {
                self.stop.cancel();
                return Ok(());
            }
        }
    }

    async fn dispatch(
        &self,
        request: Request,
        connection_options: &crate::WorkspaceOptions,
    ) -> Result<Response> {
        match request {
            Request::Complete { session, input } => {
                ensure!(input.len() <= 4096, "completion input is too long");
                let handle = self.session(&session).await?;
                let info = handle.current_info().await;
                let entries = super::completion::complete(
                    &handle.store,
                    &info.id,
                    &info.workspace,
                    connection_options.home.as_deref(),
                    &input,
                )
                .await?;
                Ok(Response::Completions { input, entries })
            }
            Request::Hello { .. } => bail!("workspace handshake already completed"),
            Request::Open {
                policy,
                resume,
                configuration,
            } => {
                let _opening = self.opening.lock().await;
                ensure!(
                    !self.closing.load(Ordering::Acquire),
                    "workspace service is shutting down"
                );
                if let Some(id) = &resume
                    && let Some(handle) = self.sessions.lock().await.get(id)
                {
                    ensure!(
                        handle.policy == policy,
                        "open session has a different capability policy"
                    );
                    return Ok(Response::Opened(handle.current_info().await));
                }
                ensure!(
                    self.sessions.lock().await.len() < MAX_SESSIONS,
                    "workspace session limit reached; reconnect after idle service shutdown"
                );
                let configuration = match configuration {
                    Some(configuration) => Some(
                        super::configuration::Installed::install(configuration, connection_options)
                            .await?,
                    ),
                    None => None,
                };
                let options = configuration
                    .as_ref()
                    .map_or(connection_options, |configuration| &configuration.options);
                let setup =
                    super::setup::prepare_service_with_policy(options, resume, policy).await?;
                let handle = Arc::new(Handle::spawn(setup, configuration));
                let info = handle.info.clone();
                self.sessions.lock().await.insert(info.id.clone(), handle);
                Ok(Response::Opened(info))
            }
            Request::Plugins { session, action } => {
                let handle = self.session(&session).await?;
                let catalog = super::plugins::execute(&handle, action).await?;
                handle
                    .action(orbit_protocol::SessionAction::RefreshSkills)
                    .await?;
                Ok(Response::Plugins(catalog))
            }
            Request::Admin { action } => {
                let _opening = self.opening.lock().await;
                ensure!(
                    !self.closing.load(Ordering::Acquire),
                    "workspace service is shutting down"
                );
                if let orbit_protocol::admin::Action::DeleteSession { id } = &action {
                    let handle = self.sessions.lock().await.get(id).cloned();
                    if let Some(handle) = handle {
                        handle.command(session::Command::Close).await?;
                        self.sessions.lock().await.remove(id);
                    }
                }
                let store =
                    orbit_store::store::Store::open(connection_options.database.clone()).await?;
                Ok(Response::Report(
                    super::admin::execute(&store, &connection_options.initial_directory, action)
                        .await?,
                ))
            }
            Request::ListSessions { session } => {
                let handle = self.session(&session).await?;
                let session = handle.store.session(&handle.info.id).await?;
                let sessions = handle
                    .store
                    .list()
                    .await?
                    .into_iter()
                    .filter(|item| item.project_id == session.project_id)
                    .collect();
                Ok(Response::Sessions(sessions))
            }
            Request::Sessions => {
                let store =
                    orbit_store::store::Store::open(connection_options.database.clone()).await?;
                let project = store
                    .resolve_project(&connection_options.initial_directory)
                    .await?;
                let sessions = store
                    .list()
                    .await?
                    .into_iter()
                    .filter(|session| session.project_id == project.id)
                    .collect();
                Ok(Response::Sessions(sessions))
            }
            Request::Start {
                session,
                request_id,
                prompt,
                images,
                compact,
                mode,
                model,
            } => {
                let _opening = self.opening.lock().await;
                ensure!(
                    !self.closing.load(Ordering::Acquire),
                    "workspace service is shutting down"
                );
                self.session(&session)
                    .await?
                    .command(session::Command::Start {
                        request_id,
                        prompt,
                        images,
                        compact,
                        mode,
                        model,
                    })
                    .await?;
                Ok(Response::Ok)
            }
            Request::Ask {
                session,
                question,
                follow_up,
            } => {
                let _opening = self.opening.lock().await;
                ensure!(
                    !self.closing.load(Ordering::Acquire),
                    "workspace service is shutting down"
                );
                let session = self.session(&session).await?;
                let selected = session.questions.start(&question, follow_up).await?;
                Ok(Response::Questions(
                    session.questions.snapshot(None, Some(selected)).await?,
                ))
            }
            Request::ReadQuestions { session, after } => Ok(Response::Questions(
                self.session(&session)
                    .await?
                    .questions
                    .snapshot(after, None)
                    .await?,
            )),
            Request::CancelQuestion { session, index } => {
                self.session(&session).await?.questions.cancel(index).await;
                Ok(Response::Ok)
            }
            Request::SessionAction { session, action } => Ok(Response::Report(
                self.session(&session).await?.action(action).await?,
            )),
            Request::ConfigureModels {
                session,
                configuration,
            } => {
                self.session(&session)
                    .await?
                    .command(session::Command::ConfigureModels { configuration })
                    .await?;
                Ok(Response::Ok)
            }
            Request::SetMode { session, mode } => {
                self.session(&session)
                    .await?
                    .command(session::Command::SetMode { mode })
                    .await?;
                Ok(Response::Ok)
            }
            Request::SetModel {
                session,
                model,
                effort,
            } => {
                self.session(&session)
                    .await?
                    .command(session::Command::SetModel { model, effort })
                    .await?;
                Ok(Response::Ok)
            }
            Request::Steer { session, message } => {
                self.session(&session)
                    .await?
                    .command(session::Command::Steer { message })
                    .await?;
                Ok(Response::Ok)
            }
            Request::Poll { session, after } => Ok(Response::Page(
                self.session(&session)
                    .await?
                    .view
                    .lock()
                    .await
                    .page(after)?,
            )),
            Request::Undo { session, path } => {
                self.session(&session)
                    .await?
                    .command(session::Command::Undo { path })
                    .await?;
                Ok(Response::Ok)
            }
            Request::Cancel { session } => {
                self.session(&session)
                    .await?
                    .command(session::Command::Cancel)
                    .await?;
                Ok(Response::Ok)
            }
            Request::Approve {
                session,
                approval,
                allow,
                remember,
            } => {
                self.session(&session)
                    .await?
                    .command(session::Command::Approve {
                        id: approval,
                        allow,
                        remember,
                    })
                    .await?;
                Ok(Response::Ok)
            }
            Request::ReadFile { session, path } => {
                let session = self.session(&session).await?;
                let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
                    use std::io::Read;
                    let file = session
                        .workspace
                        .read()
                        .map_err(|_| anyhow::anyhow!("workspace state unavailable"))?
                        .authorize_read(Path::new(&path))?;
                    let mut bytes = Vec::new();
                    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
                    ensure!(
                        bytes.len() as u64 <= MAX_FILE_BYTES,
                        "workspace file exceeds 20 MiB"
                    );
                    Ok(bytes)
                })
                .await??;
                Ok(Response::File(
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                ))
            }
            Request::ReadSubagent {
                session,
                agent,
                after,
            } => {
                let session = self.session(&session).await?;
                Ok(Response::Subagent(
                    session.subagent_page(&agent, after).await?,
                ))
            }
            Request::SendSubagent {
                session,
                agent,
                message,
            } => {
                ensure!(
                    !message.trim().is_empty() && message.len() <= 1024 * 1024,
                    "invalid subagent message"
                );
                let session = self.session(&session).await?;
                session.authorize_subagent(&agent).await?;
                session
                    .command(session::Command::SendSubagent { agent, message })
                    .await?;
                Ok(Response::Ok)
            }
            Request::Shutdown => {
                let _opening = self.opening.lock().await;
                ensure!(
                    !self.busy().await,
                    "workspace has active tasks; cancel them before stopping the service"
                );
                self.closing.store(true, Ordering::Release);
                Ok(Response::Ok)
            }
        }
    }
}

async fn capabilities() -> orbit_protocol::Capabilities {
    static CAPABILITIES: tokio::sync::OnceCell<orbit_protocol::Capabilities> =
        tokio::sync::OnceCell::const_new();
    CAPABILITIES
        .get_or_init(|| async {
            let sandbox = match orbit_runtime::execution::capabilities::check_sandbox().await {
                Ok(()) => orbit_protocol::Availability::Available,
                Err(error) => orbit_protocol::Availability::Unavailable(format!("{error:#}")),
            };
            orbit_protocol::Capabilities {
                system: std::env::consts::OS.into(),
                architecture: std::env::consts::ARCH.into(),
                shell: std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
                native_processes: true,
                files: true,
                sandbox,
                features: [
                    "sessions",
                    "events",
                    "approvals",
                    "cancel",
                    "files",
                    "subagents",
                    "images",
                    "compact",
                    "steer",
                    "side_questions",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            }
        })
        .await
        .clone()
}
