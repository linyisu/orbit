use std::{
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use clap::{Subcommand, ValueEnum};
use orbit_auth::{AuthStore, CallbackFlow, CallbackListener, OAuthLogin, OAuthProvider};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum LoginProvider {
    Anthropic,
    Google,
    Antigravity,
    Chatgpt,
}

#[derive(Debug, Subcommand)]
pub(crate) enum AuthCommand {
    /// Sign in using the provider's browser OAuth flow (experimental).
    Login {
        #[arg(value_enum)]
        provider: LoginProvider,
        /// Credential name referenced by providers.<name>.credential.
        #[arg(long)]
        name: Option<String>,
        /// Explicit Google Cloud project for Workspace / paid Code Assist accounts.
        #[arg(long)]
        project: Option<String>,
        /// Paste the full redirect URL instead of listening on localhost.
        #[arg(long)]
        manual: bool,
        /// Print the login URL without launching a browser.
        #[arg(long)]
        no_browser: bool,
    },
    /// Save an API key, scoped to an explicit service base URL.
    SetKey {
        name: String,
        #[arg(long)]
        base_url: String,
        /// Read the secret from a pipe instead of a hidden terminal prompt.
        #[arg(long)]
        stdin: bool,
    },
    /// List credential metadata without exposing tokens.
    List,
    /// Refresh and list account models without sending a model inference request.
    Models {
        #[arg(long, default_value = "config.toml")]
        config: PathBuf,
    },
    /// Remove a local credential (does not revoke the remote grant).
    Logout { name: String },
}

pub(crate) fn directory(
    explicit: Option<PathBuf>,
    config_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(std::path::absolute(path)?);
    }
    let config = config_home
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| home.map(|home| home.join(".config")))
        .context("无法确定凭据目录，请设置 --auth-dir")?;
    if !config.is_absolute() {
        bail!("XDG_CONFIG_HOME 必须是绝对路径，或通过 --auth-dir 指定凭据目录");
    }

    Ok(config.join("orbit/auth"))
}

pub(crate) async fn run(command: AuthCommand, directory: PathBuf) -> Result<()> {
    let store = AuthStore::new(directory.clone())?;
    match command {
        AuthCommand::Models { config } => {
            let mut config = crate::config::FileConfig::load(&config, &directory)?;
            for diagnostic in crate::account_models::refresh(&mut config, true).await {
                eprintln!("{diagnostic}");
            }
            for model in config.model_options()? {
                println!("{}", model.qualified_name());
            }
        }
        AuthCommand::SetKey {
            name,
            base_url,
            stdin,
        } => {
            let key = read_secret(stdin)?;
            store.set_api_key(&name, key, &base_url).await?;
            println!("已保存凭据 {name}，配置中使用 credential = {name:?}");
        }
        AuthCommand::List => {
            for entry in store.list()? {
                println!(
                    "{}  {:?}  {}  {}",
                    entry.name,
                    entry.kind,
                    entry.endpoint,
                    entry.account.as_deref().unwrap_or("—")
                );
            }
        }
        AuthCommand::Logout { name } => {
            println!(
                "{}",
                if store.remove(&name).await? {
                    "已移除本地凭据"
                } else {
                    "未找到该凭据"
                }
            );
        }
        AuthCommand::Login {
            provider,
            name,
            project,
            manual,
            no_browser,
        } => {
            let provider = match provider {
                LoginProvider::Anthropic => OAuthProvider::Anthropic,
                LoginProvider::Antigravity => OAuthProvider::Antigravity,
                LoginProvider::Google => OAuthProvider::Google,
                LoginProvider::Chatgpt => OAuthProvider::Chatgpt,
            };
            let name = name.unwrap_or_else(|| provider.name().to_owned());
            let login = OAuthLogin::new(provider, project)?;
            let listener = if manual {
                None
            } else {
                Some(
                    CallbackListener::bind_at(login.redirect_uri().clone())
                        .await
                        .context("无法监听登录回调端口；可使用 --manual")?,
                )
            };
            let url = login.authorization_url();
            println!("在浏览器中完成 {} 登录：\n{url}", provider.name());
            if !no_browser {
                launch_browser(url.as_str()).await;
            }

            let callback = if let Some(listener) = listener {
                tokio::select! {
                    result = listener.wait(&login, Duration::from_secs(300)) => result?,
                    _ = tokio::signal::ctrl_c() => bail!("登录已取消"),
                }
            } else {
                // The callback contains a short-lived authorization code; do not echo it.
                eprintln!("粘贴浏览器最终跳转的完整 URL：");
                read_secret(!io::stdin().is_terminal())?
            };
            println!("正在交换登录凭据并检查账号配置…");
            tokio::select! {
                result = login.finish(&callback, &store, &name) => result?,
                _ = tokio::signal::ctrl_c() => bail!("登录已取消"),
            }
            println!("登录凭据已保存为 {name}。模型请求仍需通过服务端权限检查。");
        }
    }

    Ok(())
}

fn read_secret(from_stdin: bool) -> Result<String> {
    if from_stdin {
        if io::stdin().is_terminal() {
            bail!("--stdin 需要管道输入；交互输入请省略该选项");
        }
        let mut bytes = Vec::new();
        io::stdin().take(16_385).read_to_end(&mut bytes)?;
        if bytes.len() > 16_384 {
            bail!("输入过长");
        }
        return Ok(String::from_utf8(bytes)?.trim().to_owned());
    }
    if !io::stdin().is_terminal() {
        bail!("当前没有交互终端，请使用 --stdin");
    }

    eprint!("输入（隐藏；Enter 确认，Esc 取消）：");
    io::stderr().flush()?;
    crossterm::terminal::enable_raw_mode()?;
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = crossterm::terminal::disable_raw_mode();
            eprintln!();
        }
    }
    let _restore = Restore;
    let mut value = String::new();

    loop {
        use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
        if let Event::Key(key) = crossterm::event::read()? {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            match key.code {
                KeyCode::Enter => return Ok(value),
                KeyCode::Esc => bail!("输入已取消"),
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    bail!("输入已取消")
                }
                KeyCode::Backspace => {
                    value.pop();
                }
                KeyCode::Char(c)
                    if !c.is_control() && !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if value.len() + c.len_utf8() > 16_384 {
                        bail!("输入过长");
                    }
                    value.push(c);
                }
                _ => {}
            }
        }
    }
}

pub(crate) async fn launch_browser(url: &str) {
    #[cfg(target_os = "linux")]
    let command = "xdg-open";
    #[cfg(target_os = "macos")]
    let command = "open";
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = url;
        return;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::process::Command::new(command)
                .arg(url)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_home_controls_the_store_without_reading_host_environment() {
        let root = tempfile::tempdir().unwrap();
        let actual = directory(
            None,
            Some(root.path().join("config")),
            Some(root.path().join("home")),
        )
        .unwrap();

        assert_eq!(actual, root.path().join("config/orbit/auth"));
        assert!(directory(None, None, None).is_err());
        assert!(directory(None, Some("relative".into()), None).is_err());
    }
}
