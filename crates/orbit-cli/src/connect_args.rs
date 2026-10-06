use clap::Args;
use std::path::PathBuf;

#[derive(Clone, Debug, Args)]
pub struct ConnectArgs {
    /// System SSH destination or a Host alias from ~/.ssh/config.
    #[arg(long, conflicts_with = "wsl")]
    pub(crate) ssh: Option<String>,
    #[arg(long, requires = "ssh")]
    pub(crate) ssh_user: Option<String>,
    #[arg(long, requires = "ssh")]
    pub(crate) ssh_port: Option<u16>,
    #[arg(long, requires = "ssh")]
    pub(crate) identity: Option<String>,
    /// WSL distribution. Omit both --ssh and --wsl for a local workspace.
    #[arg(long)]
    pub(crate) wsl: Option<String>,
    #[arg(long, requires = "wsl")]
    pub(crate) wsl_user: Option<String>,
    /// Directory on the target; ~/ is expanded by the target service.
    #[arg(long, default_value = ".")]
    pub(crate) workspace: String,
    /// Use an installed target executable instead of automatically deploying the server.
    #[arg(long)]
    pub(crate) server: Option<String>,
    /// Upload a Linux Orbit binary matching the target architecture.
    #[arg(long, conflicts_with = "server")]
    pub(crate) upload_server: Option<PathBuf>,
    #[arg(long)]
    pub(crate) resume: Option<String>,
    /// Run a prompt without entering the TUI. Pending approvals are denied.
    #[arg(long, conflicts_with_all = ["list", "stop"])]
    pub(crate) prompt: Option<String>,
    /// List this workspace's sessions without starting an agent.
    #[arg(long, conflicts_with = "stop")]
    pub(crate) list: bool,
    /// Stop an idle workspace service. Active tasks must be cancelled first.
    #[arg(long)]
    pub(crate) stop: bool,
    #[arg(long)]
    pub(crate) allow_mcp_start: bool,
}

impl ConnectArgs {
    pub fn options(&self) -> orbit_client::ConnectionOptions {
        orbit_client::ConnectionOptions {
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
