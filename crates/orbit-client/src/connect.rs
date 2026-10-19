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

/// Open stored history or a draft without preparing a model request.
/// Cached model choices enrich the view; configuration failures are deferred to execution.
pub async fn open_session(
    client: &mut Client,
    connection: &Connection,
    resume: Option<String>,
    preferred: Option<String>,
) -> Result<SessionInfo> {
    anyhow::ensure!(
        client
            .capabilities
            .as_ref()
            .is_none_or(|capabilities| capabilities
                .features
                .iter()
                .any(|feature| feature == orbit_protocol::MODEL_INDEPENDENT_SESSIONS)),
        "目标端 Orbit 尚不支持独立读取历史，请更新目标端服务后重试"
    );
    let response = client
        .request(Request::Browse { resume })
        .await?;
    let Response::Opened(mut info) = response else {
        anyhow::bail!("unexpected conversation response");
    };
    if info.phase == orbit_protocol::SessionPhase::Browsing
        && let Some(model) = preferred
    {
        client
            .raw_request(Request::SetModel {
                session: info.id.clone(),
                model: model.clone(),
                effort: None,
            })
            .await?;
        info.model = model;
    }
    let select_initial =
        info.phase == orbit_protocol::SessionPhase::Browsing && info.model.is_empty();
    enrich_browsing(&mut info, connection).await;
    if select_initial && !info.model.is_empty() {
        client
            .raw_request(Request::SetModel {
                session: info.id.clone(),
                model: info.model.clone(),
                effort: info.reasoning_effort.clone(),
            })
            .await?;
    }
    Ok(info)
}

pub(crate) async fn enrich_browsing(info: &mut SessionInfo, connection: &Connection) {
    if info.phase == orbit_protocol::SessionPhase::Browsing
        && let Some(local) = &connection.local_configuration
    {
        // Configuration errors belong to execution. Browsing stored data must remain available.
        if let Ok(models) = local.model_catalog().await {
            info.model_catalog = models
                .iter()
                .map(|model| orbit_protocol::ModelSummary {
                    provider: model.provider.clone(),
                    model: model.model.clone(),
                    supports_vision: model.supports_vision,
                    reasoning_effort: model
                        .config
                        .reasoning_effort
                        .map(|effort| effort.as_str().into()),
                    reasoning_efforts: model
                        .reasoning_efforts
                        .iter()
                        .map(|effort| effort.as_str().into())
                        .collect(),
                })
                .collect();
            info.models = models
                .iter()
                .map(|model| model.qualified_name())
                .collect();
        }
        if info.model.is_empty() {
            info.model = local
                .selected_model()
                .await
                .unwrap_or_default()
                .or_else(|| info.models.first().cloned())
                .unwrap_or_default();
        }
        if let Some(model) = info
            .model_catalog
            .iter()
            .find(|model| model.qualified_name() == info.model)
        {
            info.supports_vision = model.supports_vision;
            info.reasoning_efforts
                .clone_from(&model.reasoning_efforts);
            info.reasoning_effort = info
                .reasoning_effort
                .clone()
                .or_else(|| model.reasoning_effort.clone());
        }
    }
}

/// Resolve execution configuration only when a request is about to start a model run.
pub(crate) async fn prepare_session(
    client: &mut Client,
    connection: &Connection,
    session: String,
) -> Result<()> {
    let local = connection.local_configuration.as_ref();
    let model = match local {
        Some(local) => local.selected_model().await?,
        None => None,
    };
    match client
        .raw_request(Request::Prepare {
            session: session.clone(),
            model,
            configuration: connection.configuration().await?,
        })
        .await?
    {
        Response::Profile(_) => {
            client
                .raw_request(Request::EnableUserInput {
                    session: session.clone(),
                })
                .await?;
            client
                .raw_request(Request::Plan {
                    session,
                    action: orbit_protocol::plan::Action::Enable,
                })
                .await?;
            Ok(())
        }
        _ => anyhow::bail!("unexpected execution preparation response"),
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
        progress.report("读取会话");
        let mut client = connection
            .connect_progress(Some(&progress))
            .await?;
        let local = connection.local_configuration.as_ref();
        let remember = resume.is_none();
        let info = open_session(&mut client, &connection, resume, None).await?;
        if remember
            && !info.model.is_empty()
            && let Some(local) = local
            && let Err(error) = local.remember_model(&info.model)
        {
            progress.log(&format!("无法保存模型偏好：{error:#}"));
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
