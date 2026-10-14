//! Append-only planning snapshots. Compare, create tasks and save under one transaction.
use crate::{
    entities::{project_tasks as tasks, session_plans as plans, sessions},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use orbit_protocol::plan::State;
use sea_orm::*;
impl Store {
    pub async fn planning(&self, session: &str) -> Result<State> {
        self.read_access(|conn| async move {
            Ok(plans::Entity::find()
                .filter(plans::Column::SessionId.eq(session))
                .order_by_desc(plans::Column::Generation)
                .one(&conn)
                .await?
                .map(|row| serde_json::from_str(&row.payload))
                .transpose()?
                .unwrap_or_default())
        })
        .await
    }

    pub async fn save_planning(
        &self,
        session: &str,
        expected: u64,
        state: &State,
    ) -> Result<State> {
        ensure!(
            Some(state.generation) == expected.checked_add(1),
            "invalid planning generation"
        );
        let mut state = state.clone();
        let generation = i64::try_from(state.generation)?;
        self.write_access(|conn| async move {
            let tx = conn.begin().await?;
            let current = plans::Entity::find()
                .filter(plans::Column::SessionId.eq(session))
                .order_by_desc(plans::Column::Generation)
                .one(&tx)
                .await?
                .map_or(0, |row| row.generation);
            ensure!(
                u64::try_from(current)? == expected,
                "计划已更新，请查看最新版本"
            );
            if let Some(plan) = &mut state.plan
                && plan.status == orbit_protocol::plan::Status::Approved
                && plan.task_ids.is_empty()
            {
                let project = sessions::Entity::find_by_id(session)
                    .one(&tx)
                    .await?
                    .context("session not found")?
                    .project_id
                    .context("session has no project")?;
                for (i, step) in plan.steps.iter().enumerate() {
                    let task = tasks::ActiveModel {
                        project_id: Set(project.clone()),
                        title: Set(step.chars().take(512).collect()),
                        details: Set(format!(
                            "Plan {} revision {} step {}\n{}",
                            plan.id,
                            plan.revision,
                            i + 1,
                            step
                        )),
                        ..Default::default()
                    }
                    .insert(&tx)
                    .await?;
                    plan.task_ids.push(task.id);
                }
            }
            plans::ActiveModel {
                session_id: Set(session.into()),
                generation: Set(generation),
                payload: Set(serde_json::to_string(&state)?),
            }
            .insert(&tx)
            .await?;
            tx.commit().await?;
            Ok(state)
        })
        .await
    }
}
