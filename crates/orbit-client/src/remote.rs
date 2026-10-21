//! Pinned TLS controller transport; never deploys a service or loads host credentials.

use anyhow::{Context, Result, ensure};
pub mod presentation;
pub mod vault;
pub use orbit_protocol as protocol;
use orbit_protocol::remote;
pub use orbit_protocol::remote::{Pairing, Permissions, Request, Response, Welcome};
pub use orbit_protocol::user_input;
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
    stream: Option<TlsStream<Box<dyn Duplex>>>,
    _endpoint: Option<iroh::Endpoint>,
    pub workspace: String,
    pub permissions: Permissions,
}

trait Duplex: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin> Duplex for T {}

async fn dial(
    address: &str,
    certificate: Vec<u8>,
) -> Result<(TlsStream<Box<dyn Duplex>>, Option<iroh::Endpoint>)> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(certificate))?;
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let (socket, endpoint) = socket(address).await?;
    let stream = TlsConnector::from(Arc::new(tls))
        .connect(ServerName::try_from(remote::SERVER_NAME)?, socket)
        .await?;
    Ok((stream, endpoint))
}

/// Registers through a bounded, single-use Orbit invitation.
pub async fn pair(code: &str) -> Result<Pairing> {
    pair_saved(code, &[]).await
}

/// Reuses an authenticated saved grant for the same host before requesting a new grant.
pub async fn pair_saved(code: &str, saved: &[Pairing]) -> Result<Pairing> {
    pair_named(code, saved, "手机").await
}

/// Uses a display-only controller name; authorization remains bound to the stored credential.
pub async fn pair_named(code: &str, saved: &[Pairing], name: &str) -> Result<Pairing> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    ensure!(code.len() <= 8_192, "配对码过长");
    let mut display_name = None;
    let invite: remote::Invitation = if let Some(encoded) = code
        .trim()
        .strip_prefix("orbit-pair:2:")
    {
        let fields: Vec<_> = encoded.split('|').collect();
        ensure!((3..=4).contains(&fields.len()), "配对二维码内容不完整");
        if fields.len() == 4 {
            display_name = Some(String::from_utf8(URL_SAFE_NO_PAD.decode(fields[3])?)?);
        }
        remote::Invitation {
            version: remote::VERSION,
            address: fields[0].into(),
            certificate: fields[1].into(),
            token: fields[2].into(),
        }
    } else {
        let encoded = code
            .trim()
            .strip_prefix("orbit-pair:1:")
            .context("这不是 Orbit 配对二维码")?;
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded)?)?
    };
    ensure!(
        invite.version == remote::VERSION,
        "配对版本不兼容，请更新 Orbit"
    );
    tokio::time::timeout(Duration::from_secs(110), async {
        let certificate = URL_SAFE_NO_PAD.decode(&invite.certificate)?;
        for previous in saved
            .iter()
            .rev()
            .filter(|p| p.certificate == certificate)
        {
            let mut current = previous.clone();
            current.address = invite.address.clone();
            if let Some(name) = &display_name {
                current.host_name = name.clone();
            }
            if Client::connect(&current).await.is_ok() {
                return Ok(current);
            }
        }
        let (mut stream, _endpoint) = dial(&invite.address, certificate.clone()).await?;
        remote::write(
            &mut stream,
            &remote::Authentication {
                device_name: name
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(80)
                    .collect(),
                version: remote::VERSION,
                device: "pair".into(),
                secret: invite.token,
            },
        )
        .await?;
        match remote::read(&mut stream).await? {
            Welcome::Paired(pairing) => {
                ensure!(
                    pairing.version == remote::VERSION
                        && pairing.address == invite.address
                        && pairing.certificate == certificate
                        && !pairing.secret.is_empty(),
                    "电脑返回了不匹配的设备授权"
                );
                Ok(pairing)
            }
            _ => anyhow::bail!("配对邀请无效或已过期，请刷新二维码"),
        }
    })
    .await
    .context("配对连接超时，请检查两端网络后重试")?
}

async fn socket(address: &str) -> Result<(Box<dyn Duplex>, Option<iroh::Endpoint>)> {
    if let Some(encoded) = address.strip_prefix("iroh:") {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
        let address: iroh::EndpointAddr =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded)?)?;
        let endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0).await?;
        let connection = endpoint
            .connect(address, remote::ALPN)
            .await?;
        let (send, receive) = connection.open_bi().await?;
        Ok((Box::new(tokio::io::join(receive, send)), Some(endpoint)))
    } else {
        Ok((
            Box::new(tokio::net::TcpStream::connect(address).await?),
            None,
        ))
    }
}

impl Client {
    /// Authenticate only against the certificate transferred through host authorization.
    pub async fn connect(pairing: &Pairing) -> Result<Self> {
        ensure!(
            pairing.version == remote::VERSION,
            "remote protocol mismatch"
        );
        tokio::time::timeout(Duration::from_secs(15), async {
            let (mut stream, endpoint) =
                dial(&pairing.address, pairing.certificate.clone()).await?;
            remote::write(
                &mut stream,
                &remote::Authentication {
                    device_name: String::new(),
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
                _endpoint: endpoint,
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
    Workspace(String),
    Action(Box<orbit_protocol::Request>),
    SendAttachments {
        request_id: String,
        text: String,
        attachments: Vec<Attachment>,
    },
    Refresh,
    Answer {
        id: String,
        answers: Option<Vec<user_input::Answer>>,
    },
    Open(Option<String>),
    Send {
        request_id: String,
        text: String,
    },
    Stop,
    Approve {
        id: u64,
        allow: bool,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub bytes: Vec<u8>,
    pub image: bool,
}

async fn upload(
    client: &mut Client,
    session: &str,
    attachments: Vec<Attachment>,
) -> Result<Vec<String>> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha256};
    ensure!(
        attachments.len() <= 8
            && attachments
                .iter()
                .map(|a| a.bytes.len())
                .sum::<usize>()
                <= remote::ATTACHMENT_TOTAL_BYTES,
        "附件总大小不能超过 20 MiB，最多 8 个文件"
    );
    let mut ids = Vec::new();
    for attachment in attachments {
        let id = request_id();
        let response = client
            .request(Request::UploadBegin {
                session: session.into(),
                id: id.clone(),
                name: attachment.name,
                bytes: attachment.bytes.len(),
                sha256: Sha256::digest(&attachment.bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect(),
                image: attachment.image,
            })
            .await?;
        ensure!(matches!(response, Response::Ok), "电脑拒绝接收附件");
        for (ix, chunk) in attachment
            .bytes
            .chunks(remote::ATTACHMENT_CHUNK_BYTES)
            .enumerate()
        {
            let response = client
                .request(Request::UploadChunk {
                    id: id.clone(),
                    offset: ix * remote::ATTACHMENT_CHUNK_BYTES,
                    data: STANDARD.encode(chunk),
                })
                .await?;
            ensure!(
                matches!(response, Response::Ok),
                "附件上传失败，文件仍保留在手机"
            );
        }
        ids.push(id);
    }
    Ok(ids)
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

/// Reconnects transport and history without replaying commands whose result is unknown.
/// Dropping the command sender stops reconnection; host tasks outlive this controller.
pub async fn observe(
    pairing: Pairing,
    mut commands: tokio::sync::mpsc::Receiver<Command>,
    events: tokio::sync::mpsc::Sender<Event>,
) {
    let mut resume = None;
    loop {
        match observe_inner(pairing.clone(), &mut commands, &events, &mut resume).await {
            Ok(()) => return,
            Err(error) => {
                if events
                    .send(Event::Disconnected(error.to_string()))
                    .await
                    .is_err()
                {
                    return;
                }
                // Commands queued against a broken connection must be explicitly resubmitted.
                while commands.try_recv().is_ok() {}
                tokio::select! {
                    _ = events.closed() => return,
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                }
                if commands.is_closed() {
                    return;
                }
                while commands.try_recv().is_ok() {}
            }
        }
    }
}

async fn observe_inner(
    pairing: Pairing,
    commands: &mut tokio::sync::mpsc::Receiver<Command>,
    events: &tokio::sync::mpsc::Sender<Event>,
    resume: &mut Option<(String, String)>,
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
    if let Some((id, workspace)) = resume.clone() {
        let request = if control.workspace == workspace {
            Request::Browse {
                resume: Some(id.clone()),
            }
        } else {
            Request::Workspace {
                path: workspace.clone(),
                resume: Some(id.clone()),
            }
        };
        let response = control.request(request).await?;
        if matches!(&response, Response::Opened(_)) {
            session = Some(id.clone());
            polling.spawn(poll_history(pairing.clone(), id, workspace, events.clone()));
        } else {
            *resume = None;
        }
        events
            .send(Event::Response(response))
            .await?;
    }

    loop {
        tokio::select! {
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()); };
                let submitting = matches!(&command, Command::Send { .. } | Command::SendAttachments { .. });
                let request = match command {
                    Command::SendAttachments {request_id, text, attachments} => {
                        let session = session.clone().context("open a session first")?;
                        let attachments = upload(&mut control, &session, attachments).await?;
                        Request::Submit {session, request_id, prompt: text, attachments}
                    }
                    Command::Refresh => Request::Sessions,
                    Command::Answer { id, answers } => Request::AnswerUserInput {
                        session: session.clone().context("open a session first")?,
                        request: id,
                        answers,
                    },
                    Command::Action(request) => Request::Action { request },
                    command @ (Command::Open(_) | Command::Workspace(_)) => {
                        let request = match command {
                            Command::Open(resume) => Request::Browse { resume },
                            Command::Workspace(path) => Request::Workspace { path, resume: None },
                            _ => unreachable!(),
                        };
                        let response = control.request(request).await?;
                        let opened = match &response {
                            Response::Opened(info) => Some((info.id.clone(), info.workspace.clone())),
                            _ => None,
                        };
                        events.send(Event::Response(response)).await?;
                        if let Some((opened, workspace)) = opened {
                            session = Some(opened.clone());
                            *resume = Some((opened.clone(), workspace.clone()));
                            polling.abort_all();
                            while polling.join_next().await.is_some() {}
                            polling.spawn(poll_history(pairing.clone(), opened, workspace, events.clone()));
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
    workspace: String,
    events: tokio::sync::mpsc::Sender<Event>,
) -> Result<()> {
    let mut history = Client::connect(&pairing).await?;
    let attach = if history.workspace == workspace {
        Request::Browse {
            resume: Some(session.clone()),
        }
    } else {
        Request::Workspace {
            path: workspace,
            resume: Some(session.clone()),
        }
    };
    ensure!(
        matches!(history.request(attach).await?, Response::Opened(_)),
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
