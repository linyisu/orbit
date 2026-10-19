#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
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
const MAX_FILE_BYTES: u64 = 20 * 1_024 * 1_024;

mod endpoints;
mod lifecycle;
#[cfg(unix)]
#[path = "server/transport/unix.rs"]
mod transport;
#[cfg(windows)]
#[path = "server/transport/windows.rs"]
mod transport;
use transport::{Listener, Stream};

#[derive(serde::Serialize, serde::Deserialize)]
struct ConnectionContext {
    config: PathBuf,
    auth: PathBuf,
    allow_mcp_start: bool,
    workspace: PathBuf,
}

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
    let database = crate::session::database_path(
        database
            .map(|path| resolve_database(path, &directory))
            .transpose()?,
    )?;
    let database_directory = database
        .parent()
        .context("database has no parent")?;
    #[cfg(unix)]
    fs::create_dir_all(database_directory)?;
    #[cfg(windows)]
    orbit_runtime::filesystem::private::create_directory_all(database_directory)
        .context("database directory must be private to protect SQLite journal files")?;
    #[cfg(windows)]
    orbit_runtime::filesystem::private::open_file(&database, true, false)?;
    #[cfg(unix)]
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&database)?;
    let database = database.canonicalize()?;
    let auth = std::path::absolute(auth)?;
    let root = runtime_directory()?;
    let socket = endpoints::shared(&root, &database)?;
    if !args.daemon {
        #[cfg(windows)]
        lifecycle::isolate_stdio()?;
        let mut child =
            lifecycle::connect_or_start(&socket, &args, &config, &database, &auth).await?;
        protocol::write(
            &mut child,
            &ConnectionContext {
                config,
                auth,
                allow_mcp_start: args.allow_mcp_start,
                workspace: directory,
            },
        )
        .await?;
        let (mut reader, mut writer) = tokio::io::split(&mut child);
        let mut stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        stdout
            .write_all(protocol::PREAMBLE)
            .await?;
        stdout.flush().await?;
        tokio::select! {
            result = tokio::io::copy(&mut stdin, &mut writer) => {
                result?;
            }
            result = tokio::io::copy(&mut reader, &mut stdout) => {
                result?;
            }
        }
        return Ok(());
    }
    orbit_runtime::execution::shell_environment::initialize().await;

    let lock = lifecycle::private_file(&socket.with_extension("lock"))?;
    if lock.try_lock().is_err() {
        return Ok(());
    }
    // Retain the old daemon locks for the entire lifetime of the shared server.
    // Older launchers can connect through aliases but cannot start a rival.
    let _legacy_locks = endpoints::lock_legacy(&root, &database)?;
    #[cfg(unix)]
    match fs::remove_file(&socket) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut listener = Listener::bind(&socket)?;
    #[cfg(unix)]
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    endpoints::publish_aliases(&root, &database, &socket)?;
    let home = orbit_config::directories::home();
    let options = crate::WorkspaceOptions {
        model: None,
        initial_directory: directory.clone(),
        config,
        database,
        auth_directory: auth,
        allow_mcp_start: args.allow_mcp_start,
        config_home: orbit_config::directories::config(),
        home,
        instruction_dir: crate::instructions::default_user_instruction_dir(),
        editor_command: "vi".into(),
    };
    let service = Arc::new(Service {
        library: Mutex::new(super::library::Library::new(&options)?),
        options,
        sessions: Mutex::new(HashMap::new()),
        browsing: Mutex::new(HashMap::new()),
        opening: Mutex::new(()),
        last_activity: Mutex::new(tokio::time::Instant::now()),
        stop: tokio_util::sync::CancellationToken::new(),
        closing: AtomicBool::new(false),
    });
    let permits = Arc::new(Semaphore::new(MAX_CLIENTS));
    let mut idle = tokio::time::interval(Duration::from_secs(30));
    loop {
        tokio::select! {
            connection = transport::accept(&mut listener) => {
                let socket = connection?;
                let Ok(permit) = permits.clone().try_acquire_owned() else {
                    continue;
                };
                let service = service.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let (reader, writer) = tokio::io::split(socket);
                    if let Err(error) = service.client(reader, writer).await
                        && !error
                            .downcast_ref::<std::io::Error>()
                            .is_some_and(|error| error.kind() == std::io::ErrorKind::UnexpectedEof)
                    {
                        eprintln!("workspace connection closed: {error}");
                    }
                });
            }
            _ = service.stop.cancelled() => break,
            _ = idle.tick() => {
                let _opening = service.opening.lock().await;
                service.collect_idle_sessions().await;
                if service
                    .last_activity
                    .lock()
                    .await
                    .elapsed()
                    >= IDLE_TIMEOUT
                    && !service.busy().await
                {
                    service
                        .closing
                        .store(true, Ordering::Release);
                    break;
                }
            }
        }
    }
    // Release session writers before publishing the endpoint as replaceable.
    // A replacement daemon may restore the same session immediately.
    service
        .closing
        .store(true, Ordering::Release);
    let _opening = service.opening.lock().await;
    for handle in service.sessions.lock().await.values() {
        handle
            .command(session::Command::Close)
            .await?;
    }
    #[cfg(unix)]
    fs::remove_file(socket)?;
    Ok(())
}

#[cfg(unix)]
pub fn runtime_directory() -> Result<PathBuf> {
    let uid = unsafe { libc::geteuid() };
    let directory = std::env::temp_dir().join(format!("orbit-workspace-{uid}"));
    match fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
    {
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

#[cfg(windows)]
pub fn runtime_directory() -> Result<PathBuf> {
    let sid = orbit_runtime::filesystem::private::user_id()?;
    let directory = std::env::temp_dir().join(format!("orbit-workspace-{sid}"));
    orbit_runtime::filesystem::private::create_directory(&directory)?;
    Ok(directory)
}

fn expand_home(path: &str) -> Result<PathBuf> {
    if path == "~" || path.starts_with("~/") {
        let home =
            orbit_config::directories::home().context("user home is unavailable on target")?;
        return Ok(home.join(path.strip_prefix("~/").unwrap_or("")));
    }
    Ok(path.into())
}

fn resolve_database(path: PathBuf, workspace: &Path) -> Result<PathBuf> {
    let path = match path.to_str() {
        Some(path) if path == "~" || path.starts_with("~/") => expand_home(path)?,
        _ => path,
    };
    Ok(if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    })
}

struct Service {
    library: Mutex<super::library::Library>,
    options: crate::WorkspaceOptions,
    sessions: Mutex<HashMap<String, Arc<Handle>>>,
    browsing: Mutex<HashMap<String, Arc<Mutex<session::browsing::Browsing>>>>,
    opening: Mutex<()>,
    last_activity: Mutex<tokio::time::Instant>,
    stop: tokio_util::sync::CancellationToken,
    closing: AtomicBool,
}

impl Service {
    async fn browsing(
        &self,
        id: &str,
    ) -> Option<tokio::sync::OwnedMutexGuard<session::browsing::Browsing>> {
        let origin = self
            .browsing
            .lock()
            .await
            .get(id)
            .cloned()?;
        let guard = origin.clone().lock_owned().await;
        // Preparation replaces the registry entry while holding this same session lock.
        // A request waiting on the old entry must route to the prepared actor instead.
        let current = self
            .browsing
            .lock()
            .await
            .get(id)
            .is_some_and(|entry| Arc::ptr_eq(entry, &origin));
        current.then_some(guard)
    }

    async fn busy(&self) -> bool {
        if self
            .browsing
            .lock()
            .await
            .values()
            .any(|session| session.try_lock().is_err())
        {
            return true;
        }
        let sessions: Vec<_> = self
            .sessions
            .lock()
            .await
            .values()
            .cloned()
            .collect();
        for session in sessions {
            if session.running().await {
                return true;
            }
        }
        false
    }

    async fn conversation(
        &self,
        id: &str,
    ) -> Result<(orbit_store::store::Store, protocol::SessionInfo)> {
        if let Some(browsing) = self.browsing(id).await {
            return Ok((browsing.session.store.clone(), browsing.info()));
        }
        let handle = self.session(id).await?;
        Ok((handle.store.clone(), handle.current_info().await))
    }

    async fn archive_store(
        &self,
        parent: &str,
        agent: Option<&str>,
        options: &crate::WorkspaceOptions,
    ) -> Result<orbit_store::store::Store> {
        let (store, _) = self.conversation(parent).await?;
        if let Some(agent) = agent {
            if let Ok(handle) = self.session(parent).await {
                handle.authorize_subagent(agent).await?;
            } else {
                let browsing = self
                    .browsing(parent)
                    .await
                    .context("conversation is not open")?;
                let config = browsing.config(options)?;
                ensure!(
                    !config
                        .disabled_capabilities
                        .iter()
                        .any(|capability| matches!(
                            capability,
                            orbit_protocol::Capability::All | orbit_protocol::Capability::Subagents
                        )),
                    "子 Agent 功能已禁用"
                );
                session::authorize_subagent_record(&store, parent, agent).await?;
            }
        }
        Ok(store)
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
        let context = tokio::time::timeout(
            Duration::from_secs(10),
            protocol::read::<ConnectionContext>(&mut reader),
        )
        .await??;
        let request = tokio::time::timeout(
            Duration::from_secs(10),
            protocol::read::<Request>(&mut reader),
        )
        .await??;
        let version = match &request {
            Request::Hello { version, .. } => *version,
            _ => 0,
        };
        if !orbit_protocol::supports_version(version) {
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
        options.config = context.config;
        options.auth_directory = context.auth;
        options.allow_mcp_start = context.allow_mcp_start;
        options.initial_directory = context.workspace;
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
                version,
                build_generation: lifecycle::build_generation(),
                capabilities: capabilities().await,
                workspace: options
                    .initial_directory
                    .to_string_lossy()
                    .into_owned(),
            },
        )
        .await?;
        static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);
        let viewer = NEXT_CLIENT.fetch_add(1, Ordering::Relaxed);
        let mut opened = Vec::new();
        let result = async {
            loop {
                let request = protocol::read::<Request>(&mut reader).await?;
                *self.last_activity.lock().await = tokio::time::Instant::now();
                let shutdown = matches!(request, Request::Shutdown);
                let response = match self
                    .dispatch(request, &options, viewer)
                    .await
                {
                    Ok(response) => response,
                    Err(error) => Response::Error(format!("{error:#}")),
                };
                if let Response::Opened(info) = &response {
                    opened.push(info.id.clone());
                }
                let stop = shutdown && matches!(response, Response::Ok);
                protocol::write(&mut writer, &response).await?;
                if stop {
                    self.stop.cancel();
                    return Ok(());
                }
            }
        }
        .await;
        self.release_client(opened, viewer)
            .await;
        result
    }

    async fn release_client(&self, opened: Vec<String>, viewer: u64) {
        let _opening = self.opening.lock().await;
        let sessions = self.sessions.lock().await.clone();
        for handle in sessions.values() {
            handle.release_viewer(viewer).await;
        }
        for id in opened {
            if let Some(mut session) = self.browsing(&id).await {
                session.clients = session.clients.saturating_sub(1);
                if session.clients == 0 {
                    self.browsing.lock().await.remove(&id);
                }
                continue;
            }
            let Some(handle) = sessions.get(&id) else {
                continue;
            };
            handle
                .clients
                .fetch_sub(1, Ordering::AcqRel);
        }
        self.collect_idle_sessions().await;
    }

    /// The opening lock serializes attachment with retirement. Never wait for an
    /// actor while holding the session-map lock needed by unrelated clients.
    async fn collect_idle_sessions(&self) {
        let candidates: Vec<_> = self
            .sessions
            .lock()
            .await
            .iter()
            .filter(|(_, handle)| handle.clients.load(Ordering::Acquire) == 0)
            .map(|(id, handle)| (id.clone(), handle.clone()))
            .collect();
        for (id, handle) in candidates {
            let (result, discarded) = tokio::sync::oneshot::channel();
            if handle
                .command(session::Command::RetireUnobserved { result })
                .await
                .is_ok()
                && discarded.await.unwrap_or(false)
            {
                self.sessions.lock().await.remove(&id);
            }
        }
    }

    async fn dispatch(
        &self,
        request: Request,
        connection_options: &crate::WorkspaceOptions,
        viewer: u64,
    ) -> Result<Response> {
        match request {
            Request::Browse {
                resume,
                configuration,
            } => {
                let _opening = self.opening.lock().await;
                ensure!(
                    !self.closing.load(Ordering::Acquire),
                    "workspace service is shutting down"
                );
                if let Some(id) = &resume
                    && let Some(handle) = self
                        .sessions
                        .lock()
                        .await
                        .get(id)
                        .cloned()
                {
                    handle
                        .clients
                        .fetch_add(1, Ordering::AcqRel);
                    return Ok(Response::Opened(handle.current_info().await));
                }
                if let Some(id) = &resume
                    && let Some(mut browsing) = self.browsing(id).await
                {
                    browsing.clients += 1;
                    return Ok(Response::Opened(browsing.info()));
                }
                ensure!(
                    self.sessions.lock().await.len() + self.browsing.lock().await.len()
                        < MAX_SESSIONS,
                    "workspace session limit reached; close unused conversations"
                );
                let session = crate::session::SessionState::browse(
                    connection_options.database.clone(),
                    resume,
                    &connection_options.initial_directory,
                )
                .await?;
                let mode = match session
                    .store
                    .tui_mode(session.id())
                    .await?
                    .as_str()
                {
                    "ask" => orbit_protocol::Mode::Ask,
                    "YOLO" => orbit_protocol::Mode::Yolo,
                    _ => orbit_protocol::Mode::Auto,
                };
                let configuration = match configuration {
                    Some(Ok(configuration)) => {
                        super::configuration::Installed::install(configuration, connection_options)
                            .await
                            .map(|installed| Some(Arc::new(installed)))
                            .map_err(|_| ())
                    }
                    Some(Err(())) => Err(()),
                    None => Ok(None),
                };
                let browsing = session::browsing::Browsing {
                    session,
                    directory: connection_options
                        .initial_directory
                        .clone(),
                    home: connection_options.home.clone(),
                    selection: None,
                    clients: 1,
                    mode,
                    mode_revision: 0,
                    plan_mode: None,
                    configuration,
                };
                let info = browsing.info();
                self.browsing
                    .lock()
                    .await
                    .insert(info.id.clone(), Arc::new(Mutex::new(browsing)));
                Ok(Response::Opened(info))
            }
            Request::Prepare {
                session,
                model,
                configuration,
            } => {
                let _opening = self.opening.lock().await;
                ensure!(
                    !self.closing.load(Ordering::Acquire),
                    "workspace service is shutting down"
                );
                if let Some(handle) = self
                    .sessions
                    .lock()
                    .await
                    .get(&session)
                    .cloned()
                {
                    return Ok(Response::Profile(Box::new(handle.current_info().await)));
                }
                let browsing = self
                    .browsing(&session)
                    .await
                    .context("conversation is not open")?;
                let configuration = match configuration {
                    Some(configuration) => Some(
                        super::configuration::Installed::install(configuration, connection_options)
                            .await
                            .map(Arc::new)?,
                    ),
                    None => browsing
                        .configuration
                        .as_ref()
                        .map_err(|()| anyhow::anyhow!("远程配置尚未就绪，请修正配置后重试"))?
                        .clone(),
                };
                let mut options = configuration
                    .as_ref()
                    .map_or(connection_options, |installed| &installed.options)
                    .clone();
                options.model = browsing
                    .selection
                    .as_ref()
                    .map(|(model, _)| model.clone())
                    .or(model);
                options
                    .initial_directory
                    .clone_from(&browsing.directory);
                let mut state = browsing.session.execution_copy();
                if state.is_persisted() {
                    state
                        .validate_directory(&options.initial_directory)
                        .await?;
                    state
                        .store
                        .refresh_runtime(&mut state.snapshot)
                        .await?;
                }
                let mut resources = super::setup::prepare_resources(
                    &options,
                    &options.initial_directory,
                    &state,
                    Default::default(),
                    &[],
                )
                .await?;
                if let Some((model, effort)) = &browsing.selection {
                    super::profile::apply(
                        &mut resources,
                        model,
                        effort
                            .as_deref()
                            .map(str::parse)
                            .transpose()
                            .map_err(|()| anyhow::anyhow!("unknown reasoning effort"))?,
                    )?;
                } else {
                    let (saved, effort) = state.saved_model_profile();
                    if let Some(choice) = resources
                        .budget
                        .model_options
                        .iter()
                        .find(|choice| choice.qualified_name() == saved)
                    {
                        let effort = effort.filter(|effort| {
                            choice
                                .reasoning_efforts
                                .contains(effort)
                        });
                        super::profile::apply(&mut resources, saved, effort)?;
                    }
                }
                resources.mode = browsing.mode;
                resources.mode_revision = browsing.mode_revision;
                resources
                    .workspace
                    .policy()
                    .approval_policy()
                    .set_mode(super::setup::approval_mode(browsing.mode));
                if let Some(mode) = browsing.plan_mode {
                    resources
                        .planning
                        .action(match mode {
                            orbit_protocol::plan::WorkMode::Plan => {
                                orbit_protocol::plan::Action::Enter
                            }
                            orbit_protocol::plan::WorkMode::Build => {
                                orbit_protocol::plan::Action::Leave
                            }
                        })
                        .await?;
                }
                state
                    .initialize_audit(
                        resources.workspace.clone(),
                        resources.command_limits.clone(),
                    )
                    .await?;
                state
                    .set_model_profile(
                        resources.model_name.clone(),
                        resources
                            .model
                            .config()
                            .reasoning_effort,
                    )
                    .await?;
                let setup = crate::PreparedWorkspace {
                    session: state,
                    resources,
                };
                let handle = Arc::new(Handle::spawn(setup, configuration).await?);
                let info = handle.info.clone();
                handle
                    .clients
                    .store(browsing.clients, Ordering::Release);
                self.sessions
                    .lock()
                    .await
                    .insert(session.clone(), handle);
                self.browsing
                    .lock()
                    .await
                    .remove(&session);
                drop(browsing);
                Ok(Response::Profile(Box::new(info)))
            }
            Request::Library { action } => Ok(Response::Library(
                self.library
                    .lock()
                    .await
                    .execute(action)
                    .await?,
            )),
            Request::RestoreSessions => {
                let store =
                    orbit_store::store::Store::open(connection_options.database.clone()).await?;
                let ids = store.previous_tui_tabs().await?;
                let mut sessions: std::collections::HashMap<_, _> = store
                    .list()
                    .await?
                    .into_iter()
                    .map(|session| (session.id.clone(), session))
                    .collect();
                Ok(Response::Sessions(
                    ids.into_iter()
                        .filter_map(|id| sessions.remove(&id))
                        .collect(),
                ))
            }
            Request::Complete { session, input } => {
                ensure!(input.len() <= 4_096, "completion input is too long");
                let (store, info) = self.conversation(&session).await?;
                let entries = super::completion::complete(
                    &store,
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
                model,
                policy,
                resume,
                configuration,
            } => {
                let _opening = self.opening.lock().await;
                ensure!(
                    !self.closing.load(Ordering::Acquire),
                    "workspace service is shutting down"
                );
                let existing = {
                    let sessions = self.sessions.lock().await;
                    resume
                        .as_ref()
                        .and_then(|id| sessions.get(id).cloned())
                };
                if let Some(handle) = existing {
                    ensure!(
                        handle.requested_policy == policy,
                        "open session has a different capability policy"
                    );
                    handle
                        .clients
                        .fetch_add(1, Ordering::AcqRel);
                    handle
                        .reset_progress_delivery(viewer)
                        .await;
                    return Ok(Response::Opened(handle.current_info().await));
                }
                self.collect_idle_sessions().await;
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
                let mut options = options.clone();
                options.model = model;
                let setup =
                    super::setup::prepare_service_with_policy(&options, resume, policy.clone())
                        .await?;
                let mut handle = Handle::spawn(setup, configuration.map(Arc::new)).await?;
                handle.requested_policy = policy;
                let handle = Arc::new(handle);
                let info = handle.info.clone();
                let browsing = self
                    .browsing
                    .lock()
                    .await
                    .remove(&info.id);
                let clients = match browsing {
                    Some(browsing) => browsing.lock().await.clients,
                    None => 0,
                };
                handle
                    .clients
                    .store(clients + 1, Ordering::Release);
                self.sessions
                    .lock()
                    .await
                    .insert(info.id.clone(), handle);
                Ok(Response::Opened(info))
            }
            Request::Plugins { .. } => bail!("插件管理已迁移至全局库，请更新客户端后重试"),
            Request::Resources {
                session,
                expected_workspace,
                action,
            } => {
                if let Some(mut browsing) = self.browsing(&session).await {
                    ensure!(
                        expected_workspace
                            .as_deref()
                            .is_none_or(|expected| expected == browsing.info().workspace),
                        "项目已切换，请刷新后重试"
                    );
                    return Ok(Response::Resources(
                        super::resources::browse(&mut browsing, connection_options, action).await?,
                    ));
                }
                Ok(Response::Resources(
                    self.session(&session)
                        .await?
                        .resources(action, expected_workspace)
                        .await?,
                ))
            }
            Request::Tasks {
                session,
                expected_workspace,
                action,
            } => {
                if let Some(mut browsing) = self.browsing(&session).await {
                    ensure!(
                        expected_workspace
                            .as_deref()
                            .is_none_or(|expected| expected == browsing.info().workspace),
                        "项目已切换，请刷新后重试"
                    );
                    let config = browsing.config(connection_options)?;
                    let policy = orbit_protocol::SessionPolicy {
                        disabled: config.disabled_capabilities,
                        ..Default::default()
                    };
                    return Ok(Response::Tasks(
                        super::tasks::snapshot_session(&mut browsing.session, &policy, action)
                            .await?,
                    ));
                }
                Ok(Response::Tasks(
                    self.session(&session)
                        .await?
                        .tasks(action, expected_workspace)
                        .await?,
                ))
            }
            Request::Files {
                session,
                expected_workspace,
                action,
            } => {
                if let Some(browsing) = self.browsing(&session).await {
                    ensure!(
                        expected_workspace
                            .as_deref()
                            .is_none_or(|expected| expected == browsing.info().workspace),
                        "项目已切换，请刷新后重试"
                    );
                    return Ok(Response::Files(
                        super::files::browse(&browsing, connection_options, action).await?,
                    ));
                }
                Ok(Response::Files(
                    self.session(&session)
                        .await?
                        .files(action, expected_workspace)
                        .await?,
                ))
            }
            Request::Review {
                session,
                expected_workspace,
                action,
            } => {
                if let Some(browsing) = self.browsing(&session).await {
                    ensure!(
                        expected_workspace == browsing.info().workspace,
                        "项目已切换，请刷新后重试"
                    );
                    return Ok(Response::Review(
                        super::review::browse(&browsing, connection_options, action).await?,
                    ));
                }
                Ok(Response::Review(
                    self.session(&session)
                        .await?
                        .review(action, expected_workspace)
                        .await?,
                ))
            }
            Request::Subagents {
                session,
                expected_workspace,
                action,
            } => {
                if let Some(browsing) = self.browsing(&session).await {
                    ensure!(
                        expected_workspace == browsing.info().workspace,
                        "项目已切换，请刷新后重试"
                    );
                    let config = browsing.config(connection_options)?;
                    let policy = orbit_protocol::SessionPolicy {
                        disabled: config.disabled_capabilities,
                        ..Default::default()
                    };
                    ensure!(
                        !policy.disabled(orbit_protocol::Capability::Subagents),
                        "子 Agent 功能已禁用"
                    );
                    let orbit_protocol::subagents::Action::List { offset } = action else {
                        bail!("子 Agent 尚未启动执行");
                    };
                    return Ok(Response::Subagents(
                        super::subagents::list(&browsing.session.store, &session, offset).await?,
                    ));
                }
                Ok(Response::Subagents(
                    self.session(&session)
                        .await?
                        .subagents(action, expected_workspace)
                        .await?,
                ))
            }
            Request::InputQueue {
                session,
                expected_workspace,
                action,
            } => {
                if let Some(mut browsing) = self.browsing(&session).await {
                    return Ok(Response::InputQueue(
                        super::input_queue::execute_session(
                            &mut browsing.session,
                            None,
                            &expected_workspace,
                            action,
                        )
                        .await?,
                    ));
                }
                Ok(Response::InputQueue(
                    self.session(&session)
                        .await?
                        .input_queue(action, expected_workspace)
                        .await?,
                ))
            }
            Request::Admin { action } => {
                let _opening = self.opening.lock().await;
                ensure!(
                    !self.closing.load(Ordering::Acquire),
                    "workspace service is shutting down"
                );
                if let orbit_protocol::admin::Action::RenameSession { id, title } = &action {
                    super::admin::ensure_title(title)?;
                    if let Some(mut browsing) = self.browsing(id).await {
                        browsing
                            .session
                            .rename(title.clone())
                            .await?;
                        return Ok(Response::Report(orbit_protocol::Report {
                            title: "会话标题".into(),
                            text: title.clone(),
                        }));
                    }
                    let handle = self
                        .sessions
                        .lock()
                        .await
                        .get(id)
                        .cloned();
                    if let Some(handle) = handle {
                        return Ok(Response::Report(
                            handle
                                .action(orbit_protocol::SessionAction::Rename {
                                    title: title.clone(),
                                })
                                .await?,
                        ));
                    }
                }
                let mut deleted_browsing = None;
                if let orbit_protocol::admin::Action::DeleteSession { id } = &action {
                    deleted_browsing = self.browsing(id).await;
                    let handle = self
                        .sessions
                        .lock()
                        .await
                        .get(id)
                        .cloned();
                    if let Some(handle) = handle {
                        handle
                            .command(session::Command::Close)
                            .await?;
                        self.sessions.lock().await.remove(id);
                    }
                }
                let store =
                    orbit_store::store::Store::open(connection_options.database.clone()).await?;
                let report =
                    super::admin::execute(&store, &connection_options.initial_directory, action)
                        .await?;
                if let Some(browsing) = deleted_browsing {
                    self.browsing
                        .lock()
                        .await
                        .remove(browsing.session.id());
                }
                Ok(Response::Report(report))
            }
            Request::ListSessions { session } => {
                let (store, info) = self.conversation(&session).await?;
                let Some(project) = store
                    .project_at(&info.workspace)
                    .await?
                else {
                    return Ok(Response::Sessions(Vec::new()));
                };
                let projects = store
                    .equivalent_project_ids(&project.id)
                    .await?;
                let sessions = store
                    .list_resumable()
                    .await?
                    .into_iter()
                    .filter(|item| projects.contains(&item.project_id))
                    .collect();
                Ok(Response::Sessions(sessions))
            }
            Request::AllSessions => {
                let store =
                    orbit_store::store::Store::open(connection_options.database.clone()).await?;
                let mut sessions = store.list_resumable().await?;
                // Opening a directory is read-only until a conversation is resumed or
                // started. Resolve its identity as the TUI does, so a moved project
                // can be resumed before its new path has been persisted.
                if let Some(project) = store
                    .project_at(&connection_options.initial_directory)
                    .await?
                {
                    let ids = store
                        .equivalent_project_ids(&project.id)
                        .await?;
                    if let Some(path) = project.path {
                        for session in &mut sessions {
                            if ids.contains(&session.project_id) {
                                session.workspace.clone_from(&path);
                            }
                        }
                    }
                }
                Ok(Response::Sessions(sessions))
            }
            Request::Sessions => {
                let store =
                    orbit_store::store::Store::open(connection_options.database.clone()).await?;
                let Some(project) = store
                    .project_at(&connection_options.initial_directory)
                    .await?
                else {
                    return Ok(Response::Sessions(Vec::new()));
                };
                let projects = store
                    .equivalent_project_ids(&project.id)
                    .await?;
                let sessions = store
                    .list_resumable()
                    .await?
                    .into_iter()
                    .filter(|session| projects.contains(&session.project_id))
                    .collect();
                Ok(Response::Sessions(sessions))
            }
            Request::SubmitAttachments {
                session,
                request_id,
                prompt,
                images,
                files,
                steer,
            } => {
                let _opening = self.opening.lock().await;
                ensure!(
                    !self.closing.load(Ordering::Acquire),
                    "workspace service is shutting down"
                );
                let handle = self.session(&session).await?;
                if steer {
                    let (accepted, receipt) = tokio::sync::oneshot::channel();
                    handle
                        .command(session::Command::Steer {
                            delivery: orbit_agent::inbox::Delivery::AtBoundary,
                            message: prompt,
                            images,
                            files,
                            accepted,
                        })
                        .await?;
                    Ok(Response::InputQueued {
                        message_id: receipt
                            .await?
                            .context("missing steering receipt")?,
                    })
                } else {
                    handle
                        .command(session::Command::Start {
                            request_id,
                            prompt,
                            images,
                            files,
                            compact: false,
                            model: None,
                        })
                        .await?;
                    Ok(Response::Ok)
                }
            }
            Request::Start {
                session,
                request_id,
                prompt,
                images,
                compact,
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
                        files: Vec::new(),
                        compact,
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
                ensure!(!question.trim().is_empty(), "问题不能为空");
                let selected = session
                    .questions
                    .start(viewer, &question, follow_up)
                    .await?;
                Ok(Response::Questions(
                    session
                        .questions
                        .snapshot(viewer, None, Some(selected))
                        .await?,
                ))
            }
            Request::OpenQuestions { session } => Ok(Response::Questions(
                self.session(&session)
                    .await?
                    .questions
                    .open(viewer)
                    .await?,
            )),
            Request::CloseQuestions { session } => {
                self.session(&session)
                    .await?
                    .questions
                    .release(viewer)
                    .await;
                Ok(Response::Ok)
            }
            Request::ReadQuestions { session, after } => Ok(Response::Questions(
                self.session(&session)
                    .await?
                    .questions
                    .snapshot(viewer, after, None)
                    .await?,
            )),
            Request::CancelQuestion { session, index } => {
                self.session(&session)
                    .await?
                    .questions
                    .cancel(viewer, index)
                    .await;
                Ok(Response::Ok)
            }
            Request::SessionAction { session, action } => {
                if let Some(mut browsing) = self.browsing(&session).await {
                    return super::actions::browse(&mut browsing, connection_options, action).await;
                }
                let directory_change = matches!(
                    action,
                    orbit_protocol::SessionAction::ChangeDirectory { .. }
                );
                let report = self
                    .session(&session)
                    .await?
                    .action(action)
                    .await?;
                Ok(if directory_change {
                    Response::Ok
                } else {
                    Response::Report(report)
                })
            }
            Request::Usage { session } => {
                let handle = self.session(&session).await?;
                let (config, persisted) = handle.usage_context().await?;
                // Account HTTP and database reads run outside the session actor.
                Ok(Response::Usage(
                    super::usage::snapshot(&config, &handle.store, &session, persisted).await,
                ))
            }
            Request::InspectUsage {
                session,
                configuration,
            } => {
                if let Ok(handle) = self.session(&session).await {
                    let (config, persisted) = handle.usage_context().await?;
                    return Ok(Response::Usage(
                        super::usage::snapshot(&config, &handle.store, &session, persisted).await,
                    ));
                }
                let browsing = self
                    .browsing(&session)
                    .await
                    .context("conversation is not open")?;
                let info = browsing.info();
                let store = browsing.session.store.clone();
                let retained = browsing.configuration.clone();
                drop(browsing);
                let installed = match configuration {
                    Some(configuration) => Some(
                        super::configuration::Installed::install(configuration, connection_options)
                            .await
                            .map(Arc::new)?,
                    ),
                    None => retained.map_err(|()| anyhow::anyhow!("远程配置尚未就绪"))?,
                };
                let options = installed
                    .as_ref()
                    .map_or(connection_options, |installed| &installed.options);
                let mut config =
                    crate::config::FileConfig::load(&options.config, &options.auth_directory)?;
                orbit_config::models::updater::load_cached(&mut config).await;
                let model = config
                    .select_model(&info.model)
                    .ok()
                    .or_else(|| {
                        config
                            .model_options()
                            .ok()?
                            .into_iter()
                            .next()
                    });
                let snapshot = match model {
                    Some(model) => {
                        super::usage::snapshot(&model.config, &store, &session, info.persisted)
                            .await
                    }
                    None => orbit_protocol::usage::Snapshot {
                        session: super::usage::session_usage(&store, &session, info.persisted)
                            .await,
                        ..Default::default()
                    },
                };
                Ok(Response::Usage(snapshot))
            }
            Request::ConfigureModels {
                session,
                configuration,
            } => {
                if let Some(mut browsing) = self.browsing(&session).await {
                    let installed = match configuration {
                        Some(configuration) => Some(
                            super::configuration::Installed::install(
                                configuration,
                                connection_options,
                            )
                            .await
                            .map(Arc::new)?,
                        ),
                        None => browsing
                            .configuration
                            .as_ref()
                            .map_err(|()| anyhow::anyhow!("远程配置尚未就绪"))?
                            .clone(),
                    };
                    let options = installed
                        .as_ref()
                        .map_or(connection_options, |installed| &installed.options);
                    let mut config =
                        crate::config::FileConfig::load(&options.config, &options.auth_directory)?;
                    orbit_config::models::updater::load_cached(&mut config).await;
                    let models = config.model_options()?;
                    if browsing.info().model.is_empty()
                        && let Some(model) = models.first()
                    {
                        browsing.selection = Some((
                            model.qualified_name(),
                            model
                                .config
                                .reasoning_effort
                                .map(|effort| effort.as_str().into()),
                        ));
                    }
                    let mut info = browsing.info();
                    super::profile::catalog(&mut info, &models);
                    browsing.configuration = Ok(installed);
                    return Ok(Response::Profile(Box::new(info)));
                }
                self.session(&session)
                    .await?
                    .command(session::Command::ConfigureModels { configuration })
                    .await?;
                Ok(Response::Ok)
            }
            Request::Plan { session, action } => {
                if let Some(mut browsing) = self.browsing(&session).await {
                    if action == orbit_protocol::plan::Action::Enable {
                        return Ok(Response::Ok);
                    }
                    ensure!(
                        matches!(
                            action,
                            orbit_protocol::plan::Action::Enter
                                | orbit_protocol::plan::Action::Leave
                        ),
                        "该计划操作需要先准备模型请求"
                    );
                    let state = &browsing.session;
                    if !state.is_persisted() {
                        browsing.plan_mode = Some(match action {
                            orbit_protocol::plan::Action::Enter => {
                                orbit_protocol::plan::WorkMode::Plan
                            }
                            orbit_protocol::plan::Action::Leave => {
                                orbit_protocol::plan::WorkMode::Build
                            }
                            _ => bail!("当前会话没有待审阅的计划"),
                        });
                        return Ok(Response::Ok);
                    }
                    state
                        .validate_directory(&browsing.directory)
                        .await?;
                    let planning = crate::planning::Planning::new(
                        state.store.clone(),
                        session,
                        orbit_runtime::filesystem::Workspace::new(&browsing.directory)?,
                    )
                    .await?;
                    planning.action(action).await?;
                    return Ok(Response::Ok);
                }
                self.session(&session)
                    .await?
                    .command(session::Command::Plan { action })
                    .await?;
                Ok(Response::Ok)
            }
            Request::SetMode { session, mode } => {
                if let Some(mut browsing) = self.browsing(&session).await {
                    if browsing.session.is_persisted() {
                        browsing
                            .session
                            .store
                            .set_tui_mode(
                                &session,
                                match mode {
                                    orbit_protocol::Mode::Ask => "ask",
                                    orbit_protocol::Mode::Auto => "auto",
                                    orbit_protocol::Mode::Yolo => "YOLO",
                                },
                            )
                            .await?;
                    }
                    browsing.mode = mode;
                    browsing.mode_revision += 1;
                    return Ok(Response::Profile(Box::new(browsing.info())));
                }
                let handle = self.session(&session).await?;
                handle
                    .command(session::Command::SetMode { mode })
                    .await?;
                Ok(Response::Profile(Box::new(handle.current_info().await)))
            }
            Request::SetModel {
                session,
                model,
                effort,
            } => {
                if let Some(mut browsing) = self.browsing(&session).await {
                    browsing.selection = Some((model, effort));
                    return Ok(Response::Profile(Box::new(browsing.info())));
                }
                self.session(&session)
                    .await?
                    .command(session::Command::SetModel { model, effort })
                    .await?;
                Ok(Response::Ok)
            }
            Request::QueueInput {
                session,
                message,
                images,
                files,
            } => {
                let (accepted, receipt) = tokio::sync::oneshot::channel();
                self.session(&session)
                    .await?
                    .command(session::Command::Steer {
                        delivery: orbit_agent::inbox::Delivery::NextRun,
                        message,
                        images,
                        files,
                        accepted,
                    })
                    .await?;
                Ok(match receipt.await? {
                    Some(message_id) => Response::InputQueued { message_id },
                    None => Response::Ok,
                })
            }
            Request::SteerImages {
                session,
                message,
                images,
            } => {
                let (accepted, receipt) = tokio::sync::oneshot::channel();
                self.session(&session)
                    .await?
                    .command(session::Command::Steer {
                        delivery: orbit_agent::inbox::Delivery::AtBoundary,
                        message,
                        images,
                        files: Vec::new(),
                        accepted,
                    })
                    .await?;
                Ok(Response::InputQueued {
                    message_id: receipt
                        .await?
                        .context("missing steering receipt")?,
                })
            }
            Request::Steer { session, message } => {
                let (accepted, receipt) = tokio::sync::oneshot::channel();
                self.session(&session)
                    .await?
                    .command(session::Command::Steer {
                        delivery: orbit_agent::inbox::Delivery::AtBoundary,
                        message,
                        images: vec![],
                        files: Vec::new(),
                        accepted,
                    })
                    .await?;
                Ok(Response::InputQueued {
                    message_id: receipt
                        .await?
                        .context("missing steering receipt")?,
                })
            }
            Request::EnableUserInput { session } => {
                self.session(&session)
                    .await?
                    .command(session::Command::EnableUserInput)
                    .await?;
                Ok(Response::Ok)
            }
            Request::AnswerUserInput {
                session,
                request,
                answers,
            } => {
                self.session(&session)
                    .await?
                    .command(session::Command::AnswerUserInput {
                        id: request,
                        answers,
                    })
                    .await?;
                Ok(Response::Ok)
            }
            Request::UserInputHistory { session, before } => {
                let (store, _) = self.conversation(&session).await?;
                Ok(Response::UserInputs(
                    store
                        .user_input_history(&session, before)
                        .await?,
                ))
            }
            Request::Poll { session, after } => {
                if let Some(browsing) = self.browsing(&session).await {
                    return Ok(Response::Page(browsing.page(after).await?));
                }
                Ok(Response::Page(
                    self.session(&session)
                        .await?
                        .page(viewer, after)
                        .await?,
                ))
            }
            Request::Undo { session, path } => {
                self.session(&session)
                    .await?
                    .command(session::Command::Undo { path })
                    .await?;
                Ok(Response::Ok)
            }
            Request::Cancel { session } => {
                if self.browsing(&session).await.is_some() {
                    return Ok(Response::Ok);
                }
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
            Request::ReadToolOutput {
                session,
                agent,
                source,
                offset,
            } => {
                let store = self
                    .archive_store(&session, agent.as_deref(), connection_options)
                    .await?;
                let directory = store
                    .path()
                    .with_extension("command-output")
                    .join(agent.as_deref().unwrap_or(&session));
                Ok(Response::ToolOutput(
                    super::tool_output::read(directory, source, offset).await?,
                ))
            }
            Request::ReadFile {
                session,
                path,
                allow_missing,
            } => {
                let (store, attachment, workspace) =
                    if let Some(browsing) = self.browsing(&session).await {
                        let store = browsing.session.store.clone();
                        let attachment = store
                            .session_has_attachment(&session, &path)
                            .await?;
                        let workspace = if attachment {
                            None
                        } else {
                            Some(
                                super::files::browsing_access(&browsing, connection_options)
                                    .await?
                                    .0,
                            )
                        };
                        (store, attachment, workspace)
                    } else {
                        let session = self.session(&session).await?;
                        let attachment = session.has_image(&path).await;
                        let workspace = if attachment {
                            None
                        } else {
                            Some(
                                session
                                    .workspace
                                    .read()
                                    .map_err(|_| anyhow::anyhow!("workspace state unavailable"))?
                                    .clone(),
                            )
                        };
                        (session.store.clone(), attachment, workspace)
                    };
                let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
                    if attachment {
                        return orbit_tools::image_assets::ImageAssets::for_database(store.path())
                            .read(Path::new(&path));
                    }
                    use std::io::Read;
                    let file = workspace
                        .as_ref()
                        .context("workspace state unavailable")?
                        .authorize_read(Path::new(&path))?;
                    let mut bytes = Vec::new();
                    file.take(MAX_FILE_BYTES + 1)
                        .read_to_end(&mut bytes)?;
                    ensure!(
                        bytes.len() as u64 <= MAX_FILE_BYTES,
                        "workspace file exceeds 20 MiB"
                    );
                    Ok(bytes)
                })
                .await?;
                let bytes = match bytes {
                    Ok(bytes) => bytes,
                    Err(error)
                        if allow_missing
                            && error.chain().any(|cause| {
                                cause
                                    .downcast_ref::<std::io::Error>()
                                    .is_some_and(|error| {
                                        error.kind() == std::io::ErrorKind::NotFound
                                    })
                            }) =>
                    {
                        return Ok(Response::FileMissing);
                    }
                    Err(error) => return Err(error),
                };
                Ok(Response::File(
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                ))
            }
            Request::ReadSubagent {
                session,
                agent,
                after,
            } => {
                if let Ok(handle) = self.session(&session).await {
                    return Ok(Response::Subagent(
                        handle
                            .subagent_page(&agent, after)
                            .await?,
                    ));
                }
                let store = self
                    .archive_store(&session, Some(&agent), connection_options)
                    .await?;
                Ok(Response::Subagent(
                    session::archived_page(&store, &agent, after).await?,
                ))
            }
            Request::SendSubagent {
                session,
                agent,
                message,
            } => {
                let _opening = self.opening.lock().await;
                ensure!(
                    !self.closing.load(Ordering::Acquire),
                    "workspace service is shutting down"
                );
                ensure!(
                    !message.trim().is_empty() && message.len() <= 1_024 * 1_024,
                    "invalid subagent message"
                );
                let session = self.session(&session).await?;
                session
                    .authorize_subagent(&agent)
                    .await?;
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
                self.closing
                    .store(true, Ordering::Release);
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
            orbit_protocol::Capabilities {
                system: std::env::consts::OS.into(),
                architecture: std::env::consts::ARCH.into(),
                shell: std::env::var("SHELL").unwrap_or_else(|_| {
                    #[cfg(windows)]
                    {
                        std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into())
                    }
                    #[cfg(unix)]
                    {
                        "/bin/sh".into()
                    }
                }),
                native_processes: true,
                files: true,
                features: [
                    orbit_protocol::MODEL_INDEPENDENT_SESSIONS,
                    "sessions",
                    "events",
                    "approvals",
                    "cancel",
                    "files",
                    "retained_tool_output",
                    "subagents",
                    "images",
                    "compact",
                    "steer",
                    "side_questions",
                    "user_input",
                    "planning",
                    "project_state",
                    "plugins",
                    "completions",
                    "administration",
                    "configuration",
                    "undo",
                    "development_worktrees",
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            }
        })
        .await
        .clone()
}

#[cfg(test)]
mod tests;
