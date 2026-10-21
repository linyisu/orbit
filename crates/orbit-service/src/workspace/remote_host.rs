//! Local management and one-use QR pairing for the remote gateway.
use super::*;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use orbit_protocol::remote::{HostAction, HostStatus, Invitation};
use tokio::sync::Mutex;

const INVITATION_LIFETIME: Duration = Duration::from_secs(300);
struct Invite {
    token_hash: [u8; 32],
    encoded: String,
    created: std::time::Instant,
    permissions: Permissions,
}
#[derive(Default)]
struct Pairings {
    invite: Option<Invite>,
}
struct Running {
    endpoint: iroh::Endpoint,
    task: tokio::task::JoinHandle<()>,
    options: HostOptions,
    address: String,
    pairings: Arc<Mutex<Pairings>>,
}
#[derive(Default)]
pub(in crate::workspace) struct Host(Option<Running>);

impl Host {
    pub fn enabled(&self) -> bool {
        self.0.is_some()
    }

    pub async fn stop(&mut self) {
        if let Some(running) = self.0.take() {
            running.task.abort();
            running.endpoint.close().await;
        }
    }

    pub async fn action(&mut self, action: HostAction, options: HostOptions) -> Result<HostStatus> {
        match action {
            HostAction::Enable if self.0.is_none() => {
                identity(&options)?;
                acceptor(&options)?;
                let key_file = options.state.join("endpoint.key");
                if !key_file.exists() {
                    registration::create(&key_file, random_secret()?.as_bytes())?;
                }
                let key: iroh::SecretKey = std::fs::read_to_string(key_file)?.parse()?;
                let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
                    .secret_key(key)
                    .alpns(vec![remote::ALPN.to_vec()])
                    .bind()
                    .await?;
                tokio::time::timeout(Duration::from_secs(30), endpoint.online())
                    .await
                    .map_err(|_| anyhow::anyhow!("无法连接公网中继，请检查网络后重试"))?;
                let address = format!(
                    "iroh:{}",
                    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&endpoint.addr())?)
                );
                let pairings = Arc::new(Mutex::new(Pairings::default()));
                let task = tokio::spawn(listen(
                    endpoint.clone(),
                    options.clone(),
                    address.clone(),
                    pairings.clone(),
                ));
                self.0 = Some(Running {
                    endpoint,
                    task,
                    options: options.clone(),
                    address,
                    pairings,
                });
            }
            HostAction::Disable => {
                self.stop().await;
            }
            HostAction::Invite { permissions } => {
                let running = self
                    .0
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("请先开启手机连接"))?;
                let token = random_secret()?;
                let invitation = Invitation {
                    version: remote::VERSION,
                    address: running.address.clone(),
                    certificate: URL_SAFE_NO_PAD
                        .encode(std::fs::read(running.options.state.join("host.der"))?),
                    token: token.clone(),
                };
                let encoded = format!(
                    "orbit-pair:2:{}|{}|{}|{}",
                    invitation.address,
                    invitation.certificate,
                    invitation.token,
                    URL_SAFE_NO_PAD.encode(host_name())
                );
                let mut pairings = running.pairings.lock().await;
                pairings.invite = Some(Invite {
                    token_hash: registration::digest(&token),
                    encoded,
                    created: std::time::Instant::now(),
                    permissions,
                });
            }
            HostAction::Confirm { .. } => bail!("配对已由二维码完成，无需再次确认"),
            HostAction::Revoke { device } => {
                registration::revoke(&options.state, &device)?;
            }
            HostAction::Status | HostAction::Enable => {}
        }
        let mut status = HostStatus {
            enabled: self.enabled(),
            workspace: options.workspace.display().to_string(),
            ..Default::default()
        };
        if let Some(running) = &self.0 {
            status.workspace = running
                .options
                .workspace
                .display()
                .to_string();
            let pairings = running.pairings.lock().await;
            status.invitation = pairings
                .invite
                .as_ref()
                .filter(|i| i.created.elapsed() < INVITATION_LIFETIME)
                .map(|i| i.encoded.clone());
        }
        if let Ok(entries) = std::fs::read_dir(&options.state) {
            for entry in entries.flatten() {
                if let Some(id) = entry
                    .file_name()
                    .to_str()
                    .and_then(|n| n.strip_prefix("device-"))
                    .and_then(|n| n.strip_suffix(".json"))
                {
                    if let Ok(bytes) = std::fs::read(entry.path())
                        && let Ok(grant) = serde_json::from_slice::<registration::Grant>(&bytes)
                        && !grant.name.is_empty()
                    {
                        status
                            .device_names
                            .insert(id.to_owned(), grant.name);
                    }
                    status.devices.push(id.to_owned());
                }
            }
        }
        status.devices.sort();
        Ok(status)
    }
}

async fn listen(
    endpoint: iroh::Endpoint,
    options: HostOptions,
    address: String,
    pairings: Arc<Mutex<Pairings>>,
) {
    let Ok(acceptor) = acceptor(&options) else {
        return;
    };
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    while let Some(incoming) = endpoint.accept().await {
        let Ok(slot) = slots.clone().try_acquire_owned() else {
            incoming.refuse();
            continue;
        };
        let (options, acceptor, address, pairings) = (
            options.clone(),
            acceptor.clone(),
            address.clone(),
            pairings.clone(),
        );
        tokio::spawn(async move {
            let _slot = slot;
            let result: Result<()> = async {
                let conn = tokio::time::timeout(AUTH_TIMEOUT, incoming).await??;
                let (send, recv) = tokio::time::timeout(AUTH_TIMEOUT, conn.accept_bi()).await??;
                let mut stream = tokio::time::timeout(
                    AUTH_TIMEOUT,
                    acceptor.accept(tokio::io::join(recv, send)),
                )
                .await??;
                let authentication: remote::Authentication =
                    tokio::time::timeout(AUTH_TIMEOUT, remote::read(&mut stream)).await??;
                if authentication.device == "pair" {
                    ensure!(
                        authentication.version == remote::VERSION,
                        "unsupported version"
                    );
                    let permissions = {
                        let mut state = pairings.lock().await;
                        let invite = state
                            .invite
                            .as_ref()
                            .ok_or_else(|| anyhow::anyhow!("pairing expired"))?;
                        ensure!(
                            invite.created.elapsed() < INVITATION_LIFETIME
                                && invite
                                    .token_hash
                                    .iter()
                                    .zip(registration::digest(&authentication.secret))
                                    .fold(0_u8, |difference, (left, right)| difference
                                        | (left ^ right))
                                    == 0,
                            "invalid invitation"
                        );
                        let invite = state
                            .invite
                            .take()
                            .expect("validated invitation");
                        invite.permissions
                    };
                    let pairing = provision(
                        &options,
                        address,
                        permissions,
                        registration::Scope::Computer,
                        authentication.device_name,
                    )?;
                    remote::write(&mut stream, &remote::Welcome::Paired(pairing)).await?;
                    // QUIC close would discard a queued welcome. Keep the connection alive
                    // until the controller receives it and closes its one-shot endpoint.
                    use tokio::io::AsyncWriteExt;
                    tokio::time::timeout(AUTH_TIMEOUT, stream.shutdown()).await??;
                    let _ = tokio::time::timeout(AUTH_TIMEOUT, conn.closed()).await;
                    return Ok(());
                }
                let grant = validate(&options, &authentication)?;
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
                eprintln!("remote connection closed");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn host(public: bool) -> (tempfile::TempDir, HostOptions, Host) {
        let directory = tempfile::tempdir().unwrap();
        let options = HostOptions {
            state: directory.path().join("remote"),
            workspace: directory.path().into(),
            database: directory.path().join("test.db"),
            config: directory.path().join("config.toml"),
            auth: directory.path().join("auth"),
        };
        identity(&options).unwrap();
        let mut builder =
            iroh::Endpoint::builder(iroh::endpoint::presets::N0).alpns(vec![remote::ALPN.to_vec()]);
        if !public {
            builder = builder
                .relay_mode(iroh::RelayMode::Disabled)
                .bind_addr("127.0.0.1:0")
                .unwrap();
        }
        let endpoint = builder.bind().await.unwrap();
        let address = if public {
            tokio::time::timeout(Duration::from_secs(30), endpoint.online())
                .await
                .unwrap();
            // Supplying only a relay address prevents the initial connection from using LAN hints.
            iroh::EndpointAddr::new(endpoint.id()).with_relay_url(
                endpoint
                    .addr()
                    .relay_urls()
                    .next()
                    .unwrap()
                    .clone(),
            )
        } else {
            iroh::EndpointAddr::new(endpoint.id()).with_ip_addr(endpoint.bound_sockets()[0])
        };
        let address = format!(
            "iroh:{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&address).unwrap())
        );
        let pairings = Arc::new(Mutex::new(Pairings::default()));
        let task = tokio::spawn(listen(
            endpoint.clone(),
            options.clone(),
            address.clone(),
            pairings.clone(),
        ));
        let host = Host(Some(Running {
            endpoint,
            task,
            options: options.clone(),
            address,
            pairings,
        }));
        (directory, options, host)
    }

    async fn pair_round_trip(public: bool) {
        let (_directory, options, mut host) = host(public).await;
        let status = host
            .action(
                HostAction::Invite {
                    permissions: Permissions {
                        control: true,
                        approve: false,
                    },
                },
                options.clone(),
            )
            .await
            .unwrap();
        let code = status.invitation.unwrap();
        let fields: Vec<_> = code
            .strip_prefix("orbit-pair:2:")
            .unwrap()
            .split('|')
            .collect();
        let mut invalid = Invitation {
            version: remote::VERSION,
            address: fields[0].into(),
            certificate: fields[1].into(),
            token: fields[2].into(),
        };
        invalid.token = "wrong-secret".into();
        let invalid = format!(
            "orbit-pair:1:{}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&invalid).unwrap())
        );
        assert!(
            orbit_client::remote::pair(&invalid)
                .await
                .is_err()
        );
        assert!(
            host.action(HostAction::Status, options.clone())
                .await
                .unwrap()
                .invitation
                .is_some()
        );
        let pairing = orbit_client::remote::pair_named(&code, &[], "测试设备 FinalZ")
            .await
            .unwrap();
        assert_eq!(pairing.host_name, host_name());
        assert!(
            orbit_client::remote::pair(&code)
                .await
                .is_err()
        );
        let reused = orbit_client::remote::pair_saved(&code, std::slice::from_ref(&pairing))
            .await
            .unwrap();
        assert_eq!(reused.device, pairing.device);

        let status = host
            .action(HostAction::Status, options.clone())
            .await
            .unwrap();
        assert!(status.invitation.is_none());
        assert!(status.pending.is_empty());
        assert_eq!(
            status
                .device_names
                .get(&pairing.device)
                .map(String::as_str),
            Some("测试设备 FinalZ")
        );
        let auth = remote::Authentication {
            device_name: String::new(),
            version: remote::VERSION,
            device: pairing.device.clone(),
            secret: pairing.secret,
        };
        assert!(validate(&options, &auth).unwrap().scope == registration::Scope::Computer);
        assert!(
            validate(&options, &auth)
                .unwrap()
                .permissions
                .control
        );
        assert!(
            !validate(&options, &auth)
                .unwrap()
                .permissions
                .approve
        );
        host.action(
            HostAction::Revoke {
                device: pairing.device,
            },
            options.clone(),
        )
        .await
        .unwrap();
        assert!(validate(&options, &auth).is_err());
        host.action(HostAction::Disable, options)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn pairing_completes_once_and_invalid_codes_do_not_consume_the_invitation() {
        tokio::time::timeout(Duration::from_secs(60), pair_round_trip(false))
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires public relay connectivity"]
    async fn public_relay_pairing_and_device_revocation() {
        tokio::time::timeout(Duration::from_secs(120), pair_round_trip(true))
            .await
            .unwrap();
    }
}
