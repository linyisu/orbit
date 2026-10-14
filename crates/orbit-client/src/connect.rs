use super::{
    protocol::{Request, Response, SessionInfo},
    transport::{Client, Connection},
};
use anyhow::Result;
use std::sync::Arc;

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
        let model = if resume.is_none() {
            let selected = match local {
                Some(local) => local.selected_model().await?,
                None => None,
            };
            Some(selected.ok_or_else(|| {
                anyhow::anyhow!("请选择模型；在 TUI 中选择，或使用 --model 连接名::模型名")
            })?)
        } else {
            None
        };
        let remember = resume.is_none();
        let configuration = connection.configuration().await?;
        let Response::Opened(info) = client
            .request(Request::Open {
                model,
                policy: Default::default(),
                resume,
                configuration,
            })
            .await?
        else {
            anyhow::bail!("unexpected workspace response");
        };
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
