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

const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_CONNECTIONS: usize = 8;

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
    registration::directory(&options.state)?;
    let certificate = options.state.join("host.der");
    let key = options.state.join("host-key.der");
    if !certificate.exists() && !key.exists() {
        let generated = rcgen::generate_simple_self_signed(vec![remote::SERVER_NAME.into()])?;
        registration::create(&key, &generated.signing_key.serialize_der())?;
        registration::create(&certificate, generated.cert.der().as_ref())?;
    }
    ensure!(key.is_file(), "host identity is incomplete");
    let device = random_secret()?;
    let secret = random_secret()?;
    let grant = registration::Grant {
        workspace: options.workspace.canonicalize()?,
        database: std::path::absolute(&options.database)?,
        permissions,
        secret_hash: registration::digest(&secret),
    };
    let pairing = remote::Pairing {
        version: remote::VERSION,
        address,
        certificate: std::fs::read(certificate)?,
        device: device.clone(),
        secret,
    };
    registration::create(
        &registration::device_path(&options.state, &device)?,
        &serde_json::to_vec(&grant)?,
    )?;
    if let Err(error) = registration::create(&output, &serde_json::to_vec(&pairing)?) {
        registration::revoke(&options.state, &device)?;
        return Err(error);
    }
    Ok(device)
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
        grant.workspace == options.workspace.canonicalize()?
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
    let args = super::ServiceOptions {
        workspace: options.workspace.display().to_string(),
        daemon: false,
        allow_mcp_start: false,
    };
    let mut backend = super::server::remote_backend(
        &args,
        options.config.clone(),
        options.database.clone(),
        options.auth.clone(),
    )
    .await?;
    let mut opened = HashSet::new();
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
        let response = match tokio::time::timeout(
            REQUEST_TIMEOUT,
            dispatch(
                request,
                &grant,
                &mut opened,
                &mut backend,
                options,
                &authentication,
            ),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                let response: super::protocol::Response = Response::Error(error.to_string());
                remote::write(&mut stream, &response).await?;
                return Err(error);
            }
            Err(_) => bail!("request result unconfirmed"),
        };
        validate(options, &authentication)?;
        tokio::time::timeout(REQUEST_TIMEOUT, remote::write(&mut stream, &response)).await??;
    }
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
) -> Result<super::protocol::Response> {
    let request = match request {
        remote::Request::Sessions => Request::Sessions,
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
    if let Response::Sessions(sessions) = &mut response {
        sessions.retain(|session| std::path::Path::new(&session.workspace) == grant.workspace);
    }
    if let Response::Opened(info) = &response {
        ensure!(
            std::path::Path::new(&info.workspace) == grant.workspace,
            "workspace identity changed"
        );
        opened.insert(info.id.clone());
    }
    Ok(response)
}
