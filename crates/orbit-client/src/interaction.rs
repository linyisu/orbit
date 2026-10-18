use std::{
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{mpsc, oneshot, watch},
};
use tokio_util::task::AbortOnDropHandle;

use super::protocol;
use orbit_runtime::ipc;
#[cfg(unix)]
use std::{os::unix::fs::PermissionsExt, process::Stdio};

const MAX_PROMPT_BYTES: usize = 16 * 1_024;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(300);
const ASKPASS_ENV: &str = "ORBIT_INTERNAL_SSH_ASKPASS";
const PARENT_ENV: &str = "ORBIT_SSH_ASKPASS_PARENT";
const SOCKET_ENV: &str = "ORBIT_SSH_ASKPASS_SOCKET";

#[derive(Clone)]
pub struct Progress {
    pub stage: watch::Sender<String>,
    pub logs: Arc<Mutex<std::collections::VecDeque<String>>>,
    waiting: Arc<AtomicUsize>,
    secrets: Arc<Mutex<Vec<String>>>,
    prompts: mpsc::Sender<Challenge>,
}

pub struct Updates {
    pub stage: watch::Receiver<String>,
    pub logs: Arc<Mutex<std::collections::VecDeque<String>>>,
    pub prompts: mpsc::Receiver<Challenge>,
}

pub struct Challenge {
    pub prompt: String,
    pub confirm: bool,
    pub reply: oneshot::Sender<Option<String>>,
}

#[derive(Serialize, Deserialize)]
struct Prompt {
    text: String,
    confirm: bool,
    notification: bool,
}

pub fn channel() -> (Progress, Updates) {
    let (stage, receiver) = watch::channel("准备连接".into());
    let (prompts, incoming) = mpsc::channel(4);
    let logs = Arc::new(Mutex::new(std::collections::VecDeque::new()));
    let waiting = Arc::new(AtomicUsize::new(0));
    (
        Progress {
            stage,
            prompts,
            logs: logs.clone(),
            waiting,
            secrets: Default::default(),
        },
        Updates {
            stage: receiver,
            logs,
            prompts: incoming,
        },
    )
}

/// Remove credential-bearing fields and answers before diagnostics reach any frontend.
pub fn filtered_log(line: &str, secrets: &[String]) -> String {
    let lower = line.to_ascii_lowercase();
    let compact: String = lower
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
    if [
        "password=",
        "password:",
        "passphrase=",
        "passphrase:",
        "token=",
        "api_key=",
        "api-key:",
        "authorization:",
        "secret=",
    ]
    .iter()
    .any(|key| compact.contains(key))
        || lower.contains("bearer ")
    {
        return "[含凭据的日志已隐藏]".into();
    }
    let mut filtered = line.to_owned();
    for secret in secrets
        .iter()
        .filter(|secret| !secret.is_empty())
    {
        filtered = filtered.replace(secret, "[已隐藏凭据]");
    }
    filtered
}

impl Progress {
    pub fn log(&self, text: &str) {
        let secrets = self
            .secrets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Ok(mut logs) = self.logs.lock() {
            for line in text.lines() {
                let line = filtered_log(line, &secrets);
                if !line.trim().is_empty() {
                    logs.push_back(line.chars().take(2_048).collect());
                }
            }
            while logs.len() > 200 {
                logs.pop_front();
            }
        }
    }

    fn record_secret(&self, secret: &str) {
        if secret.is_empty() {
            return;
        }
        let mut secrets = self
            .secrets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !secrets
            .iter()
            .any(|value| value == secret)
        {
            secrets.push(secret.to_owned());
        }
        if let Ok(mut logs) = self.logs.lock() {
            for line in logs.iter_mut() {
                *line = filtered_log(line, &secrets);
            }
        }
    }

    pub fn report(&self, text: impl Into<String>) {
        let text = text.into();
        self.log(&text);
        self.stage.send_replace(text);
    }
}

pub struct Control {
    #[cfg(unix)]
    directory: tempfile::TempDir,
}

impl Control {
    #[cfg(unix)]
    fn path(&self) -> PathBuf {
        self.directory.path().join("control")
    }
}

impl Drop for Control {
    /// The private master expires after 60 seconds even if cleanup cannot run.
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = std::process::Command::new("ssh")
            .arg("-S")
            .arg(self.path())
            .args(["-O", "exit", "--", "orbit-control"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|mut child| {
                std::thread::spawn(move || {
                    let _ = child.wait();
                })
            });
    }
}

pub struct Interaction {
    pub progress: Progress,
    pub control: Arc<Control>,
    directory: tempfile::TempDir,
    helper: PathBuf,
    _listener: AbortOnDropHandle<()>,
}

impl Interaction {
    pub async fn new(progress: Progress, control: Option<Arc<Control>>) -> Result<Self> {
        #[cfg(unix)]
        let directory = tempfile::Builder::new()
            .prefix("orbit-askpass-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")?;
        #[cfg(windows)]
        let directory = orbit_runtime::filesystem::temporary_directory("orbit-askpass-")?;
        let control = match control {
            Some(control) => control,
            None => Arc::new(Control {
                #[cfg(unix)]
                directory: tempfile::Builder::new()
                    .prefix("orbit-ssh-")
                    .tempdir_in("/tmp")?,
            }),
        };
        let socket = directory.path().join("prompt");
        let mut listener = ipc::Listener::bind(&socket)?;
        #[cfg(unix)]
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        #[cfg(windows)]
        let helper = std::env::current_exe()?;
        #[cfg(unix)]
        let helper = {
            let executable = std::env::current_exe()?;
            let wrapper = directory.path().join("askpass");
            std::fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\nexec '{}' --internal-ssh-askpass \"$@\"\n",
                    executable
                        .to_string_lossy()
                        .replace('\'', "'\\''")
                ),
            )?;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700))?;
            wrapper
        };
        let sender = progress.prompts.clone();
        let logs = progress.clone();
        let waiting = progress.waiting.clone();
        let task = tokio::spawn(async move {
            while let Ok(mut stream) = ipc::accept(&mut listener).await {
                let sender = sender.clone();
                let exchange = async {
                    let prompt: Prompt = protocol::read(&mut stream).await?;
                    ensure!(
                        prompt.text.len() <= MAX_PROMPT_BYTES,
                        "SSH prompt is too long"
                    );
                    if prompt.notification {
                        logs.log(&prompt.text);
                        protocol::write(&mut stream, &Some(String::new())).await?;
                        return Ok(());
                    }
                    let _waiting = Waiting::new(waiting.clone());
                    let (reply, answer) = oneshot::channel();
                    sender
                        .send(Challenge {
                            prompt: prompt.text,
                            confirm: prompt.confirm,
                            reply,
                        })
                        .await?;
                    let answer = answer.await.unwrap_or(None);
                    if !prompt.confirm
                        && let Some(secret) = &answer
                    {
                        logs.record_secret(secret);
                    }
                    protocol::write(&mut stream, &answer).await?;
                    Ok::<_, anyhow::Error>(())
                };
                let _ = tokio::time::timeout(RESPONSE_TIMEOUT, exchange).await;
            }
        });
        Ok(Self {
            progress,
            control,
            directory,
            helper,
            _listener: AbortOnDropHandle::new(task),
        })
    }

    pub fn configure(&self, command: &mut Command) {
        command
            .args(["-o", "BatchMode=no"])
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("SSH_ASKPASS", &self.helper)
            .env(SOCKET_ENV, self.directory.path().join("prompt"))
            .env(PARENT_ENV, std::process::id().to_string());
        #[cfg(unix)]
        command
            .args(["-o", "ControlMaster=auto", "-o", "ControlPersist=60"])
            .arg("-S")
            .arg(self.control.path());
        #[cfg(windows)]
        command.env(ASKPASS_ENV, "1");
    }
}

/// Identify the dedicated helper invocation before starting either client interface.
pub fn is_askpass() -> bool {
    std::env::args().nth(1).as_deref() == Some("--internal-ssh-askpass")
        || (cfg!(windows)
            && std::env::var_os(ASKPASS_ENV).is_some_and(|value| value == "1")
            && std::env::var_os(SOCKET_ENV).is_some())
}

pub fn askpass() -> Result<()> {
    let path = std::env::var_os(SOCKET_ENV).context("missing SSH prompt channel")?;
    let mut arguments = std::env::args().skip(1);
    let first = arguments.next().unwrap_or_default();
    let text = if first == "--internal-ssh-askpass" {
        arguments.next().unwrap_or_default()
    } else {
        first
    };
    ensure!(text.len() <= MAX_PROMPT_BYTES, "SSH prompt is too long");
    let hint = std::env::var("SSH_ASKPASS_PROMPT").unwrap_or_default();
    let prompt = Prompt {
        confirm: hint == "confirm" || text.contains("(yes/no"),
        notification: hint == "none",
        text,
    };
    let answer = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let exchange = async {
                let mut socket = ipc::Stream::connect(std::path::Path::new(&path)).await?;
                #[cfg(windows)]
                {
                    let expected: u32 = std::env::var(PARENT_ENV)
                        .context("missing SSH prompt owner")?
                        .parse()?;
                    ensure!(socket.peer_pid()? == expected, "SSH prompt owner changed");
                }
                let bytes = serde_json::to_vec(&prompt)?;
                tokio::time::timeout(Duration::from_secs(5), async {
                    socket
                        .write_all(&(bytes.len() as u32).to_be_bytes())
                        .await?;
                    socket.write_all(&bytes).await
                })
                .await??;
                let mut size = [0; 4];
                socket.read_exact(&mut size).await?;
                let size = u32::from_be_bytes(size) as usize;
                ensure!(size <= MAX_PROMPT_BYTES * 6, "SSH answer is too long");
                let mut bytes = vec![0; size];
                socket.read_exact(&mut bytes).await?;
                let answer: Option<String> = serde_json::from_slice(&bytes)?;
                answer.context("SSH authentication cancelled")
            };
            tokio::time::timeout(RESPONSE_TIMEOUT, exchange).await?
        })?;
    std::io::stdout().write_all(answer.as_bytes())?;
    std::io::stdout().write_all(b"\n")?;
    Ok(())
}

struct Waiting(Arc<AtomicUsize>);

impl Waiting {
    fn new(value: Arc<AtomicUsize>) -> Self {
        value.fetch_add(1, Ordering::Relaxed);
        Self(value)
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

pub async fn timeout<T>(
    duration: Duration,
    progress: Option<&Progress>,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::pin!(future);
    let mut remaining = duration;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut previous = std::time::Instant::now();
    loop {
        tokio::select! {
            result = &mut future => return result,
            _ = tick.tick() => {
                let now = std::time::Instant::now();
                if progress.is_none_or(|progress| progress.waiting.load(Ordering::Relaxed) == 0) {
                    remaining = remaining.saturating_sub(now.duration_since(previous));
                }
                previous = now;
                ensure!(
                    !remaining.is_zero(),
                    "连接阶段超时，请检查主机地址、端口和目标环境；可重试或修改配置"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn prompt(interaction: &Interaction, text: &str) -> ipc::Stream {
        let mut stream = ipc::Stream::connect(
            &interaction
                .directory
                .path()
                .join("prompt"),
        )
        .await
        .unwrap();
        protocol::write(
            &mut stream,
            &Prompt {
                text: text.into(),
                confirm: false,
                notification: false,
            },
        )
        .await
        .unwrap();
        stream
    }

    #[test]
    fn diagnostics_are_filtered_before_delivery_and_late_answers_scrub_retained_lines() {
        let (progress, updates) = channel();
        progress.log("Authorization: Bearer fixture-key");
        progress.log("child echoed sample-answer");
        progress.record_secret("sample-answer");
        progress.log("later echoed sample-answer");
        let lines = updates
            .logs
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!lines.contains("fixture-key"));
        assert!(!lines.contains("sample-answer"));
        assert!(lines.contains("已隐藏凭据"));
    }

    #[tokio::test]
    async fn authentication_channels_are_private_and_isolated_when_reusing_transport() {
        let (first, mut first_updates) = channel();
        let first = Interaction::new(first, None)
            .await
            .unwrap();
        let (second, mut second_updates) = channel();
        let second = Interaction::new(second, Some(first.control.clone()))
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&first.control, &second.control));
        assert_ne!(first.directory.path(), second.directory.path());
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(first.directory.path().join("prompt"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let mut one = prompt(&first, "first password").await;
        let mut two = prompt(&second, "second password").await;
        let first_prompt = first_updates
            .prompts
            .recv()
            .await
            .unwrap();
        let second_prompt = second_updates
            .prompts
            .recv()
            .await
            .unwrap();
        second_prompt
            .reply
            .send(Some("second-secret".into()))
            .unwrap();
        let answer: Option<String> = protocol::read(&mut two).await.unwrap();
        assert_eq!(answer.as_deref(), Some("second-secret"));
        first_prompt.reply.send(None).unwrap();
        let answer: Option<String> = protocol::read(&mut one).await.unwrap();
        assert!(answer.is_none());
        assert!(
            first_updates
                .logs
                .lock()
                .unwrap()
                .is_empty()
        );
        assert!(
            second_updates
                .logs
                .lock()
                .unwrap()
                .is_empty()
        );
        let mut command = Command::new("ssh");
        second.configure(&mut command);
        let debug = format!("{command:?}");
        assert!(!debug.contains("second-secret"));
        assert!(debug.contains("SSH_ASKPASS_REQUIRE") && debug.contains("force"));
        assert!(!debug.contains("BatchMode=yes"));
    }

    #[tokio::test]
    async fn connection_deadline_pauses_for_user_input_but_still_bounds_network_waits() {
        let (progress, _) = channel();
        let waiting = Waiting::new(progress.waiting.clone());
        timeout(Duration::from_millis(50), Some(&progress), async {
            tokio::time::sleep(Duration::from_millis(220)).await;
            drop(waiting);
            Ok(())
        })
        .await
        .unwrap();
        assert!(
            timeout(
                Duration::from_millis(20),
                Some(&progress),
                std::future::pending::<Result<()>>()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn ssh_notifications_do_not_request_a_password() {
        let (progress, mut updates) = channel();
        let interaction = Interaction::new(progress, None)
            .await
            .unwrap();
        let mut stream = ipc::Stream::connect(
            &interaction
                .directory
                .path()
                .join("prompt"),
        )
        .await
        .unwrap();
        protocol::write(
            &mut stream,
            &Prompt {
                text: "Touch your security key".into(),
                confirm: false,
                notification: true,
            },
        )
        .await
        .unwrap();
        let answer: Option<String> = protocol::read(&mut stream)
            .await
            .unwrap();
        assert_eq!(answer.as_deref(), Some(""));
        assert!(updates.prompts.try_recv().is_err());
        assert_eq!(
            updates
                .logs
                .lock()
                .unwrap()
                .back()
                .unwrap(),
            "Touch your security key"
        );
    }
    #[test]
    fn copied_diagnostics_hide_authentication_answers_and_credential_fields() {
        let secrets = vec!["test-only-password".to_owned()];
        assert_eq!(
            filtered_log("echo test-only-password", &secrets),
            "echo [已隐藏凭据]"
        );
        assert_eq!(
            filtered_log("Authorization: Bearer test-value", &[]),
            "[含凭据的日志已隐藏]"
        );
        assert_eq!(
            filtered_log("PASSWORD=test-value", &[]),
            "[含凭据的日志已隐藏]"
        );
        assert_eq!(
            filtered_log("password = test-value", &[]),
            "[含凭据的日志已隐藏]"
        );
        assert_eq!(
            filtered_log("Permission denied (publickey,password).", &secrets),
            "Permission denied (publickey,password)."
        );
        assert_eq!(filtered_log("已完成握手", &[String::new()]), "已完成握手");
    }
}
