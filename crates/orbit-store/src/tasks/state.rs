use super::*;
use crate::entities::sessions;
use std::collections::{BTreeMap, BTreeSet};

pub(super) async fn session_in_project(
    conn: &impl ConnectionTrait,
    project: &str,
    session: &str,
) -> Result<()> {
    ensure!(
        sessions::Entity::find_by_id(session)
            .filter(sessions::Column::ProjectId.eq(project))
            .one(conn)
            .await?
            .is_some(),
        "task owner does not belong to this project"
    );
    Ok(())
}

pub(super) async fn row(
    conn: &impl ConnectionTrait,
    project: &str,
    id: i64,
) -> Result<tasks::Model> {
    tasks::Entity::find_by_id(id)
        .filter(tasks::Column::ProjectId.eq(project))
        .one(conn)
        .await?
        .context("task not found in this project")
}

pub(super) async fn authorize(
    conn: &impl ConnectionTrait,
    project: &str,
    id: i64,
    actor: TaskActor<'_>,
) -> Result<Option<String>> {
    let owner = claims::Entity::find_by_id(id)
        .one(conn)
        .await?
        .map(|r| r.session_id);
    if let TaskActor::Session(session) = actor {
        session_in_project(conn, project, session).await?;
        ensure!(
            owner
                .as_deref()
                .is_none_or(|owner| owner == session),
            "task is claimed by another session; only its owner or the user may update/release it"
        );
    }
    Ok(owner)
}

pub(super) async fn ready(conn: &impl ConnectionTrait, id: i64) -> Result<()> {
    let ids: Vec<_> = deps::Entity::find()
        .filter(deps::Column::TaskId.eq(id))
        .all(conn)
        .await?
        .into_iter()
        .map(|r| r.depends_on_id)
        .collect();
    let blocked: Vec<_> = tasks::Entity::find()
        .filter(tasks::Column::Id.is_in(ids))
        .filter(tasks::Column::Status.ne("completed"))
        .all(conn)
        .await?
        .into_iter()
        .map(|r| r.id)
        .collect();
    ensure!(
        blocked.is_empty(),
        "task has unfinished dependencies: {blocked:?}"
    );
    Ok(())
}

pub(super) async fn hydrate(
    conn: &impl ConnectionTrait,
    rows: Vec<tasks::Model>,
) -> Result<Vec<ProjectTask>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<_> = rows.iter().map(|r| r.id).collect();
    let owners: BTreeMap<_, _> = claims::Entity::find()
        .filter(claims::Column::TaskId.is_in(ids.clone()))
        .all(conn)
        .await?
        .into_iter()
        .map(|r| (r.task_id, r.session_id))
        .collect();
    let edges = deps::Entity::find()
        .filter(deps::Column::TaskId.is_in(ids))
        .order_by_asc(deps::Column::DependsOnId)
        .all(conn)
        .await?;
    let targets: Vec<_> = edges
        .iter()
        .map(|r| r.depends_on_id)
        .collect();
    let completed: BTreeSet<_> = tasks::Entity::find()
        .filter(tasks::Column::Id.is_in(targets))
        .filter(tasks::Column::Status.eq("completed"))
        .all(conn)
        .await?
        .into_iter()
        .map(|r| r.id)
        .collect();
    let mut dependencies = BTreeMap::<i64, Vec<i64>>::new();
    for edge in edges {
        dependencies
            .entry(edge.task_id)
            .or_default()
            .push(edge.depends_on_id);
    }
    rows.into_iter()
        .map(|row| {
            let depends_on = dependencies
                .remove(&row.id)
                .unwrap_or_default();
            Ok(ProjectTask {
                owner_session_id: owners.get(&row.id).cloned(),
                blocked_by: depends_on
                    .iter()
                    .copied()
                    .filter(|id| !completed.contains(id))
                    .collect(),
                depends_on,
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
        })
        .collect()
}

pub(super) async fn one(
    conn: &impl ConnectionTrait,
    project: &str,
    id: i64,
) -> Result<ProjectTask> {
    hydrate(conn, vec![row(conn, project, id).await?])
        .await?
        .pop()
        .context("task not found")
}

pub(super) fn validate(title: &str, details: &str, priority: i64) -> Result<()> {
    ensure!(!title.trim().is_empty(), "task title must not be empty");
    ensure!(title.len() <= 512, "task title exceeds 512 bytes");
    ensure!(details.len() <= 8_192, "task details exceed 8192 bytes");
    ensure!(
        (-100..=100).contains(&priority),
        "task priority must be between -100 and 100"
    );
    Ok(())
}

/// Release unfinished assignments atomically with a child completion or a
/// project move. Closed tasks keep their owner while that session exists.
pub(crate) async fn release_unfinished(
    conn: &impl ConnectionTrait,
    session: &str,
    keep_project: Option<&str>,
) -> Result<()> {
    let ids: Vec<_> = claims::Entity::find()
        .filter(claims::Column::SessionId.eq(session))
        .all(conn)
        .await?
        .into_iter()
        .map(|r| r.task_id)
        .collect();
    let mut query = tasks::Entity::find()
        .filter(tasks::Column::Id.is_in(ids))
        .filter(tasks::Column::Status.is_not_in(["completed", "cancelled"]));
    if let Some(project) = keep_project {
        query = query.filter(tasks::Column::ProjectId.ne(project));
    }
    for task in query.all(conn).await? {
        claims::Entity::delete_by_id(task.id)
            .exec(conn)
            .await?;
        if task.status == "in_progress" {
            tasks::ActiveModel {
                id: Set(task.id),
                status: Set("pending".into()),
                updated_at: Set(now()),
                ..Default::default()
            }
            .update(conn)
            .await?;
        }
    }
    Ok(())
}
