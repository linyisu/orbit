//! Shared project tasks: dependencies, ownership and state changes commit together.
mod dependencies;
mod state;
use crate::{
    database::now,
    entities::{
        project_tasks as tasks, projects, task_claims as claims, task_dependencies as deps,
    },
    store::Store,
    types::{ProjectTask, TaskStatus},
};
use anyhow::{Context, Result, ensure};
use sea_orm::*;
pub(crate) use state::release_unfinished;

#[derive(Default)]
pub struct NewTask {
    pub parent_id: Option<i64>,
    pub title: String,
    pub details: String,
    pub priority: i64,
    pub depends_on: Vec<i64>,
}

impl NewTask {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            ..Default::default()
        }
    }
}

#[derive(Default)]
pub struct TaskChanges {
    pub title: Option<String>,
    pub details: Option<String>,
    pub status: Option<TaskStatus>,
    pub priority: Option<i64>,
    /// None leaves dependencies unchanged; Some([]) clears them.
    pub depends_on: Option<Vec<i64>>,
}

#[derive(Clone, Copy)]
pub enum TaskActor<'a> {
    User,
    Session(&'a str),
}

impl Store {
    pub async fn tasks(&self, project: &str, include_closed: bool) -> Result<Vec<ProjectTask>> {
        self.read_access(|conn| async move {
            let tx = conn.begin().await?;
            let mut query = tasks::Entity::find().filter(tasks::Column::ProjectId.eq(project));
            if !include_closed {
                query = query.filter(tasks::Column::Status.is_not_in(["completed", "cancelled"]));
            }
            let mut rows = query.all(&tx).await?;
            rows.sort_by_key(|row| {
                (
                    match row.status.as_str() {
                        "in_progress" => 0,
                        "blocked" => 1,
                        _ => 2,
                    },
                    std::cmp::Reverse(row.priority),
                    row.id,
                )
            });
            let result = state::hydrate(&tx, rows).await?;
            tx.commit().await?;
            Ok(result)
        })
        .await
    }

    pub async fn create_task(&self, project: &str, task: NewTask) -> Result<ProjectTask> {
        state::validate(&task.title, &task.details, task.priority)?;
        self.write_access(|conn| async move {
            let tx = conn.begin().await?;
            ensure!(
                projects::Entity::find_by_id(project)
                    .one(&tx)
                    .await?
                    .is_some(),
                "project not found"
            );
            if let Some(parent) = task.parent_id {
                state::row(&tx, project, parent).await?;
            }
            let row = tasks::ActiveModel {
                project_id: Set(project.into()),
                parent_id: Set(task.parent_id),
                title: Set(task.title),
                details: Set(task.details),
                priority: Set(task.priority),
                ..Default::default()
            }
            .insert(&tx)
            .await?;
            dependencies::replace(&tx, project, row.id, task.depends_on).await?;
            let result = state::one(&tx, project, row.id).await?;
            tx.commit().await?;
            Ok(result)
        })
        .await
    }

    pub async fn update_task(
        &self,
        project: &str,
        id: i64,
        changes: TaskChanges,
        actor: TaskActor<'_>,
    ) -> Result<ProjectTask> {
        ensure!(
            changes.title.is_some()
                || changes.details.is_some()
                || changes.status.is_some()
                || changes.priority.is_some()
                || changes.depends_on.is_some(),
            "task update must change a field"
        );
        self.write_access(|conn| async move {
            let tx = conn.begin().await?;
            let row = state::row(&tx, project, id).await?;
            let owner = state::authorize(&tx, project, id, actor).await?;
            let status = changes
                .status
                .unwrap_or(TaskStatus::parse(&row.status).context("invalid task status")?);
            let title = changes.title.unwrap_or(row.title);
            let details = changes.details.unwrap_or(row.details);
            let priority = changes.priority.unwrap_or(row.priority);
            state::validate(&title, &details, priority)?;
            if row.status == "completed" && status != TaskStatus::Completed {
                dependencies::can_reopen(&tx, id).await?;
            }
            if let Some(dependencies) = changes.depends_on {
                dependencies::replace(&tx, project, id, dependencies).await?;
            }
            if matches!(status, TaskStatus::InProgress | TaskStatus::Completed) {
                state::ready(&tx, id).await?;
                if let TaskActor::Session(session) = actor {
                    ensure!(
                        owner.as_deref() == Some(session),
                        "claim the task before starting or completing it"
                    );
                }
            }
            tasks::ActiveModel {
                id: Set(id),
                title: Set(title),
                details: Set(details),
                status: Set(status.as_str().into()),
                priority: Set(priority),
                updated_at: Set(now()),
                ..Default::default()
            }
            .update(&tx)
            .await?;
            let result = state::one(&tx, project, id).await?;
            tx.commit().await?;
            Ok(result)
        })
        .await
    }

    /// The per-database writer lease and primary key make competing claims
    /// atomic even when agents use independent Store connections/processes.
    pub async fn claim_task(&self, project: &str, id: i64, session: &str) -> Result<ProjectTask> {
        self.write_access(|conn| async move {
            let tx = conn.begin().await?;
            let row = state::row(&tx, project, id).await?;
            let owner = state::authorize(&tx, project, id, TaskActor::Session(session)).await?;
            ensure!(
                !matches!(row.status.as_str(), "completed" | "cancelled"),
                "closed task cannot be claimed"
            );
            state::ready(&tx, id).await?;
            if owner.is_none() {
                claims::ActiveModel {
                    task_id: Set(id),
                    session_id: Set(session.into()),
                }
                .insert(&tx)
                .await?;
            }
            tasks::ActiveModel {
                id: Set(id),
                status: Set("in_progress".into()),
                updated_at: Set(now()),
                ..Default::default()
            }
            .update(&tx)
            .await?;
            let result = state::one(&tx, project, id).await?;
            tx.commit().await?;
            Ok(result)
        })
        .await
    }

    pub async fn release_task(
        &self,
        project: &str,
        id: i64,
        actor: TaskActor<'_>,
    ) -> Result<ProjectTask> {
        self.write_access(|conn| async move {
            let tx = conn.begin().await?;
            let row = state::row(&tx, project, id).await?;
            state::authorize(&tx, project, id, actor).await?;
            claims::Entity::delete_by_id(id)
                .exec(&tx)
                .await?;
            if row.status == "in_progress" {
                tasks::ActiveModel {
                    id: Set(id),
                    status: Set("pending".into()),
                    updated_at: Set(now()),
                    ..Default::default()
                }
                .update(&tx)
                .await?;
            }
            let result = state::one(&tx, project, id).await?;
            tx.commit().await?;
            Ok(result)
        })
        .await
    }
}

#[cfg(test)]
mod tests;
