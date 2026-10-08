//! SQLite connection controls not represented by SeaQuery's schema API.
//! No application data or table definitions are interpolated here.
use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement};

pub(crate) async fn foreign_keys(db: &impl ConnectionTrait, enabled: bool) -> Result<(), DbErr> {
    db.execute_unprepared(if enabled {
        "PRAGMA foreign_keys=ON"
    } else {
        "PRAGMA foreign_keys=OFF"
    })
    .await?;
    Ok(())
}

pub(crate) async fn schema_version(db: &impl ConnectionTrait) -> Result<i64, DbErr> {
    db.query_one(Statement::from_string(
        DbBackend::Sqlite,
        "PRAGMA user_version",
    ))
    .await?
    .ok_or_else(|| DbErr::Custom("missing schema version".into()))?
    .try_get("", "user_version")
}

pub(crate) async fn set_schema_version(
    db: &impl ConnectionTrait,
    version: u32,
) -> Result<(), DbErr> {
    db.execute_unprepared(&format!("PRAGMA user_version={version}"))
        .await?;
    Ok(())
}

pub(crate) async fn references_valid(db: &impl ConnectionTrait) -> Result<bool, DbErr> {
    Ok(db
        .query_all(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA foreign_key_check",
        ))
        .await?
        .is_empty())
}
