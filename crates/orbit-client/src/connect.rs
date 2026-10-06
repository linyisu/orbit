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
        let mut client = connection.connect_progress(Some(&progress)).await?;
        progress.report("已完成握手，正在打开工作区会话");
        progress.report("应用本地配置并打开工作区会话");
        let desired_model = resume
            .is_none()
            .then(|| {
                connection
                    .local_configuration
                    .as_ref()
                    .and_then(|local| local.model.clone())
            })
            .flatten();
        let configuration = connection.configuration().await?;
        let Response::Opened(mut info) = client
            .request(Request::Open {
                policy: Default::default(),
                resume,
                configuration,
            })
            .await?
        else {
            anyhow::bail!("unexpected workspace response");
        };
        if let Some(model) = desired_model.filter(|model| model != &info.model) {
            let response = client
                .request(Request::SetModel {
                    session: info.id.clone(),
                    model,
                    effort: None,
                })
                .await?;
            if !matches!(response, Response::Ok) {
                anyhow::bail!("model selection was not confirmed");
            }
            let Response::Opened(updated) = client
                .request(Request::Open {
                    policy: Default::default(),
                    resume: Some(info.id.clone()),
                    configuration: None,
                })
                .await?
            else {
                anyhow::bail!("workspace profile missing");
            };
            info = updated;
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
