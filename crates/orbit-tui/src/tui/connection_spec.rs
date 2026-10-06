use crate::workspace::{connect::Opened, transport::Connection};
use anyhow::{Context, Result, ensure};
use clap::Parser;
use std::{path::PathBuf, sync::Arc};

#[derive(Clone, Debug, clap::Args)]
struct ConnectArgs {
    /// System SSH destination or a Host alias from ~/.ssh/config.
    #[arg(long, conflicts_with = "wsl")]
    ssh: Option<String>,
    #[arg(long, requires = "ssh")]
    ssh_user: Option<String>,
    #[arg(long, requires = "ssh")]
    ssh_port: Option<u16>,
    #[arg(long, requires = "ssh")]
    identity: Option<String>,
    /// WSL distribution. Omit both --ssh and --wsl for a local workspace.
    #[arg(long)]
    wsl: Option<String>,
    #[arg(long, requires = "wsl")]
    wsl_user: Option<String>,
    /// Directory on the target; ~/ is expanded by the target service.
    #[arg(long, default_value = ".")]
    workspace: String,
    /// Use an installed target executable instead of automatically deploying the server.
    #[arg(long)]
    server: Option<String>,
    /// Upload a Linux Orbit binary matching the target architecture.
    #[arg(long, conflicts_with = "server")]
    upload_server: Option<PathBuf>,
    #[arg(long)]
    resume: Option<String>,
    /// Run a prompt without entering the TUI. Pending approvals are denied.
    #[arg(long, conflicts_with_all = ["list", "stop"])]
    prompt: Option<String>,
    /// List this workspace's sessions without starting an agent.
    #[arg(long, conflicts_with = "stop")]
    list: bool,
    /// Stop an idle workspace service. Active tasks must be cancelled first.
    #[arg(long)]
    stop: bool,
    #[arg(long)]
    allow_mcp_start: bool,
}

#[derive(Clone, Debug, Parser)]
#[command(name = "workspace", disable_help_subcommand = true)]
pub(crate) struct Spec {
    #[command(flatten)]
    args: ConnectArgs,
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    database: Option<PathBuf>,
    #[arg(long)]
    auth_dir: Option<PathBuf>,
    #[arg(skip)]
    model: Option<String>,
}

impl Spec {
    pub fn inherit(
        &mut self,
        config: &std::path::Path,
        auth: &std::path::Path,
        model: Option<&str>,
    ) {
        self.config.get_or_insert_with(|| config.to_owned());
        self.auth_dir.get_or_insert_with(|| auth.to_owned());
        if self.model.is_none() {
            self.model = model.map(str::to_owned);
        }
    }

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

    pub async fn prepare(
        &self,
        progress: crate::workspace::interaction::Progress,
    ) -> Result<Arc<Connection>> {
        let mut connection = crate::workspace::prepare_connection(
            &self.args.options(),
            self.config.clone().unwrap_or_else(|| "config.toml".into()),
            self.database.clone(),
            self.auth_dir.clone(),
            Some(progress),
        )
        .await?;
        if let Some(configuration) = &mut Arc::make_mut(&mut connection).local_configuration {
            configuration.model = self.model.clone();
        }
        Ok(connection)
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

    pub async fn open(self, progress: crate::workspace::interaction::Progress) -> Result<Opened> {
        let connection = self.prepare(progress.clone()).await?;
        Opened::connect_progress(connection, self.args.resume, progress).await
    }
}

impl ConnectArgs {
    fn options(&self) -> orbit_app::workspace::ConnectionOptions {
        orbit_app::workspace::ConnectionOptions {
            ssh: self.ssh.clone(),
            ssh_user: self.ssh_user.clone(),
            ssh_port: self.ssh_port,
            identity: self.identity.clone(),
            wsl: self.wsl.clone(),
            wsl_user: self.wsl_user.clone(),
            workspace: self.workspace.clone(),
            server: self.server.clone(),
            upload_server: self.upload_server.clone(),
            allow_mcp_start: self.allow_mcp_start,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_commands_inherit_local_settings_without_overwriting_explicit_paths() {
        let mut spec = Spec::parse("/ssh dev").unwrap();
        spec.inherit(
            std::path::Path::new("/local/config.toml"),
            std::path::Path::new("/local/auth"),
            Some("provider::model"),
        );
        assert_eq!(
            spec.config.as_deref(),
            Some(std::path::Path::new("/local/config.toml"))
        );
        assert_eq!(
            spec.auth_dir.as_deref(),
            Some(std::path::Path::new("/local/auth"))
        );
        assert_eq!(spec.model.as_deref(), Some("provider::model"));
        let mut explicit =
            Spec::parse("/wsl Ubuntu --config /other/settings.toml --auth-dir /other/auth")
                .unwrap();
        explicit.inherit(
            std::path::Path::new("/local/config.toml"),
            std::path::Path::new("/local/auth"),
            None,
        );
        assert_eq!(
            explicit.config.as_deref(),
            Some(std::path::Path::new("/other/settings.toml"))
        );
        assert_eq!(
            explicit.auth_dir.as_deref(),
            Some(std::path::Path::new("/other/auth"))
        );
    }

    #[test]
    fn tui_commands_preserve_explicit_local_configuration_and_target_paths() {
        let spec = Spec::parse(
            "/ssh dev '/home/me/project with spaces' --resume saved --config custom.toml",
        )
        .unwrap();
        assert_eq!(spec.label(), "SSH dev");
        assert_eq!(spec.workspace(), "/home/me/project with spaces");
        assert_eq!(spec.args.resume.as_deref(), Some("saved"));
        assert_eq!(spec.config, Some(PathBuf::from("custom.toml")));
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
