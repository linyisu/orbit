use super::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) async fn replace(
    conn: &impl ConnectionTrait,
    project: &str,
    id: i64,
    dependencies: Vec<i64>,
) -> Result<()> {
    ensure!(
        dependencies.len() <= 64,
        "a task may have at most 64 dependencies"
    );
    let dependencies: BTreeSet<_> = dependencies.into_iter().collect();
    ensure!(!dependencies.contains(&id), "task cannot depend on itself");
    if dependencies.is_empty() {
        deps::Entity::delete_many()
            .filter(deps::Column::TaskId.eq(id))
            .exec(conn)
            .await?;
        return Ok(());
    }
    let rows = tasks::Entity::find()
        .filter(tasks::Column::ProjectId.eq(project))
        .all(conn)
        .await?;
    let ids: BTreeSet<_> = rows.iter().map(|r| r.id).collect();
    ensure!(
        dependencies
            .iter()
            .all(|id| ids.contains(id)),
        "dependency does not belong to this project"
    );
    let mut graph = BTreeMap::<i64, Vec<i64>>::new();
    for edge in deps::Entity::find()
        .filter(deps::Column::TaskId.is_in(ids))
        .all(conn)
        .await?
    {
        if edge.task_id != id {
            graph
                .entry(edge.task_id)
                .or_default()
                .push(edge.depends_on_id);
        }
    }
    let mut pending: Vec<_> = dependencies.iter().copied().collect();
    let mut visited = BTreeSet::new();
    while let Some(current) = pending.pop() {
        ensure!(current != id, "task dependencies would form a cycle");
        if visited.insert(current) {
            pending.extend(
                graph
                    .get(&current)
                    .into_iter()
                    .flatten()
                    .copied(),
            );
        }
    }
    deps::Entity::delete_many()
        .filter(deps::Column::TaskId.eq(id))
        .exec(conn)
        .await?;
    for target in dependencies {
        deps::ActiveModel {
            task_id: Set(id),
            depends_on_id: Set(target),
        }
        .insert(conn)
        .await?;
    }
    Ok(())
}

pub(super) async fn can_reopen(conn: &impl ConnectionTrait, id: i64) -> Result<()> {
    let ids: Vec<_> = deps::Entity::find()
        .filter(deps::Column::DependsOnId.eq(id))
        .all(conn)
        .await?
        .into_iter()
        .map(|r| r.task_id)
        .collect();
    ensure!(
        tasks::Entity::find()
            .filter(tasks::Column::Id.is_in(ids))
            .filter(tasks::Column::Status.is_in(["in_progress", "completed"]))
            .count(conn)
            .await?
            == 0,
        "cannot reopen a dependency while a dependent task is in progress or completed"
    );
    Ok(())
}
