use anyhow::{Context, Result, ensure};
use orbit_protocol::plugins::{Action, Catalog, Inspection};

pub(super) async fn execute(handle: &super::session::Handle, action: Action) -> Result<Catalog> {
    let session = handle.store.session(&handle.info.id).await?;
    let project = &session.project_id;
    match action {
        Action::List => {}
        Action::Add { source } => {
            crate::marketplaces::add_current(source).await?;
        }
        Action::Enable { mut plugin } => {
            let markets = crate::marketplaces::list_current()?;
            let market = markets
                .iter()
                .find(|market| market.source == plugin.marketplace_source)
                .context("插件市场已变更，请刷新后选择")?;
            let entry = market
                .plugins
                .iter()
                .find(|entry| entry.name == plugin.plugin_name)
                .context("插件已变更，请刷新后选择")?;
            let package = crate::marketplaces::inspect_plugin(market, entry)?;
            ensure!(
                plugin.marketplace_revision == market.revision
                    && entry.source.as_deref() == Some(&plugin.package_source)
                    && plugin.version == package.version,
                "插件版本已变更，请重新检查后启用"
            );
            plugin.project_id = project.clone();
            handle.store.enable_project_plugin(plugin).await?;
        }
        Action::Disable {
            marketplace_source,
            plugin_name,
        } => {
            handle
                .store
                .disable_project_plugin(project, &marketplace_source, &plugin_name)
                .await?;
        }
    }
    let marketplaces = crate::marketplaces::list_current()?;
    let inspections = marketplaces
        .iter()
        .flat_map(|market| {
            market.plugins.iter().map(|plugin| Inspection {
                source: market.source.clone(),
                name: plugin.name.clone(),
                package: crate::marketplaces::inspect_plugin(market, plugin)
                    .map_err(|error| error.to_string()),
            })
        })
        .collect();
    Ok(Catalog {
        marketplaces,
        project_id: project.clone(),
        enabled: handle.store.project_plugins(project).await?,
        inspections,
    })
}
