//! Session-owned snapshots remain independent of mutable global catalogues.
use crate::{entities::session_extensions as packages, store::Store};
use anyhow::Result;
use orbit_protocol::library::Bundle;
use sea_orm::*;

impl Store {
    /// Replace one applied package atomically; the session must already be persisted.
    pub async fn apply_extension(&self, session: &str, bundle: &Bundle) -> Result<()> {
        let encoded = serde_json::to_string(bundle)?;
        self.write_access(|conn| async move {
            packages::Entity::insert(packages::ActiveModel {
                session_id: Set(session.to_owned()),
                key: Set(bundle.extension.key.clone()),
                bundle: Set(encoded),
            })
            .on_conflict(
                sea_query::OnConflict::columns([
                    packages::Column::SessionId,
                    packages::Column::Key,
                ])
                .update_column(packages::Column::Bundle)
                .to_owned(),
            )
            .exec(&conn)
            .await?;
            Ok(())
        })
        .await
    }

    pub async fn session_extensions(&self, session: &str) -> Result<Vec<Bundle>> {
        self.read_access(|conn| async move {
            packages::Entity::find()
                .filter(packages::Column::SessionId.eq(session))
                .order_by_asc(packages::Column::Key)
                .all(&conn)
                .await?
                .into_iter()
                .map(|row| serde_json::from_str(&row.bundle).map_err(Into::into))
                .collect()
        })
        .await
    }

    /// Remove availability for future turns; existing transcript content is retained.
    pub async fn remove_extension(&self, session: &str, key: &str) -> Result<()> {
        self.write_access(|conn| async move {
            packages::Entity::delete_by_id((session.to_owned(), key.to_owned()))
                .exec(&conn)
                .await?;
            Ok(())
        })
        .await
    }
}
