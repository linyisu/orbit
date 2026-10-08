//! Aggregate payload sizes in SQLite, without materializing large history bodies.
use crate::{
    entities::*,
    store::{SessionStorageUsage, Store},
};
use anyhow::Result;
use sea_orm::sea_query::Expr;
use sea_orm::{
    sea_query::{Alias, Func, IntoColumnRef, SimpleExpr},
    *,
};

fn bytes(column: impl IntoColumnRef) -> SimpleExpr {
    Func::cust(Alias::new("length"))
        .arg(Expr::col(column).cast_as(Alias::new("BLOB")))
        .into()
}

impl Store {
    pub async fn session_storage_usage(&self) -> Result<Vec<SessionStorageUsage>> {
        self.read_access(|conn| async move {
            let tx = conn.begin().await?;
            let rows: Vec<(String, String, i64)> = sessions::Entity::find()
                .select_only()
                .column(sessions::Column::Id)
                .column(sessions::Column::Title)
                .column_as(bytes(sessions::Column::History), "bytes")
                .into_tuple()
                .all(&tx)
                .await?;
            let mut usage: std::collections::HashMap<String, SessionStorageUsage> = rows
                .into_iter()
                .map(|(id, title, size)| {
                    (
                        id.clone(),
                        SessionStorageUsage {
                            id,
                            title,
                            payload_bytes: size.max(0) as u64,
                        },
                    )
                })
                .collect();
            let mut add = |rows: Vec<(String, i64)>| {
                for (id, bytes) in rows {
                    if let Some(row) = usage.get_mut(&id) {
                        row.payload_bytes += bytes.max(0) as u64;
                    }
                }
            };
            macro_rules! direct {
                ($entity:ident, $payload:ident) => {
                    add($entity::Entity::find()
                        .select_only()
                        .column($entity::Column::SessionId)
                        .column_as(Expr::expr(bytes($entity::Column::$payload)).sum(), "bytes")
                        .group_by($entity::Column::SessionId)
                        .into_tuple()
                        .all(&tx)
                        .await?);
                };
            }
            direct!(runs, Prompt);
            direct!(session_messages, Payload);
            direct!(compaction_progress, Payload);
            direct!(session_memories, Content);
            direct!(session_git_baselines, Payload);
            direct!(session_workspace_baselines, Payload);
            direct!(side_questions, Payload);
            direct!(user_inputs, Payload);
            macro_rules! related {
                ($entity:ident, $parent:ident, $relation:ident) => {
                    add($entity::Entity::find()
                        .select_only()
                        .column($parent::Column::SessionId)
                        .column_as(
                            Expr::expr(bytes(($entity::Entity, $entity::Column::Payload))).sum(),
                            "bytes",
                        )
                        .join(JoinType::InnerJoin, $entity::Relation::$relation.def())
                        .group_by($parent::Column::SessionId)
                        .into_tuple()
                        .all(&tx)
                        .await?);
                };
            }
            related!(events, runs, Run);
            related!(execution_checkpoints, runs, Run);
            related!(context_archive_messages, context_archives, Archive);
            let mut rows = usage.into_values().collect::<Vec<_>>();
            rows.sort_by(|a, b| {
                b.payload_bytes
                    .cmp(&a.payload_bytes)
                    .then_with(|| a.id.cmp(&b.id))
            });
            tx.commit().await?;
            Ok(rows)
        })
        .await
    }
}
