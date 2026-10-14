use std::collections::BTreeMap;

use sea_orm::{ConnectionTrait, DatabaseConnection};
use sea_orm_migration::prelude::*;

use super::support::{self, id, insert};
use crate::{database::sqlite, migrations::Migrator, store::Store};

type IndexSchema = (i64, i64, Vec<(String, i64)>);

#[derive(Debug, PartialEq, Eq)]
struct Schema {
    columns: Vec<(String, String, i64, Option<String>, i64)>,
    references: Vec<(String, String, String, String, String)>,
    indexes: Vec<IndexSchema>,
}

fn pragma_query(function: &str, table: &str) -> SelectStatement {
    Query::select()
        .from_function(Func::cust(id(function)).arg(table), id("info"))
        .to_owned()
}

async fn schema(db: &DatabaseConnection) -> BTreeMap<String, Schema> {
    let backend = db.get_database_backend();
    let query = Query::select()
        .column(id("name"))
        .from(id("sqlite_schema"))
        .and_where(Expr::col(id("type")).eq("table"))
        .and_where(Expr::col(id("name")).not_like("sqlite_%"))
        .and_where(Expr::col(id("name")).ne("seaql_migrations"))
        .to_owned();
    let mut result = BTreeMap::new();
    for row in db
        .query_all(backend.build(&query))
        .await
        .unwrap()
    {
        let table: String = row.try_get("", "name").unwrap();
        let query = pragma_query("pragma_table_info", &table)
            .columns(["name", "type", "notnull", "dflt_value", "pk"].map(id))
            .order_by(id("cid"), Order::Asc)
            .to_owned();
        let columns = db
            .query_all(backend.build(&query))
            .await
            .unwrap()
            .into_iter()
            .map(|r| {
                (
                    r.try_get("", "name").unwrap(),
                    r.try_get::<String>("", "type")
                        .unwrap()
                        .to_uppercase(),
                    r.try_get("", "notnull").unwrap(),
                    r.try_get("", "dflt_value").unwrap(),
                    r.try_get("", "pk").unwrap(),
                )
            })
            .collect();
        let query = pragma_query("pragma_foreign_key_list", &table)
            .columns(["from", "table", "to", "on_update", "on_delete"].map(id))
            .to_owned();
        let mut references = db
            .query_all(backend.build(&query))
            .await
            .unwrap()
            .into_iter()
            .map(|r| {
                (
                    r.try_get("", "from").unwrap(),
                    r.try_get("", "table").unwrap(),
                    r.try_get("", "to").unwrap(),
                    r.try_get("", "on_update").unwrap(),
                    r.try_get("", "on_delete").unwrap(),
                )
            })
            .collect::<Vec<_>>();
        references.sort();
        let query = pragma_query("pragma_index_list", &table)
            .columns(["name", "unique", "partial"].map(id))
            .to_owned();
        let mut indexes = Vec::new();
        for r in db
            .query_all(backend.build(&query))
            .await
            .unwrap()
        {
            let name: String = r.try_get("", "name").unwrap();
            let query = pragma_query("pragma_index_xinfo", &name)
                .columns(["name", "desc"].map(id))
                .and_where(Expr::col(id("key")).eq(1))
                .order_by(id("seqno"), Order::Asc)
                .to_owned();
            let columns = db
                .query_all(backend.build(&query))
                .await
                .unwrap()
                .into_iter()
                .map(|r| {
                    (
                        r.try_get("", "name").unwrap(),
                        r.try_get("", "desc").unwrap(),
                    )
                })
                .collect();
            indexes.push((
                r.try_get("", "unique").unwrap(),
                r.try_get("", "partial").unwrap(),
                columns,
            ));
        }
        indexes.sort();
        result.insert(
            table,
            Schema {
                columns,
                references,
                indexes,
            },
        );
    }
    result
}

#[tokio::test]
async fn rust_schema_retains_domain_checks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("constraints.db");
    drop(Store::open(&path).await.unwrap());
    let db = support::connect(path).await;
    insert(&db, "projects", &[("id", "project".into())])
        .await
        .unwrap();
    insert(
        &db,
        "sessions",
        &[
            ("id", "session".into()),
            ("workspace", "/workspace".into()),
            ("model", "model".into()),
            ("project_id", "project".into()),
        ],
    )
    .await
    .unwrap();
    let cases: Vec<(&str, Vec<(&str, Value)>)> = vec![
        (
            "runs",
            vec![
                ("session_id", "session".into()),
                ("prompt", "prompt".into()),
                ("status", "unknown".into()),
            ],
        ),
        (
            "project_tasks",
            vec![("project_id", "project".into()), ("title", "".into())],
        ),
        (
            "project_tasks",
            vec![
                ("project_id", "project".into()),
                ("title", "task".into()),
                ("priority", 101.into()),
            ],
        ),
        (
            "project_tasks",
            vec![
                ("project_id", "project".into()),
                ("title", "task".into()),
                ("status", "unknown".into()),
            ],
        ),
        (
            "session_memories",
            vec![
                ("session_id", "session".into()),
                ("category", "unknown".into()),
                ("title", "title".into()),
                ("content", "content".into()),
            ],
        ),
        (
            "session_memories",
            vec![
                ("session_id", "session".into()),
                ("category", "fact".into()),
                ("title", "title".into()),
                ("content", "content".into()),
                ("active", 2.into()),
            ],
        ),
        (
            "session_memories",
            vec![
                ("session_id", "session".into()),
                ("category", "fact".into()),
                ("title", "title".into()),
                ("content", "x".repeat(8_193).into()),
            ],
        ),
        (
            "project_plugins",
            vec![
                ("project_id", "project".into()),
                ("marketplace_source", "source".into()),
                ("plugin_name", "".into()),
                ("package_source", "package".into()),
            ],
        ),
        (
            "agent_sessions",
            vec![
                ("session_id", "session".into()),
                ("role", "unknown".into()),
                ("task", "task".into()),
            ],
        ),
        (
            "agent_messages",
            vec![
                ("session_id", "session".into()),
                ("content", "input".into()),
                ("received", 2.into()),
            ],
        ),
    ];
    for (table, values) in cases {
        let error = insert(&db, table, &values)
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("CHECK constraint failed"),
            "{table}: {error}"
        );
    }
    db.close().await.unwrap();
}

#[tokio::test]
async fn every_released_schema_upgrades_without_losing_history_and_reopens_idempotently() {
    let reference_dir = tempfile::tempdir().unwrap();
    let reference_path = reference_dir.path().join("current.db");
    drop(
        Store::open(&reference_path)
            .await
            .unwrap(),
    );
    let reference = support::connect(reference_path).await;
    let expected_schema = schema(&reference).await;
    assert!(
        !expected_schema["sessions"]
            .columns
            .iter()
            .any(|column| column.0 == "demo")
    );
    reference.close().await.unwrap();
    // Cover databases made before SeaORM and databases that already have its ledger.
    for ledger in [false, true] {
        let latest: u32 = if ledger {
            Migrator::migrations()
                .len()
                .try_into()
                .unwrap()
        } else {
            27
        };
        for version in 0..=latest {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("legacy.db");
            let legacy = support::connect(&path).await;
            if ledger {
                Migrator::up(&legacy, Some(version))
                    .await
                    .unwrap();
                sqlite::set_schema_version(&legacy, version)
                    .await
                    .unwrap();
            } else {
                support::legacy_schema(&legacy, version as usize).await;
            }
            if version > 0 {
                if version >= 2 {
                    insert(
                        &legacy,
                        "projects",
                        &[("id", "project".into()), ("path", "/workspace".into())],
                    )
                    .await
                    .unwrap();
                }
                let mut values = vec![
                    ("id", "session".into()),
                    ("workspace", "/workspace".into()),
                    ("model", "model".into()),
                    ("title", "Keep me".into()),
                ];
                if SchemaManager::new(&legacy)
                    .has_column("sessions", "demo")
                    .await
                    .unwrap()
                {
                    values.push(("demo", true.into()));
                }
                if version >= 2 {
                    values.push(("project_id", "project".into()));
                }
                insert(&legacy, "sessions", &values)
                    .await
                    .unwrap();
                insert(
                    &legacy,
                    "runs",
                    &[
                        ("id", 7.into()),
                        ("session_id", "session".into()),
                        ("prompt", "original prompt".into()),
                        ("status", "completed".into()),
                    ],
                )
                .await
                .unwrap();
                insert(
                    &legacy,
                    "events",
                    &[
                        ("run_id", 7.into()),
                        (
                            "payload",
                            serde_json::to_string(&orbit_agent::events::AgentEvent::TurnStarted {
                                turn: 1,
                            })
                            .unwrap()
                            .into(),
                        ),
                    ],
                )
                .await
                .unwrap();
            }
            legacy.close().await.unwrap();
            for _ in 0..2 {
                let store = Store::open(&path)
                    .await
                    .unwrap_or_else(|e| panic!("v{version}, ledger={ledger}: {e:#}"));
                if version > 0 {
                    let snapshot = store.load("session").await.unwrap();
                    assert_eq!(snapshot.session.title, "Keep me", "v{version}");
                    assert_eq!(snapshot.runs[0].prompt, "original prompt", "v{version}");
                    assert_eq!(snapshot.runs[0].events.len(), 1);
                    let (gap, _) = store
                        .canonical_messages("session", None, 1)
                        .await
                        .unwrap();
                    assert_eq!(gap, version < 14);
                }
                drop(store);
            }
            let db = support::connect(path).await;
            assert_eq!(
                schema(&db).await,
                expected_schema,
                "v{version}, ledger={ledger}"
            );
            assert_eq!(
                support::count(&db, "seaql_migrations").await,
                Migrator::migrations().len() as i64
            );
            assert_eq!(
                sqlite::schema_version(&db)
                    .await
                    .unwrap(),
                Migrator::migrations().len() as i64
            );
            db.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn upgrade_failure_rolls_back_schema_data_version_and_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken.db");
    let db = support::connect(&path).await;
    support::legacy_schema(&db, 1).await;
    insert(
        &db,
        "sessions",
        &[
            ("id", "session".into()),
            ("workspace", "/workspace".into()),
            ("model", "model".into()),
            ("demo", false.into()),
        ],
    )
    .await
    .unwrap();
    // Collide with v5 after earlier migrations have transformed the schema and data.
    SchemaManager::new(&db)
        .create_table(
            Table::create()
                .table(id("project_tasks"))
                .col(ColumnDef::new(id("unexpected")).integer())
                .to_owned(),
        )
        .await
        .unwrap();
    db.close().await.unwrap();
    assert!(Store::open(&path).await.is_err());
    let db = support::connect(&path).await;
    assert_eq!(
        sqlite::schema_version(&db)
            .await
            .unwrap(),
        1
    );
    for table in ["projects", "project_paths", "seaql_migrations"] {
        assert!(!support::table_exists(&db, table).await);
    }
    let query = Query::select()
        .column(id("workspace"))
        .from(id("sessions"))
        .and_where(Expr::col(id("id")).eq("session"))
        .to_owned();
    assert_eq!(
        db.query_one(db.get_database_backend().build(&query))
            .await
            .unwrap()
            .unwrap()
            .try_get::<String>("", "workspace")
            .unwrap(),
        "/workspace"
    );
    SchemaManager::new(&db)
        .drop_table(
            Table::drop()
                .table(id("project_tasks"))
                .to_owned(),
        )
        .await
        .unwrap();
    db.close().await.unwrap();
    assert!(Store::open(&path).await.is_ok());
}

#[tokio::test]
async fn inconsistent_ledger_and_broken_references_are_rejected_without_adopting_them() {
    for broken_reference in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.db");
        if !broken_reference {
            drop(Store::open(&path).await.unwrap());
        }
        let db = support::connect(&path).await;
        if broken_reference {
            support::legacy_schema(&db, 27).await;
            insert(
                &db,
                "runs",
                &[
                    ("session_id", "missing".into()),
                    ("prompt", "prompt".into()),
                    ("status", "completed".into()),
                ],
            )
            .await
            .unwrap();
        } else {
            let query = Query::delete()
                .from_table(id("seaql_migrations"))
                .and_where(Expr::col(id("version")).like("%projects"))
                .to_owned();
            db.execute(db.get_database_backend().build(&query))
                .await
                .unwrap();
        }
        db.close().await.unwrap();
        let error = Store::open(&path)
            .await
            .err()
            .expect("invalid state rejected");
        assert!(
            error
                .to_string()
                .contains(if broken_reference {
                    "broken references"
                } else {
                    "ledger"
                }),
            "{error:#}"
        );
        let db = support::connect(path).await;
        assert_eq!(
            support::table_exists(&db, "seaql_migrations").await,
            !broken_reference
        );
        db.close().await.unwrap();
    }
}
