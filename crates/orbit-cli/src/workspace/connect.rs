use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result, ensure};
use clap::Parser;

use super::{
    ConnectArgs,
    protocol::{Request, Response, SessionInfo},
    transport::{Client, Connection},
};

#[derive(Clone, Debug, Parser)]
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
            !words.is_empty(),
            "用法：/ssh HOST [PATH] 或 /wsl DISTRO [PATH]；可附加 --workspace、--resume、--config、--server"
        );
        let flag = match words[0].as_str() {
            "/ssh" => "--ssh",
            "/wsl" => "--wsl",
            _ => anyhow::bail!("未知工作区连接命令"),
        };
        let mut arguments = vec![
            "workspace".to_owned(),
            flag.into(),
            words.get(1).cloned().unwrap_or_default(),
        ];
        let mut rest = &words[words.len().min(2)..];
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

    pub async fn prepare(&self, progress: super::interaction::Progress) -> Result<Arc<Connection>> {
        super::prepare_connection(
            &self.args,
            self.config.clone(),
            self.database.clone(),
            self.auth_dir.clone(),
            Some(progress),
        )
        .await
    }

    pub fn remote(&self) -> bool {
        self.args.ssh.is_some() || self.args.wsl.is_some()
    }

    pub fn configure_required(&self) -> bool {
        self.args.workspace == "." && self.args.resume.is_none()
    }

    pub fn is_wsl(&self) -> bool {
        self.args.wsl.is_some()
    }

    pub fn target(&self) -> String {
        self.args
            .ssh
            .as_ref()
            .or(self.args.wsl.as_ref())
            .cloned()
            .unwrap_or_default()
    }

    pub fn fields(&self) -> [String; 5] {
        [
            self.target(),
            if self.is_wsl() {
                self.args.wsl_user.clone()
            } else {
                self.args.ssh_user.clone()
            }
            .unwrap_or_default(),
            self.args
                .ssh_port
                .map(|port| port.to_string())
                .unwrap_or_default(),
            self.args.identity.clone().unwrap_or_default(),
            if self.args.workspace == "." {
                String::new()
            } else {
                self.args.workspace.clone()
            },
        ]
    }

    pub fn update(&mut self, values: &[String; 5]) -> Result<()> {
        ensure!(!values[0].trim().is_empty(), "请输入主机或发行版");
        let optional = |value: &String| (!value.trim().is_empty()).then(|| value.trim().to_owned());
        if self.is_wsl() {
            self.args.wsl = Some(values[0].trim().into());
            self.args.wsl_user = optional(&values[1]);
        } else {
            self.args.ssh = Some(values[0].trim().into());
            self.args.ssh_user = optional(&values[1]);
            self.args.ssh_port = optional(&values[2])
                .map(|port| port.parse())
                .transpose()
                .context("端口须为 1–65535")?;
            ensure!(self.args.ssh_port != Some(0), "端口须为 1–65535");
            self.args.identity = optional(&values[3]);
        }
        self.args.workspace = optional(&values[4]).unwrap_or_else(|| ".".into());
        Ok(())
    }

    pub async fn open(self, progress: super::interaction::Progress) -> Result<Opened> {
        let connection = super::prepare_connection(
            &self.args,
            self.config,
            self.database,
            self.auth_dir,
            Some(progress.clone()),
        )
        .await?;
        Opened::connect_progress(connection, self.args.resume, progress).await
    }
}

impl Opened {
    pub async fn connect_progress(
        connection: Arc<Connection>,
        resume: Option<String>,
        progress: super::interaction::Progress,
    ) -> Result<Self> {
        progress.report("启动工作区服务并等待握手");
        let mut client = connection.connect_progress(Some(&progress)).await?;
        progress.report("已完成握手，正在打开工作区会话");
        let Response::Opened(info) = client.request(Request::Open { resume }).await? else {
            anyhow::bail!("unexpected workspace response");
        };
        let opened = Self {
            client,
            connection,
            info,
        };
        progress.report("已连接");
        Ok(opened)
    }

    pub async fn reopen(
        connection: Arc<Connection>,
        resume: Option<String>,
        progress: super::interaction::Progress,
    ) -> Result<Self> {
        let mut connection = (*connection).clone();
        if matches!(connection.target, super::transport::Target::Ssh { .. }) {
            let control = connection
                .interactive
                .as_ref()
                .map(|interactive| interactive.control.clone());
            connection.interactive = Some(Arc::new(
                super::interaction::Interaction::new(progress.clone(), control).await?,
            ));
        }
        Self::connect_progress(Arc::new(connection), resume, progress).await
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
            "/ssh host 'unclosed",
            "/ssh host --stop",
            "/wsl Ubuntu --prompt hello",
        ] {
            assert!(Spec::parse(command).is_err(), "{command}");
        }
    }
}
