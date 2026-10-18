//! Global library access and explicit session application shared by GUI and TUI.
use crate::protocol::Response;
use crate::{
    ConnectionOptions,
    transport::{Client, Connection, Target},
};
use anyhow::{Context, Result, bail};
use orbit_protocol::{
    Request,
    library::{self, Action, ExtensionKind, Snapshot},
    plugins, resources,
};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub struct Library {
    config: PathBuf,
    auth: Option<PathBuf>,
    server: Option<String>,
}

impl Library {
    /// The library always belongs to this client host, regardless of the active workspace.
    pub fn new(config: PathBuf, auth: Option<PathBuf>, local_server: Option<String>) -> Self {
        Self {
            config,
            auth,
            server: local_server,
        }
    }

    /// Ignore remote executable overrides when selecting the client-host library service.
    pub fn for_options(
        options: &ConnectionOptions,
        config: PathBuf,
        auth: Option<PathBuf>,
    ) -> Self {
        let server = if options.ssh.is_none() && options.wsl.is_none() {
            options.server.clone()
        } else {
            None
        };
        Self::new(config, auth, server)
    }

    fn from_connection(connection: &Connection) -> Self {
        let config = connection
            .local_configuration
            .as_ref()
            .map_or(&connection.config, |local| &local.config);
        let auth = connection
            .local_configuration
            .as_ref()
            .map(|local| local.auth.clone())
            .or_else(|| connection.auth_dir.clone());
        Self::new(
            config.clone(),
            auth,
            matches!(connection.target, Target::Local).then(|| connection.executable.clone()),
        )
    }

    /// Connect without opening a conversation or selecting a model.
    pub async fn request(&self, action: Action) -> Result<Snapshot> {
        let config = std::path::absolute(&self.config)?;
        let directory = config
            .parent()
            .filter(|directory| directory.is_dir())
            .map(PathBuf::from)
            .or_else(orbit_config::directories::home)
            .context("无法确定本机全局服务的启动目录")?;
        let mut options = ConnectionOptions::local(directory.to_string_lossy().into_owned());
        options.server = self.server.clone();
        let connection =
            crate::prepare_connection(&options, self.config.clone(), None, self.auth.clone(), None)
                .await?;
        let mut client = connection.connect().await?;
        match client
            .raw_request(Request::Library { action })
            .await?
        {
            Response::Library(snapshot) => Ok(snapshot),
            _ => bail!("服务未返回全局库数据，请更新本机服务"),
        }
    }

    pub async fn skills(&self, action: resources::Action) -> Result<Response> {
        match self
            .request(Action::Skills(action))
            .await?
        {
            Snapshot::Skills(snapshot) => Ok(Response::Resources(snapshot)),
            _ => bail!("服务未返回技能库数据"),
        }
    }

    pub async fn plugins(&self, action: plugins::Action) -> Result<Response> {
        match self
            .request(Action::Plugins(action))
            .await?
        {
            Snapshot::Plugins(catalog) => Ok(Response::Plugins(catalog)),
            _ => bail!("服务未返回全局插件市场"),
        }
    }
}

pub(crate) fn routed(request: &Request) -> bool {
    matches!(
        request,
        Request::Plugins { .. }
            | Request::SessionAction {
                action: orbit_protocol::SessionAction::LoadSkill { .. },
                ..
            }
            | Request::Resources {
                action: resources::Action::Skills
                    | resources::Action::ReadSkill { .. }
                    | resources::Action::SaveSkill { .. }
                    | resources::Action::ScanSkills { .. }
                    | resources::Action::ImportSkills { .. },
                ..
            }
    )
}

async fn export(library: &Library, action: Action) -> Result<library::Bundle> {
    match library.request(action).await? {
        Snapshot::Bundle(bundle) => Ok(bundle),
        _ => bail!("服务未返回扩展快照"),
    }
}

/// Never replay mutations after an uncertain transport result.
pub(crate) async fn route(
    client: &mut Client,
    request: Request,
    connection: Arc<Connection>,
) -> Result<Response> {
    let library = Library::from_connection(&connection);
    match request {
        Request::Resources { action, .. } => library.skills(action).await,
        Request::SessionAction {
            session,
            action: orbit_protocol::SessionAction::LoadSkill { name },
        } => {
            let bundle = export(&library, Action::ExportSkill { name: name.clone() }).await?;
            client
                .raw_request(Request::Resources {
                    session,
                    expected_workspace: None,
                    action: resources::Action::ApplyExtension {
                        bundle: Box::new(bundle),
                    },
                })
                .await?;
            Ok(Response::Report(orbit_protocol::Report {
                title: "技能".into(),
                text: format!("已将 /{name} 应用到当前会话。"),
            }))
        }
        Request::Plugins {
            session,
            expected_workspace: _,
            action,
        } => {
            let Response::Plugins(mut catalog) = library
                .plugins(plugins::Action::List)
                .await?
            else {
                unreachable!()
            };
            let response = match action {
                plugins::Action::Enable { plugin } => {
                    let bundle = export(&library, Action::ExportPlugin { plugin }).await?;
                    client
                        .raw_request(Request::Resources {
                            session: session.clone(),
                            expected_workspace: None,
                            action: resources::Action::ApplyExtension {
                                bundle: Box::new(bundle),
                            },
                        })
                        .await?
                }
                plugins::Action::Disable {
                    marketplace_source,
                    plugin_name,
                } => {
                    client
                        .raw_request(Request::Resources {
                            session: session.clone(),
                            expected_workspace: None,
                            action: resources::Action::RemoveExtension {
                                key: format!("plugin:{marketplace_source}:{plugin_name}"),
                            },
                        })
                        .await?
                }
                action => {
                    let Response::Plugins(updated) = library.plugins(action).await? else {
                        unreachable!()
                    };
                    catalog = updated;
                    client
                        .raw_request(Request::Resources {
                            session: session.clone(),
                            expected_workspace: None,
                            action: resources::Action::AppliedExtensions,
                        })
                        .await?
                }
            };
            let Response::Resources(resources::Snapshot::Extensions(applied)) = response else {
                bail!("服务未返回会话扩展状态");
            };
            catalog.session_id = session;
            catalog.applied = applied
                .into_iter()
                .filter_map(|extension| {
                    if let ExtensionKind::Plugin(plugin) = extension.kind {
                        Some(plugin)
                    } else {
                        None
                    }
                })
                .collect();
            Ok(Response::Plugins(catalog))
        }
        _ => bail!("此请求不属于全局库"),
    }
}
