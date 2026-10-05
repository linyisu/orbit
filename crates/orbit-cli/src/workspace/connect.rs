use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result, ensure};
use clap::Parser;

use super::{
    ConnectArgs,
    protocol::{Request, Response, SessionInfo},
    transport::{Client, Connection},
};

#[derive(Clone, Parser)]
#[command(name = "workspace", disable_help_subcommand = true)]
pub(crate) struct Spec {
    #[command(flatten)]
    args: ConnectArgs,
    #[arg(long, default_value = "config.toml")]
    config: PathBuf,
    #[arg(long)]
    database: Option<PathBuf>,
    #[arg(long)]
    auth_dir: Option<PathBuf>,
}

pub(crate) struct Opened {
    pub client: Client,
    pub connection: Arc<Connection>,
    pub info: SessionInfo,
}

impl Spec {
    pub fn is_command(prompt: &str) -> bool {
        matches!(prompt.split_whitespace().next(), Some("/ssh" | "/wsl"))
    }

    pub fn parse(prompt: &str) -> Result<Self> {
        let words = shlex::split(prompt).context("连接命令的引号未闭合")?;
        ensure!(
            words.len() >= 2,
            "用法：/ssh HOST [PATH] 或 /wsl DISTRO [PATH]；可附加 --workspace、--resume、--config、--server"
        );
        let flag = match words[0].as_str() {
            "/ssh" => "--ssh",
            "/wsl" => "--wsl",
            _ => anyhow::bail!("未知工作区连接命令"),
        };
        let mut arguments = vec!["workspace".to_owned(), flag.into(), words[1].clone()];
        let mut rest = &words[2..];
        if rest.first().is_some_and(|word| !word.starts_with('-')) {
            arguments.extend(["--workspace".into(), rest[0].clone()]);
            rest = &rest[1..];
        }
        arguments.extend_from_slice(rest);
        let spec = Self::try_parse_from(arguments)?;
        ensure!(
            !spec.args.list && !spec.args.stop && spec.args.prompt.is_none(),
            "TUI 连接不接受 --list、--stop 或 --prompt；连接后用 /sessions 查看会话"
        );
        Ok(spec)
    }

    pub fn local() -> Self {
        Self::parse_from(["workspace"])
    }

    pub fn label(&self) -> String {
        if let Some(host) = &self.args.ssh {
            format!("SSH {host}")
        } else if let Some(distribution) = &self.args.wsl {
            format!("WSL {distribution}")
        } else {
            "Local".into()
        }
    }

    pub fn workspace(&self) -> &str {
        &self.args.workspace
    }

    pub fn with_resume(&self, resume: Option<String>) -> Self {
        let mut spec = self.clone();
        spec.args.resume = resume;
        spec
    }

    pub async fn open(self) -> Result<Opened> {
        let connection =
            super::prepare_connection(&self.args, self.config, self.database, self.auth_dir, true)
                .await?;
        Opened::connect(connection, self.args.resume).await
    }
}

impl Opened {
    pub async fn connect(connection: Arc<Connection>, resume: Option<String>) -> Result<Self> {
        let mut client = connection.connect().await?;
        let Response::Opened(info) = client.request(Request::Open { resume }).await? else {
            anyhow::bail!("unexpected workspace response");
        };
        Ok(Self {
            client,
            connection,
            info,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tui_commands_preserve_target_paths_and_do_not_inherit_local_credentials() {
        let spec = Spec::parse(
            "/ssh dev '/home/me/project with spaces' --resume saved --config custom.toml",
        )
        .unwrap();
        assert_eq!(spec.label(), "SSH dev");
        assert_eq!(spec.workspace(), "/home/me/project with spaces");
        assert_eq!(spec.args.resume.as_deref(), Some("saved"));
        assert_eq!(spec.config, PathBuf::from("custom.toml"));
        assert!(spec.auth_dir.is_none() && spec.database.is_none());
        let spec = Spec::parse("/wsl Ubuntu --workspace ~/project --wsl-user dev").unwrap();
        assert_eq!(spec.args.wsl_user.as_deref(), Some("dev"));
        assert_eq!(spec.workspace(), "~/project");
        for command in [
            "/ssh",
            "/ssh host 'unclosed",
            "/ssh host --stop",
            "/wsl Ubuntu --prompt hello",
        ] {
            assert!(Spec::parse(command).is_err(), "{command}");
        }
    }
}
