//! Pinned TLS controller transport; never deploys a service or loads host credentials.

use anyhow::{Context, Result, ensure};
use orbit_protocol::remote;
pub use orbit_protocol::remote::{Pairing, Permissions, Request, Response, Welcome};
pub use orbit_protocol::{Session, SessionInfo, Update, request_id};
use std::{sync::Arc, time::Duration};
use tokio_rustls::{
    TlsConnector,
    client::TlsStream,
    rustls::{
        self,
        pki_types::{CertificateDer, ServerName},
    },
};

const TIMEOUT: Duration = Duration::from_secs(120);

pub struct Client {
    stream: Option<TlsStream<tokio::net::TcpStream>>,
    pub workspace: String,
    pub permissions: Permissions,
}

impl Client {
    /// Authenticate only against the certificate transferred through host authorization.
    pub async fn connect(pairing: &Pairing) -> Result<Self> {
        ensure!(
            pairing.version == remote::VERSION,
            "remote protocol mismatch"
        );
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(pairing.certificate.clone()))?;
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
        tokio::time::timeout(Duration::from_secs(15), async {
            let socket = tokio::net::TcpStream::connect(&pairing.address).await?;
            let mut stream = TlsConnector::from(Arc::new(tls))
                .connect(ServerName::try_from(remote::SERVER_NAME)?, socket)
                .await?;
            remote::write(
                &mut stream,
                &remote::Authentication {
                    version: remote::VERSION,
                    device: pairing.device.clone(),
                    secret: pairing.secret.clone(),
                },
            )
            .await?;
            let Welcome::Connected {
                workspace,
                permissions,
            } = remote::read(&mut stream).await?
            else {
                anyhow::bail!("device authorization was rejected");
            };
            Ok(Self {
                stream: Some(stream),
                workspace,
                permissions,
            })
        })
        .await
        .context("connection timed out")?
    }

    /// A failed transaction discards its socket. Mutations are never automatically retried.
    pub async fn request(&mut self, request: Request) -> Result<Response> {
        // Preflight encoding preserves the connection when nothing has been sent.
        ensure!(
            serde_json::to_vec(&request)?.len() <= remote::MAX_FRAME_BYTES,
            "request exceeds remote budget"
        );
        let mut stream = self
            .stream
            .take()
            .context("connection closed; request result may be unconfirmed")?;
        let response = tokio::time::timeout(TIMEOUT, async {
            remote::write(&mut stream, &request).await?;
            remote::read(&mut stream).await
        })
        .await
        .context("request result unconfirmed: timed out")??;
        self.stream = Some(stream);
        Ok(response)
    }
}

pub enum Command {
    Open(Option<String>),
    Send { request_id: String, text: String },
    Stop,
    Approve { id: u64, allow: bool },
}

pub enum Event {
    Connected {
        workspace: String,
        permissions: Permissions,
    },
    Response(Response),
    Page {
        session: String,
        page: orbit_protocol::Page<serde_json::Value, serde_json::Value>,
    },
    Submitted,
    Disconnected(String),
}

/// Foreground controller loop. Dropping its command sender closes transport, never the host run.
/// View owners cancel this future on backgrounding; reopening starts from a fresh history cursor.
pub async fn observe(
    pairing: Pairing,
    mut commands: tokio::sync::mpsc::Receiver<Command>,
    events: tokio::sync::mpsc::Sender<Event>,
) {
    let result = observe_inner(pairing, &mut commands, &events).await;
    if let Err(error) = result {
        let _ = events
            .send(Event::Disconnected(error.to_string()))
            .await;
    }
}

async fn observe_inner(
    pairing: Pairing,
    commands: &mut tokio::sync::mpsc::Receiver<Command>,
    events: &tokio::sync::mpsc::Sender<Event>,
) -> Result<()> {
    let mut control = Client::connect(&pairing).await?;
    events
        .send(Event::Connected {
            workspace: control.workspace.clone(),
            permissions: control.permissions,
        })
        .await?;
    events
        .send(Event::Response(
            control
                .request(Request::Sessions)
                .await?,
        ))
        .await?;
    let mut session: Option<String> = None;
    let mut polling = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()); };
                let submitting = matches!(&command, Command::Send { .. });
                let request = match command {
                    Command::Open(resume) => {
                        let response = control.request(Request::Browse { resume }).await?;
                        if let Response::Opened(info) = &response {
                            session = Some(info.id.clone());
                        }
                        events.send(Event::Response(response)).await?;
                        if let Some(session) = &session {
                            polling.abort_all();
                            while polling.join_next().await.is_some() {}
                            polling.spawn(poll_history(pairing.clone(), session.clone(), events.clone()));
                        }
                        continue;
                    }
                    Command::Send { request_id, text } => Request::Start {
                        session: session.clone().context("open a session first")?,
                        request_id,
                        prompt: text,
                    },
                    Command::Stop => Request::Cancel {
                        session: session.clone().context("open a session first")?,
                    },
                    Command::Approve { id, allow } => Request::Approve {
                        session: session.clone().context("open a session first")?,
                        approval: id,
                        allow,
                    },
                };
                let response = control.request(request).await?;
                if submitting && matches!(response, Response::Ok) {
                    events.send(Event::Submitted).await?;
                }
                events.send(Event::Response(response)).await?;
            }
            result = polling.join_next(), if !polling.is_empty() => {
                if let Some(result) = result { result??; }
            }
        }
    }
}

async fn poll_history(
    pairing: Pairing,
    session: String,
    events: tokio::sync::mpsc::Sender<Event>,
) -> Result<()> {
    let mut history = Client::connect(&pairing).await?;
    ensure!(
        matches!(
            history
                .request(Request::Browse {
                    resume: Some(session.clone())
                })
                .await?,
            Response::Opened(_)
        ),
        "cannot attach history reader"
    );
    let mut cursor = 0;
    loop {
        let response = history
            .request(Request::Poll {
                session: session.clone(),
                after: cursor,
            })
            .await?;
        let Response::Page(page) = response else {
            anyhow::bail!("history reader rejected the cursor");
        };
        cursor = page.next;
        let more = page.more;
        let busy = page.busy;
        events
            .send(Event::Page {
                session: session.clone(),
                page,
            })
            .await?;
        if !more {
            tokio::time::sleep(if busy {
                Duration::from_millis(500)
            } else {
                Duration::from_secs(2)
            })
            .await;
        }
    }
}
