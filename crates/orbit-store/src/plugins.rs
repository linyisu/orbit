//! Enabled project packages; installing and running them belongs to the service.
use crate::{
    database::now,
    entities::{project_plugins as plugins, projects},
    store::Store,
    types::ProjectPlugin,
};
use anyhow::{Context, Result, ensure};
use sea_orm::*;
impl Store {
    /// Whether any project in this database relies on a marketplace source.
    /// Removing or replacing its catalogue would invalidate those pinned packages.
    pub async fn marketplace_is_enabled(&self, source: &str) -> Result<bool> {
        self.read_access(|conn| async move {
            Ok(plugins::Entity::find()
                .filter(plugins::Column::MarketplaceSource.eq(source))
                .one(&conn)
                .await?
                .is_some())
        })
        .await
    }

    pub async fn project_plugins(&self, project_id: &str) -> Result<Vec<ProjectPlugin>> {
        self.read_access(|conn| async move {
            Ok(plugins::Entity::find()
                .filter(plugins::Column::ProjectId.eq(project_id))
                .order_by_desc(plugins::Column::EnabledAt)
                .order_by_asc(plugins::Column::PluginName)
                .order_by_asc(plugins::Column::MarketplaceSource)
                .all(&conn)
                .await?
                .into_iter()
                .map(plugin)
                .collect())
        })
        .await
    }
    pub async fn enable_project_plugin(&self, value: ProjectPlugin) -> Result<ProjectPlugin> {
        validate_project_plugin(&value)?;
        self.write_access(|conn| async move {
            let tx = conn.begin().await?;
            ensure!(
                projects::Entity::find_by_id(&value.project_id)
                    .one(&tx)
                    .await?
                    .is_some(),
                "project not found"
            );
            let id = (
                value.project_id.clone(),
                value.marketplace_source.clone(),
                value.plugin_name.clone(),
            );
            let row = plugins::ActiveModel {
                project_id: Set(value.project_id),
                marketplace_source: Set(value.marketplace_source),
                plugin_name: Set(value.plugin_name),
                package_source: Set(value.package_source),
                version: Set(value.version),
                marketplace_revision: Set(value.marketplace_revision),
                enabled_at: Set(now()),
            };
            plugins::Entity::insert(row)
                .on_conflict(
                    sea_query::OnConflict::columns([
                        plugins::Column::ProjectId,
                        plugins::Column::MarketplaceSource,
                        plugins::Column::PluginName,
                    ])
                    .update_columns([
                        plugins::Column::PackageSource,
                        plugins::Column::Version,
                        plugins::Column::MarketplaceRevision,
                        plugins::Column::EnabledAt,
                    ])
                    .to_owned(),
                )
                .exec(&tx)
                .await?;
            let saved = plugins::Entity::find_by_id(id)
                .one(&tx)
                .await?
                .context("plugin disappeared")?;
            tx.commit().await?;
            Ok(plugin(saved))
        })
        .await
    }
    pub async fn disable_project_plugin(
        &self,
        project_id: &str,
        marketplace_source: &str,
        plugin_name: &str,
    ) -> Result<bool> {
        validate_project_plugin_key(project_id, marketplace_source, plugin_name)?;
        self.write_access(|conn| async move {
            Ok(plugins::Entity::delete_by_id((
                project_id.to_owned(),
                marketplace_source.to_owned(),
                plugin_name.to_owned(),
            ))
            .exec(&conn)
            .await?
            .rows_affected
                == 1)
        })
        .await
    }
}
fn plugin(row: plugins::Model) -> ProjectPlugin {
    ProjectPlugin {
        project_id: row.project_id,
        marketplace_source: row.marketplace_source,
        plugin_name: row.plugin_name,
        package_source: row.package_source,
        version: row.version,
        marketplace_revision: row.marketplace_revision,
        enabled_at: row.enabled_at,
    }
}
fn validate_project_plugin(plugin: &ProjectPlugin) -> Result<()> {
    validate_project_plugin_key(
        &plugin.project_id,
        &plugin.marketplace_source,
        &plugin.plugin_name,
    )?;
    ensure!(
        !plugin.package_source.trim().is_empty(),
        "plugin package source must not be empty"
    );
    ensure!(
        plugin.package_source.len() <= 4096,
        "plugin package source exceeds 4096 bytes"
    );

    if let Some(version) = &plugin.version {
        ensure!(
            !version.trim().is_empty(),
            "plugin version must not be empty"
        );
        ensure!(version.len() <= 256, "plugin version exceeds 256 bytes");
    }

    if let Some(revision) = &plugin.marketplace_revision {
        ensure!(
            revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "marketplace revision must be a Git commit ID"
        );
    }

    Ok(())
}

fn validate_project_plugin_key(
    project_id: &str,
    marketplace_source: &str,
    plugin_name: &str,
) -> Result<()> {
    ensure!(
        !project_id.trim().is_empty(),
        "project id must not be empty"
    );
    ensure!(project_id.len() <= 256, "project id exceeds 256 bytes");
    ensure!(
        !marketplace_source.trim().is_empty(),
        "marketplace source must not be empty"
    );
    ensure!(
        marketplace_source.len() <= 4096,
        "marketplace source exceeds 4096 bytes"
    );
    ensure!(
        !plugin_name.trim().is_empty(),
        "plugin name must not be empty"
    );
    ensure!(plugin_name.len() <= 256, "plugin name exceeds 256 bytes");

    Ok(())
}
