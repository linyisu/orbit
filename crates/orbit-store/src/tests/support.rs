//! Test database construction through the same Rust migration definitions shipped by Orbit.
//! Historical fixtures never pretend a current schema is an older release.
use std::path::Path;

use sea_orm::{
    ConnectOptions, ConnectionTrait, DatabaseConnection, DbErr, ExecResult, Iterable, Schema,
};
use sea_orm_migration::prelude::*;

use crate::{database::sqlite, entities::sessions, migrations::Migrator};

pub fn id(name: &str) -> Alias {
    Alias::new(name)
}

pub async fn connect(path: impl AsRef<Path>) -> DatabaseConnection {
    let path = path.as_ref().to_owned();
    let mut options = ConnectOptions::new("sqlite::memory:");
    options
        .max_connections(1)
        .min_connections(1)
        .sqlx_logging(false)
        .map_sqlx_sqlite_opts(move |options| {
            options
                .filename(&path)
                .in_memory(false)
                .create_if_missing(true)
                .foreign_keys(false)
        });
    sea_orm::Database::connect(options).await.unwrap()
}

pub async fn legacy_schema(db: &DatabaseConnection, version: usize) {
    assert!(version <= 27);
    for migration in Migrator::migrations().into_iter().take(version) {
        migration.up(&SchemaManager::new(db)).await.unwrap();
    }
    sqlite::set_schema_version(db, version.try_into().unwrap())
        .await
        .unwrap();
}

pub async fn released_migration(db: &DatabaseConnection, version: usize) {
    Migrator::migrations()[version - 1]
        .up(&SchemaManager::new(db))
        .await
        .unwrap();
}

pub async fn insert(
    db: &DatabaseConnection,
    table: &str,
    values: &[(&str, Value)],
) -> Result<ExecResult, DbErr> {
    let query = Query::insert()
        .into_table(id(table))
        .columns(values.iter().map(|(column, _)| id(column)))
        .values_panic(values.iter().map(|(_, value)| value.clone().into()))
        .to_owned();
    db.execute(db.get_database_backend().build(&query)).await
}

pub async fn count(db: &DatabaseConnection, table: &str) -> i64 {
    let query = Query::select()
        .expr_as(Expr::col(Asterisk).count(), id("count"))
        .from(id(table))
        .to_owned();
    db.query_one(db.get_database_backend().build(&query))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "count")
        .unwrap()
}

pub async fn table_exists(db: &DatabaseConnection, table: &str) -> bool {
    SchemaManager::new(db).has_table(table).await.unwrap()
}

/// Inject a real SQLite constraint failure after events/status writes, without trigger SQL.
/// Rebuild from the entity definition and preserve all existing rows and foreign keys.
pub async fn restrict_history(db: &DatabaseConnection, restricted: bool) {
    let backend = db.get_database_backend();
    let manager = SchemaManager::new(db);
    let mut table = Schema::new(backend).create_table_from_entity(sessions::Entity);
    table.table(id("constrained_sessions"));
    if restricted {
        let query = Query::select()
            .column(sessions::Column::History)
            .from(sessions::Entity)
            .distinct()
            .to_owned();
        let values = db
            .query_all(backend.build(&query))
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.try_get::<String>("", "history").unwrap())
            .collect::<Vec<_>>();
        table.check(Expr::col(sessions::Column::History).is_in(values));
    }
    manager.create_table(table).await.unwrap();
    let selection = Query::select()
        .columns(sessions::Column::iter())
        .from(sessions::Entity)
        .to_owned();
    let insert = Query::insert()
        .into_table(id("constrained_sessions"))
        .columns(sessions::Column::iter())
        .select_from(selection)
        .unwrap()
        .to_owned();
    db.execute(backend.build(&insert)).await.unwrap();
    manager
        .drop_table(Table::drop().table(sessions::Entity).to_owned())
        .await
        .unwrap();
    manager
        .rename_table(
            Table::rename()
                .table(id("constrained_sessions"), sessions::Entity)
                .to_owned(),
        )
        .await
        .unwrap();
}
