//! Pending-input reads and writer-owned compare-and-swap mutations.
//! A live caller must also reserve the matching AgentInbox boundary; the
//! durable received flag alone cannot identify a message already drained.
use anyhow::{Context, Result, ensure};
use orbit_agent::inbox::InboxMessage;
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::{entities::*, store::Store};

// Match session loading: a project's current path takes precedence over the
// legacy path stored on its sessions. Resolve it inside the queue transaction
// so the same workspace guard protects both reads and mutations.
fn queue_workspace(session: &str) -> Select<sessions::Entity> {
    sessions::Entity::find_by_id(session)
        .select_only()
        .column_as(
            Expr::expr(sea_query::Func::coalesce([
                Expr::col((projects::Entity, projects::Column::Path)).into(),
                Expr::col((sessions::Entity, sessions::Column::Workspace)).into(),
            ])),
            "workspace",
        )
        .join(JoinType::LeftJoin, sessions::Relation::Project.def())
}

#[derive(Debug)]
pub struct PendingInputQueue {
    pub workspace: String,
    pub run: Option<i64>,
    pub messages: Vec<InboxMessage>,
}

impl Store {
    pub async fn input_queue(&self, session: &str) -> Result<PendingInputQueue> {
        let session = session.to_owned();
        self.read_access(move |conn| async move {
            let tx = conn.begin().await?;
            let workspace = queue_workspace(&session)
                .into_tuple::<String>()
                .one(&tx)
                .await?
                .context("session not found")?;
            let run = runs::Entity::find()
                .select_only()
                .column(runs::Column::Id)
                .filter(runs::Column::SessionId.eq(&session))
                .filter(runs::Column::Status.eq("running"))
                .order_by_desc(runs::Column::Id)
                .into_tuple::<i64>()
                .one(&tx)
                .await?;
            let messages = agent_messages::Entity::find()
                .filter(agent_messages::Column::SessionId.eq(&session))
                .filter(agent_messages::Column::Received.eq(false))
                .order_by_asc(agent_messages::Column::Id)
                .all(&tx)
                .await?
                .into_iter()
                .map(|row| -> Result<InboxMessage> {
                    Ok(InboxMessage {
                        attachments: serde_json::from_str(&row.attachments)?,
                        id: row.id,
                        content: row.content,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            tx.commit().await?;
            Ok(PendingInputQueue {
                workspace,
                run,
                messages,
            })
        })
        .await
    }
}

impl Store {
    // Keep mutation private to the store crate: service callers must obtain a
    // SessionWriter and reserve the live inbox, rather than edit SQLite alone.
    pub(crate) async fn change_queued_input(
        &self,
        session: &str,
        expected_workspace: &str,
        id: i64,
        expected: &str,
        content: Option<String>,
    ) -> Result<()> {
        ensure!(
            id > 0 && expected.len() <= 1024 * 1024,
            "invalid queued input identity"
        );
        if let Some(content) = &content {
            ensure!(
                !content.trim().is_empty() && content.len() <= 1024 * 1024,
                "message must contain 1..1048576 bytes"
            );
        }
        let session = session.to_owned();
        let expected_workspace = expected_workspace.to_owned();
        let expected = expected.to_owned();
        self.write_access(move |conn| async move {
            let tx = conn.begin().await?;
            let workspace = queue_workspace(&session)
                .into_tuple::<String>()
                .one(&tx)
                .await?
                .context("session not found")?;
            ensure!(
                workspace == expected_workspace,
                "工作区已切换，请刷新输入队列"
            );
            let filter = Condition::all()
                .add(agent_messages::Column::Id.eq(id))
                .add(agent_messages::Column::SessionId.eq(session))
                .add(agent_messages::Column::Received.eq(false))
                .add(agent_messages::Column::Content.eq(expected));
            let affected = if let Some(content) = content {
                agent_messages::Entity::update_many()
                    .col_expr(
                        agent_messages::Column::Content,
                        sea_query::Expr::value(content),
                    )
                    .filter(filter)
                    .exec(&tx)
                    .await?
                    .rows_affected
            } else {
                agent_messages::Entity::delete_many()
                    .filter(filter)
                    .exec(&tx)
                    .await?
                    .rows_affected
            };
            ensure!(affected == 1, "输入已被接收、撤销或修改，请刷新队列");
            tx.commit().await?;
            Ok(())
        })
        .await
    }
}

#[cfg(test)]
mod tests;
