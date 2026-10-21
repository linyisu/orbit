//! TLS gateway for explicitly provisioned controllers of one host workspace.

use anyhow::{Result, bail, ensure};
pub use orbit_protocol::remote::Permissions;
use orbit_protocol::{Request, Response, remote};
use orbit_store::remote as registration;
use std::{collections::HashSet, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::Semaphore,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        self,
        pki_types::{CertificateDer, PrivatePkcs8KeyDer},
    },
};

trait GatewayStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> GatewayStream for T {}

const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_CONNECTIONS: usize = 8;

#[path = "remote_host.rs"]
pub(super) mod host;
#[path = "remote_upload.rs"]
mod upload;

#[derive(Clone)]
pub struct HostOptions {
    pub state: PathBuf,
    pub workspace: PathBuf,
    pub database: PathBuf,
    pub config: PathBuf,
    pub auth: PathBuf,
}

fn random_secret() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| anyhow::anyhow!("entropy unavailable: {error}"))?;
    Ok(bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Explicit host-side authorization. The output contains one device's private credential.
pub fn authorize(
    options: &HostOptions,
    address: String,
    output: PathBuf,
    permissions: remote::Permissions,
) -> Result<String> {
    let pairing = provision(
        options,
        address,
        permissions,
        registration::Scope::Workspace,
        "手机".into(),
    )?;
    if let Err(error) = registration::create(&output, &serde_json::to_vec(&pairing)?) {
        registration::revoke(&options.state, &pairing.device)?;
        return Err(error);
    }
    Ok(pairing.device)
}

fn identity(options: &HostOptions) -> Result<()> {
    registration::directory(&options.state)?;
    let certificate = options.state.join("host.der");
    let key = options.state.join("host-key.der");
    if !certificate.exists() && !key.exists() {
        let generated = rcgen::generate_simple_self_signed(vec![remote::SERVER_NAME.into()])?;
        registration::create(&key, &generated.signing_key.serialize_der())?;
        registration::create(&certificate, generated.cert.der().as_ref())?;
    }
    ensure!(key.is_file(), "host identity is incomplete");
    Ok(())
}

fn host_name() -> String {
    #[cfg(unix)]
    {
        let mut bytes = [0u8; 256];
        // SAFETY: the writable buffer has the supplied length and is read only within that bound.
        if unsafe { libc::gethostname(bytes.as_mut_ptr().cast(), bytes.len()) } == 0 {
            let end = bytes
                .iter()
                .position(|b| *b == 0)
                .unwrap_or(bytes.len());
            let name = String::from_utf8_lossy(&bytes[..end])
                .trim()
                .to_owned();
            if !name.is_empty() {
                return name;
            }
        }
    }
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "我的电脑".into())
}

fn provision(
    options: &HostOptions,
    address: String,
    permissions: remote::Permissions,
    scope: registration::Scope,
    name: String,
) -> Result<remote::Pairing> {
    identity(options)?;
    let device = random_secret()?;
    let secret = random_secret()?;
    let grant = registration::Grant {
        name: name
            .chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect(),
        scope,
        workspace: options.workspace.canonicalize()?,
        database: std::path::absolute(&options.database)?,
        permissions,
        secret_hash: registration::digest(&secret),
    };
    let pairing = remote::Pairing {
        host_name: host_name(),
        version: remote::VERSION,
        address,
        certificate: std::fs::read(options.state.join("host.der"))?,
        device: device.clone(),
        secret,
    };
    registration::create(
        &registration::device_path(&options.state, &device)?,
        &serde_json::to_vec(&grant)?,
    )?;
    Ok(pairing)
}

fn acceptor(options: &HostOptions) -> Result<TlsAcceptor> {
    let certificate = CertificateDer::from(std::fs::read(options.state.join("host.der"))?);
    let key = PrivatePkcs8KeyDer::from(std::fs::read(options.state.join("host-key.der"))?);
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(vec![certificate], key.into())?;
    Ok(TlsAcceptor::from(Arc::new(tls)))
}

pub fn revoke(state: PathBuf, device: &str) -> Result<()> {
    registration::revoke(&state, device)
}

/// Only an explicitly selected listener is opened. TLS and device authorization precede service I/O.
pub async fn serve(options: HostOptions, listen: SocketAddr) -> Result<()> {
    registration::directory(&options.state)?;
    let certificate = CertificateDer::from(std::fs::read(options.state.join("host.der"))?);
    let key = PrivatePkcs8KeyDer::from(std::fs::read(options.state.join("host-key.der"))?);
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(vec![certificate], key.into())?;
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let listener = tokio::net::TcpListener::bind(listen).await?;
    let options = Arc::new(options);
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (socket, _) = listener.accept().await?;
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            continue;
        };
        let acceptor = acceptor.clone();
        let options = options.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let result: Result<()> = async {
                let mut stream =
                    tokio::time::timeout(AUTH_TIMEOUT, acceptor.accept(socket)).await??;
                let authentication: remote::Authentication =
                    tokio::time::timeout(AUTH_TIMEOUT, remote::read(&mut stream)).await??;
                let grant = match validate(&options, &authentication) {
                    Ok(grant) => grant,
                    Err(_) => {
                        remote::write(&mut stream, &remote::Welcome::Denied).await?;
                        return Ok(());
                    }
                };
                remote::write(
                    &mut stream,
                    &remote::Welcome::Connected {
                        workspace: grant.workspace.display().to_string(),
                        permissions: grant.permissions,
                    },
                )
                .await?;
                connection(&options, authentication, stream).await
            }
            .await;
            if result.is_err() {
                eprintln!("remote control connection closed");
            }
        });
    }
}

fn validate(
    options: &HostOptions,
    authentication: &remote::Authentication,
) -> Result<registration::Grant> {
    ensure!(
        authentication.version == remote::VERSION,
        "remote protocol mismatch"
    );
    let grant = registration::authenticate(
        &options.state,
        &authentication.device,
        &authentication.secret,
    )?;
    ensure!(
        (grant.scope == registration::Scope::Computer
            || grant.workspace == options.workspace.canonicalize()?)
            && grant.database == std::path::absolute(&options.database)?,
        "device scope mismatch"
    );
    Ok(grant)
}

async fn connection(
    options: &HostOptions,
    authentication: remote::Authentication,
    mut stream: impl AsyncRead + AsyncWrite + Unpin,
) -> Result<()> {
    let grant = validate(options, &authentication)?;
    let mut selected = options.clone();
    let args = super::ServiceOptions {
        workspace: selected.workspace.display().to_string(),
        daemon: false,
        allow_mcp_start: grant.scope == registration::Scope::Computer && grant.permissions.control,
    };
    let mut backend: Box<dyn GatewayStream> = Box::new(
        super::server::remote_backend(
            &args,
            options.config.clone(),
            options.database.clone(),
            options.auth.clone(),
        )
        .await?,
    );
    let mut opened = HashSet::new();
    let mut uploads = upload::Uploads::default();
    loop {
        // A revoked idle controller loses the socket too, not just its next write permission.
        let request = {
            let read = remote::read::<remote::Request>(&mut stream);
            tokio::pin!(read);
            loop {
                tokio::select! {
                    request = &mut read => break request?,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => { validate(options, &authentication)?; }
                }
            }
        };
        let grant = validate(options, &authentication)?;
        let request = match select_workspace(request, &grant, &mut selected, &mut backend).await {
            Ok(request) => request,
            Err(error) => {
                remote::write(
                    &mut stream,
                    &super::protocol::Response::Error(error.to_string()),
                )
                .await?;
                continue;
            }
        };
        let response = match tokio::time::timeout(
            REQUEST_TIMEOUT,
            dispatch(
                request,
                &grant,
                &mut opened,
                &mut backend,
                &selected,
                &authentication,
                &mut uploads,
            ),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                let response: super::protocol::Response = Response::Error(error.to_string());
                if error.is::<std::io::Error>() {
                    remote::write(&mut stream, &response).await?;
                    return Err(error);
                }
                response
            }
            Err(_) => bail!("request result unconfirmed"),
        };
        validate(options, &authentication)?;
        tokio::time::timeout(REQUEST_TIMEOUT, remote::write(&mut stream, &response)).await??;
    }
}

async fn select_workspace(
    request: remote::Request,
    grant: &registration::Grant,
    options: &mut HostOptions,
    backend: &mut Box<dyn GatewayStream>,
) -> Result<remote::Request> {
    let path = match &request {
        remote::Request::Workspace { path, resume } => {
            ensure!(
                grant.scope == registration::Scope::Computer,
                "请重新扫码授权整台电脑"
            );
            ensure!(
                resume.is_some() || grant.permissions.control,
                "device is read-only"
            );
            Some(PathBuf::from(path))
        }
        remote::Request::Browse { resume: Some(id) }
            if grant.scope == registration::Scope::Computer =>
        {
            let Response::Sessions(sessions) = call(backend, Request::AllSessions).await? else {
                bail!("cannot list host sessions");
            };
            sessions
                .iter()
                .find(|s| &s.id == id)
                .map(|s| PathBuf::from(&s.workspace))
        }
        _ => None,
    };
    if let Some(path) = path {
        let path = path.canonicalize()?;
        ensure!(path.is_dir(), "请选择电脑上的目录");
        if path != options.workspace {
            let args = super::ServiceOptions {
                workspace: path.display().to_string(),
                daemon: false,
                allow_mcp_start: grant.scope == registration::Scope::Computer
                    && grant.permissions.control,
            };
            // Keep the previous connection usable if opening the new project fails.
            let next = super::server::remote_backend(
                &args,
                options.config.clone(),
                options.database.clone(),
                options.auth.clone(),
            )
            .await?;
            // The concrete transport is deliberately erased at the gateway boundary.
            *backend = Box::new(next);
            options.workspace = path;
        }
    }
    Ok(match request {
        remote::Request::Workspace { resume, .. } => remote::Request::Browse { resume },
        request => request,
    })
}

async fn call(
    backend: &mut (impl AsyncRead + AsyncWrite + Unpin),
    request: Request,
) -> Result<super::protocol::Response> {
    orbit_protocol::write(backend, &request).await?;
    Ok(orbit_protocol::read(backend).await?)
}

async fn dispatch(
    request: remote::Request,
    grant: &registration::Grant,
    opened: &mut HashSet<String>,
    backend: &mut (impl AsyncRead + AsyncWrite + Unpin),
    options: &HostOptions,
    authentication: &remote::Authentication,
    uploads: &mut upload::Uploads,
) -> Result<super::protocol::Response> {
    let request = match request {
        remote::Request::Action { request } => {
            ensure!(
                grant.scope == registration::Scope::Computer,
                "请重新扫码授权整台电脑"
            );
            let (access, session) = remote::access(&request)
                .ok_or_else(|| anyhow::anyhow!("operation is unavailable remotely"))?;
            ensure!(
                match access {
                    remote::Access::Read => true,
                    remote::Access::Control => grant.permissions.control,
                    remote::Access::Approve => grant.permissions.approve,
                },
                "operation is not authorized"
            );
            if let Some(session) = session {
                ensure!(opened.contains(session), "open the session first");
            }
            *request
        }
        remote::Request::UploadBegin {
            session,
            id,
            name,
            bytes,
            sha256,
            image,
        } => {
            ensure!(
                grant.permissions.control && opened.contains(&session),
                "session control is not authorized"
            );
            uploads.begin(session, id, name, bytes, sha256, image)?;
            return Ok(Response::Ok);
        }
        remote::Request::UploadChunk { id, offset, data } => {
            ensure!(grant.permissions.control, "device is read-only");
            uploads.chunk(&id, offset, &data)?;
            return Ok(Response::Ok);
        }
        remote::Request::Submit {
            session,
            request_id,
            prompt,
            attachments,
        } => {
            ensure!(
                grant.permissions.control && opened.contains(&session),
                "session control is not authorized"
            );
            let prepared = call(
                backend,
                Request::Prepare {
                    session: session.clone(),
                    model: None,
                    configuration: None,
                },
            )
            .await?;
            if matches!(prepared, Response::Error(_)) {
                return Ok(prepared);
            }
            let enabled = call(
                backend,
                Request::EnableUserInput {
                    session: session.clone(),
                },
            )
            .await?;
            if matches!(enabled, Response::Error(_)) {
                return Ok(enabled);
            }
            let (images, files) = uploads.finish(&session, &attachments)?;
            Request::SubmitAttachments {
                session,
                request_id,
                prompt,
                images,
                files,
                steer: false,
            }
        }
        remote::Request::Sessions => {
            if grant.scope == registration::Scope::Computer {
                Request::AllSessions
            } else {
                Request::Sessions
            }
        }
        remote::Request::Workspace { .. } => {
            unreachable!("workspace selection is handled before dispatch")
        }
        remote::Request::Browse { resume } => {
            if resume.is_none() {
                ensure!(grant.permissions.control, "device is read-only");
            }
            Request::Browse {
                resume,
                configuration: None,
            }
        }
        remote::Request::Poll { session, after } => {
            ensure!(opened.contains(&session), "open the session first");
            Request::Poll { session, after }
        }
        remote::Request::Start {
            session,
            request_id,
            prompt,
        } => {
            ensure!(
                grant.permissions.control && opened.contains(&session),
                "session control is not authorized"
            );
            let prepared = call(
                backend,
                Request::Prepare {
                    session: session.clone(),
                    model: None,
                    configuration: None,
                },
            )
            .await?;
            if matches!(prepared, Response::Error(_)) {
                return Ok(prepared);
            }
            let enabled = call(
                backend,
                Request::EnableUserInput {
                    session: session.clone(),
                },
            )
            .await?;
            if matches!(enabled, Response::Error(_)) {
                return Ok(enabled);
            }
            Request::Start {
                session,
                request_id,
                prompt,
                images: Vec::new(),
                compact: false,
                model: None,
            }
        }
        remote::Request::Cancel { session } => {
            ensure!(
                grant.permissions.control && opened.contains(&session),
                "session control is not authorized"
            );
            Request::Cancel { session }
        }
        remote::Request::AnswerUserInput {
            session,
            request,
            answers,
        } => {
            ensure!(
                grant.permissions.control && opened.contains(&session),
                "session control is not authorized"
            );
            Request::AnswerUserInput {
                session,
                request,
                answers,
            }
        }
        remote::Request::Approve {
            session,
            approval,
            allow,
        } => {
            ensure!(
                grant.permissions.approve && opened.contains(&session),
                "approval is not authorized"
            );
            Request::Approve {
                session,
                approval,
                allow,
                remember: false,
            }
        }
    };
    validate(options, authentication)?;
    let mut response = call(backend, request).await?;
    if let Response::Sessions(sessions) = &mut response
        && grant.scope == registration::Scope::Workspace
    {
        sessions.retain(|session| std::path::Path::new(&session.workspace) == grant.workspace);
    }
    if let Response::Opened(info) = &response {
        ensure!(
            std::path::Path::new(&info.workspace) == options.workspace,
            "workspace identity changed"
        );
        opened.insert(info.id.clone());
    }
    Ok(response)
}
