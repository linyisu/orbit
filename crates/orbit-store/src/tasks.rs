//! Project task repository. Parent ownership and updates are transactional.
use crate::{
    database::now,
    entities::{project_tasks as tasks, projects},
    store::Store,
    types::{ProjectTask, TaskStatus},
};
use anyhow::{Context, Result, ensure};
use sea_orm::*;
impl Store {
    pub async fn tasks(&self, project_id: &str, include_closed: bool) -> Result<Vec<ProjectTask>> {
        self.read_access(|conn| async move {
            let mut query = tasks::Entity::find().filter(tasks::Column::ProjectId.eq(project_id));
            if !include_closed {
                query = query.filter(tasks::Column::Status.is_not_in(["completed", "cancelled"]));
            }
            let mut rows = query.all(&conn).await?;
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
            rows.into_iter().map(task).collect()
        })
        .await
    }
    pub async fn create_task(
        &self,
        project_id: &str,
        parent_id: Option<i64>,
        title: String,
        details: String,
        priority: i64,
    ) -> Result<ProjectTask> {
        validate_task_fields(&title, &details, priority)?;
        self.write_access(|conn| async move {
            let tx = conn.begin().await?;
            ensure!(
                projects::Entity::find_by_id(project_id)
                    .one(&tx)
                    .await?
                    .is_some(),
                "project not found"
            );
            if let Some(parent) = parent_id {
                ensure!(
                    tasks::Entity::find_by_id(parent)
                        .filter(tasks::Column::ProjectId.eq(project_id))
                        .one(&tx)
                        .await?
                        .is_some(),
                    "parent task does not belong to this project"
                );
            }
            let row = tasks::ActiveModel {
                project_id: Set(project_id.into()),
                parent_id: Set(parent_id),
                title: Set(title),
                details: Set(details),
                priority: Set(priority),
                ..Default::default()
            }
            .insert(&tx)
            .await?;
            let result = task(row)?;
            tx.commit().await?;
            Ok(result)
        })
        .await
    }
    pub async fn update_task(
        &self,
        project_id: &str,
        id: i64,
        title: Option<String>,
        details: Option<String>,
        status: Option<TaskStatus>,
        priority: Option<i64>,
    ) -> Result<ProjectTask> {
        if let Some(title) = &title {
            validate_task_fields(
                title,
                details.as_deref().unwrap_or(""),
                priority.unwrap_or(0),
            )?;
        }
        if let Some(details) = &details {
            ensure!(details.len() <= 8192, "task details exceed 8192 bytes");
        }
        if let Some(priority) = priority {
            ensure!(
                (-100..=100).contains(&priority),
                "task priority must be between -100 and 100"
            );
        }
        self.write_access(|conn| async move {
            let tx = conn.begin().await?;
            let mut row = tasks::Entity::find_by_id(id)
                .filter(tasks::Column::ProjectId.eq(project_id))
                .one(&tx)
                .await?
                .context("task not found in this project")?
                .into_active_model();
            if let Some(title) = title {
                row.title = Set(title);
            }
            if let Some(details) = details {
                row.details = Set(details);
            }
            if let Some(status) = status {
                row.status = Set(status.as_str().into());
            }
            if let Some(priority) = priority {
                row.priority = Set(priority);
            }
            row.updated_at = Set(now());
            let result = task(row.update(&tx).await?)?;
            tx.commit().await?;
            Ok(result)
        })
        .await
    }
}
fn task(row: tasks::Model) -> Result<ProjectTask> {
    Ok(ProjectTask {
        id: row.id,
        project_id: row.project_id,
        parent_id: row.parent_id,
        title: row.title,
        details: row.details,
        status: TaskStatus::parse(&row.status).context("invalid stored task status")?,
        priority: row.priority,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}
fn validate_task_fields(title: &str, details: &str, priority: i64) -> Result<()> {
    ensure!(!title.trim().is_empty(), "task title must not be empty");
    ensure!(title.len() <= 512, "task title exceeds 512 bytes");
    ensure!(details.len() <= 8192, "task details exceed 8192 bytes");
    ensure!(
        (-100..=100).contains(&priority),
        "task priority must be between -100 and 100"
    );

    Ok(())
}
