use anyhow::{Context, Result, ensure};
use orbit_protocol::plugins::{Action, Catalog, Inspection};

pub(super) async fn execute(
    handle: &super::session::Handle,
    action: Action,
    expected_workspace: Option<&str>,
) -> Result<Catalog> {
    let info = handle.current_info().await;
    ensure!(
        expected_workspace.is_none_or(|expected| expected == info.workspace),
        "工作区已切换，请重新打开当前项目的设置后重试"
    );
    // Enabling a package and changing its source must not interleave between
    // the usage check and the atomic catalogue replacement.
    let _catalog_lock = if matches!(&action, Action::List) {
        None
    } else {
        Some(crate::marketplaces::lock_current()?)
    };
    // Capture the project once: another client moving this session later must
    // not redirect a plugin operation into its new workspace.
    let project_id = if matches!(&action, Action::Enable { .. }) {
        handle
            .store
            .resolve_project(&info.workspace)
            .await?
            .id
    } else {
        handle
            .store
            .project_at(&info.workspace)
            .await?
            .map(|project| project.id)
            .unwrap_or_default()
    };
    let project = &project_id;
    match action {
        Action::List => {}
        Action::Add { source } => {
            require_unused_source(&handle.store, source.trim()).await?;
            crate::marketplaces::add_current(source).await?;
        }
        Action::Refresh { source } => {
            require_unused_source(&handle.store, source.trim()).await?;
            crate::marketplaces::refresh_current(source).await?;
        }
        Action::Remove { source } => {
            require_unused_source(&handle.store, &source).await?;
            crate::marketplaces::remove_current(&source)?;
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
            handle
                .store
                .enable_project_plugin(plugin)
                .await?;
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
            market
                .plugins
                .iter()
                .map(|plugin| Inspection {
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
        enabled: handle
            .store
            .project_plugins(project)
            .await?,
        inspections,
    })
}

async fn require_unused_source(store: &orbit_store::store::Store, source: &str) -> Result<()> {
    ensure!(
        !store
            .marketplace_is_enabled(source)
            .await?,
        "该市场仍有插件被项目启用，请先在相关项目中停用插件，再刷新或移除市场"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn source_changes_are_blocked_until_all_projects_disable_their_plugins() {
        let directory = tempfile::tempdir().unwrap();
        let store = orbit_store::store::Store::open(directory.path().join("orbit.db"))
            .await
            .unwrap();
        let source = "owner/market";
        let mut projects = Vec::new();
        for name in ["first", "second"] {
            let root = directory.path().join(name);
            std::fs::create_dir(&root).unwrap();
            let project = store
                .resolve_project(&root)
                .await
                .unwrap();
            store
                .enable_project_plugin(orbit_store::types::ProjectPlugin {
                    project_id: project.id.clone(),
                    marketplace_source: source.into(),
                    plugin_name: "review".into(),
                    package_source: "plugins/review".into(),
                    version: Some("1.0.0".into()),
                    marketplace_revision: Some("a".repeat(40)),
                    enabled_at: 0,
                })
                .await
                .unwrap();
            projects.push(project);
        }

        assert!(
            require_unused_source(&store, "other/market")
                .await
                .is_ok()
        );
        assert!(
            require_unused_source(&store, source)
                .await
                .is_err()
        );
        store
            .disable_project_plugin(&projects[0].id, source, "review")
            .await
            .unwrap();
        assert!(
            require_unused_source(&store, source)
                .await
                .is_err()
        );
        store
            .disable_project_plugin(&projects[1].id, source, "review")
            .await
            .unwrap();
        assert!(
            require_unused_source(&store, source)
                .await
                .is_ok()
        );
    }
}
