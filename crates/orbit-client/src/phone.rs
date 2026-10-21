//! Local-only management of phone access through the existing workspace service.
use crate::{
    ConnectionOptions, prepare_connection,
    protocol::{Request, Response},
};
use anyhow::{Result, bail, ensure};
pub use orbit_protocol::remote::{HostAction, HostStatus, Permissions};
use std::path::PathBuf;

/// Manage the local gateway without opening a conversation or loading a model.
pub async fn request(
    options: ConnectionOptions,
    config: PathBuf,
    database: Option<PathBuf>,
    auth: Option<PathBuf>,
    action: HostAction,
) -> Result<HostStatus> {
    ensure!(
        options.ssh.is_none() && options.wsl.is_none(),
        "请在本机项目中管理手机连接"
    );
    let connection = prepare_connection(&options, config, database, auth, None).await?;
    request_connection(&connection, action).await
}

/// Manage phone access on an existing local workspace connection without creating a session.
pub async fn request_connection(
    connection: &crate::transport::Connection,
    action: HostAction,
) -> Result<HostStatus> {
    ensure!(
        matches!(connection.target, crate::transport::Target::Local),
        "请在本机项目中管理手机连接"
    );
    let mut client = connection.connect().await?;
    match client
        .raw_request(Request::RemoteHost { action })
        .await?
    {
        Response::RemoteHost(status) => Ok(status),
        Response::Error(error) => bail!("{error}"),
        _ => bail!("请更新电脑端服务后重试"),
    }
}
