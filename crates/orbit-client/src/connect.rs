mod recovery;
pub use recovery::Recovery;

use super::{
    protocol::{Request, Response, SessionInfo},
    transport::{Client, Connection},
};
use anyhow::Result;
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RemoteKind {
    Ssh,
    Wsl,
}

pub struct RemoteDraft {
    pub mode: RemoteKind,
    pub target: String,
    pub workspace: String,
    pub user: String,
    pub port: String,
    pub identity: String,
    pub server: String,
    pub database: String,
}

impl RemoteDraft {
    pub fn options(
        &self,
    ) -> anyhow::Result<(super::ConnectionOptions, Option<std::path::PathBuf>)> {
        let target = self.target.trim();
        anyhow::ensure!(
            !target.is_empty(),
            match self.mode {
                RemoteKind::Ssh => "填写 SSH 主机或配置别名",
                RemoteKind::Wsl => "填写 WSL 发行版名称",
            }
        );
        anyhow::ensure!(
            !target.starts_with('-') && !target.chars().any(char::is_control),
            "连接目标不能以横线开头或包含控制字符"
        );
        if self.mode == RemoteKind::Ssh {
            anyhow::ensure!(
                !target.chars().any(char::is_whitespace) && !target.contains("://"),
                "SSH 主机使用 Host 别名或 user@hostname，不包含空格或 URL 前缀"
            );
        }
        let path = self.workspace.trim();
        anyhow::ensure!(!path.is_empty(), "填写目标机器上的工作区目录");
        anyhow::ensure!(
            !path.chars().any(char::is_control),
            "工作区目录包含无效字符"
        );
        let user = optional(&self.user);
        if let Some(user) = &user {
            anyhow::ensure!(
                !user.starts_with('-')
                    && !user
                        .chars()
                        .any(|c| c.is_whitespace() || c.is_control()),
                "用户名不能以横线开头或包含空格"
            );
        }
        let port = if self.mode == RemoteKind::Ssh {
            optional(&self.port)
                .map(|port| {
                    port.parse::<u16>()
                        .ok()
                        .filter(|port| *port != 0)
                        .ok_or_else(|| anyhow::anyhow!("SSH 端口必须为 1–65535"))
                })
                .transpose()?
        } else {
            None
        };
        for (name, value) in [
            ("服务程序", &self.server),
            ("数据库路径", &self.database),
            ("密钥路径", &self.identity),
        ] {
            anyhow::ensure!(!value.chars().any(char::is_control), "{name}包含无效字符");
        }
        anyhow::ensure!(
            !matches!(optional(&self.server).as_deref(), Some("~"))
                && !self.server.trim().starts_with("~/"),
            "服务程序使用目标机器上的绝对路径，或 PATH 中的命令名"
        );
        let mut options = super::ConnectionOptions::local(path.into());
        options.set_target(match self.mode {
            RemoteKind::Ssh => super::transport::Target::Ssh {
                host: target.into(),
                options: super::transport::SshOptions {
                    user,
                    port,
                    identity: optional(&self.identity),
                },
            },
            RemoteKind::Wsl => super::transport::Target::Wsl {
                distribution: target.into(),
                user,
            },
        })?;

        options.server = optional(&self.server);
        let database = optional(&self.database).map(std::path::PathBuf::from);
        Ok((options, database))
    }
}

fn optional(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Open a fresh or restored session using the same model fallback and configuration contract.
/// Saved sessions keep their historical model; local selection is only a fallback.
pub async fn open_session(
    client: &mut Client,
    connection: &Connection,
    resume: Option<String>,
    preferred: Option<String>,
) -> Result<SessionInfo> {
    let local = connection.local_configuration.as_ref();
    let model = if resume.is_some() {
        match local {
            Some(local) => local
                .selected_model()
                .await
                .unwrap_or_default()
                .or(preferred),
            None => preferred,
        }
    } else if let Some(model) = preferred {
        Some(model)
    } else {
        match local {
            Some(local) => match local.selected_model().await? {
                Some(model) => Some(model),
                None => local
                    .model_catalog()
                    .await?
                    .first()
                    .map(|model| model.qualified_name()),
            },
            None => None,
        }
    };
    match client
        .request(Request::Open {
            model,
            policy: Default::default(),
            resume,
            configuration: connection.configuration().await?,
        })
        .await?
    {
        Response::Opened(info) => Ok(info),
        _ => anyhow::bail!("unexpected workspace response"),
    }
}

pub struct Opened {
    pub client: Client,
    pub connection: Arc<Connection>,
    pub info: SessionInfo,
}

impl Opened {
    pub async fn connect_progress(
        connection: Arc<Connection>,
        resume: Option<String>,
        progress: super::interaction::Progress,
    ) -> Result<Self> {
        progress.report("启动工作区服务并等待握手");
        let mut client = connection
            .connect_progress(Some(&progress))
            .await?;
        progress.report("已完成握手，正在打开工作区会话");
        progress.report("应用本地配置并打开工作区会话");
        let local = connection.local_configuration.as_ref();
        let remember = resume.is_none();
        let info = open_session(&mut client, &connection, resume, None).await?;
        if remember && let Some(local) = local {
            local.remember_model(&info.model)?;
        }
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
