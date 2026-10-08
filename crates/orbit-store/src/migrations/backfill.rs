//! Historical data transforms are kept independent of today's application entities.
use sea_orm::{ConnectionTrait, DbErr};
use sea_orm_migration::prelude::*;
use std::collections::BTreeMap;

fn id(value: &str) -> Alias {
    Alias::new(value)
}

pub(super) async fn projects(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let db = manager.get_connection();
    let backend = manager.get_database_backend();
    let workspaces = Query::select()
        .column(id("workspace"))
        .from(id("sessions"))
        .distinct()
        .to_owned();
    for row in db.query_all(backend.build(&workspaces)).await? {
        let workspace: String = row.try_get("", "workspace")?;
        let project = crate::database::new_id().map_err(|e| DbErr::Custom(e.to_string()))?;
        let insert = Query::insert()
            .into_table(id("projects"))
            .columns([id("id"), id("path"), id("legacy")])
            .values_panic([project.clone().into(), workspace.clone().into(), 1.into()])
            .to_owned();
        db.execute(backend.build(&insert)).await?;
        let insert = Query::insert()
            .into_table(id("project_paths"))
            .columns([id("project_id"), id("path")])
            .values_panic([project.into(), workspace.into()])
            .to_owned();
        db.execute(backend.build(&insert)).await?;
    }
    // SQLite cannot add a foreign-key constraint with ALTER TABLE. Rebuild explicitly.
    manager
        .create_table(
            Table::create()
                .table(id("sessions_v2"))
                .col(ColumnDef::new(id("id")).text().primary_key())
                .col(ColumnDef::new(id("workspace")).text().not_null())
                .col(ColumnDef::new(id("model")).text().not_null())
                .col(ColumnDef::new(id("demo")).integer().not_null())
                .col(ColumnDef::new(id("title")).text().not_null().default(""))
                .col(
                    ColumnDef::new(id("updated_at"))
                        .integer()
                        .not_null()
                        .default(super::timestamp_default()),
                )
                .col(
                    ColumnDef::new(id("history"))
                        .text()
                        .not_null()
                        .default("[]"),
                )
                .col(ColumnDef::new(id("project_id")).text())
                .foreign_key(
                    ForeignKey::create()
                        .from(id("sessions_v2"), id("project_id"))
                        .to(id("projects"), id("id")),
                )
                .to_owned(),
        )
        .await?;
    let columns = [
        "id",
        "workspace",
        "model",
        "demo",
        "title",
        "updated_at",
        "history",
    ];
    let select = Query::select()
        .columns(columns.map(|column| (id("sessions"), id(column))))
        .column((id("projects"), id("id")))
        .from(id("sessions"))
        .left_join(
            id("projects"),
            Expr::col((id("projects"), id("path"))).equals((id("sessions"), id("workspace"))),
        )
        .to_owned();
    let insert = Query::insert()
        .into_table(id("sessions_v2"))
        .columns(columns.into_iter().chain(["project_id"]).map(id))
        .select_from(select)
        .map_err(|e| DbErr::Custom(e.to_string()))?
        .to_owned();
    db.execute(backend.build(&insert)).await?;
    manager
        .drop_table(Table::drop().table(id("sessions")).to_owned())
        .await?;
    manager
        .rename_table(
            Table::rename()
                .table(id("sessions_v2"), id("sessions"))
                .to_owned(),
        )
        .await?;
    Ok(())
}

pub(super) async fn session_memories(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let db = manager.get_connection();
    let backend = manager.get_database_backend();
    let select = Query::select()
        .columns([id("id"), id("project_id")])
        .from(id("sessions"))
        .to_owned();
    let mut by_project: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in db.query_all(backend.build(&select)).await? {
        if let Some(project) = row.try_get::<Option<String>>("", "project_id")? {
            by_project
                .entry(project)
                .or_default()
                .push(row.try_get("", "id")?);
        }
    }
    let select = Query::select()
        .columns(
            [
                "project_id",
                "source_session_id",
                "category",
                "title",
                "content",
                "active",
                "created_at",
                "updated_at",
            ]
            .map(id),
        )
        .from(id("project_memories"))
        .order_by(id("id"), Order::Asc)
        .to_owned();
    for row in db.query_all(backend.build(&select)).await? {
        let project: String = row.try_get("", "project_id")?;
        let source: Option<String> = row.try_get("", "source_session_id")?;
        let Some(sessions) = by_project.get(&project) else {
            continue;
        };
        let owner = match source {
            Some(source) if sessions.contains(&source) => source,
            None if sessions.len() == 1 => sessions[0].clone(),
            _ => continue,
        };
        let insert = Query::insert()
            .into_table(id("session_memories"))
            .columns(
                [
                    "session_id",
                    "category",
                    "title",
                    "content",
                    "active",
                    "created_at",
                    "updated_at",
                ]
                .map(id),
            )
            .values_panic([
                owner.into(),
                row.try_get::<String>("", "category")?.into(),
                row.try_get::<String>("", "title")?.into(),
                row.try_get::<String>("", "content")?.into(),
                row.try_get::<i64>("", "active")?.into(),
                row.try_get::<i64>("", "created_at")?.into(),
                row.try_get::<i64>("", "updated_at")?.into(),
            ])
            .to_owned();
        db.execute(backend.build(&insert)).await?;
    }
    Ok(())
}

pub(super) async fn message_gaps(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let select = Query::select()
        .column(id("id"))
        .expr(Expr::val("session predates the canonical message log"))
        .from(id("sessions"))
        .to_owned();
    let insert = Query::insert()
        .into_table(id("session_log_gaps"))
        .columns([id("session_id"), id("reason")])
        .select_from(select)
        .map_err(|e| DbErr::Custom(e.to_string()))?
        .to_owned();
    manager
        .get_connection()
        .execute(manager.get_database_backend().build(&insert))
        .await?;
    Ok(())
}
