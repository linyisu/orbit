mod support;
use orbit_store::store::Store;
use rusqlite::{Connection, params};

type IndexSchema = (i64, i64, Vec<(String, i64)>);

#[derive(Debug, PartialEq, Eq)]
struct Schema {
    columns: Vec<(String, String, i64, Option<String>, i64)>,
    references: Vec<(String, String, String, String, String)>,
    indexes: Vec<IndexSchema>,
}

fn schema(db: &Connection) -> std::collections::BTreeMap<String, Schema> {
    let tables = db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name<>'seaql_migrations' ORDER BY name")
        .unwrap().query_map([], |row| row.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
    tables.into_iter().map(|table| {
        let columns = db.prepare("SELECT name,upper(type),\"notnull\",dflt_value,pk FROM pragma_table_info(?1) ORDER BY cid")
            .unwrap().query_map([&table], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))
            .unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        let mut references = db.prepare("SELECT \"from\",\"table\",\"to\",on_update,on_delete FROM pragma_foreign_key_list(?1)")
            .unwrap().query_map([&table], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))
            .unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        references.sort();
        let indices = db.prepare("SELECT name,\"unique\",partial FROM pragma_index_list(?1)")
            .unwrap().query_map([&table], |row| Ok((row.get::<_, String>(0)?,row.get::<_, i64>(1)?,row.get::<_, i64>(2)?)))
            .unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        let mut indexes = indices.into_iter().map(|(name, unique, partial)| {
            let columns = db.prepare("SELECT name,desc FROM pragma_index_xinfo(?1) WHERE key=1 ORDER BY seqno")
                .unwrap().query_map([name], |row| Ok((row.get(0)?,row.get(1)?)))
                .unwrap().collect::<Result<Vec<_>, _>>().unwrap();
            (unique, partial, columns)
        }).collect::<Vec<_>>();
        indexes.sort();
        (table, Schema { columns, references, indexes })
    }).collect()
}

#[tokio::test]
async fn rust_schema_retains_domain_checks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("constraints.db");
    drop(Store::open(&path).await.unwrap());
    let current = Connection::open(path).unwrap();
    let historical = Connection::open_in_memory().unwrap();
    support::legacy_schema(&historical, 27);
    for db in [&current, &historical] {
        db.execute("INSERT INTO projects(id) VALUES('project')", [])
            .unwrap();
        db.execute("INSERT INTO sessions(id,workspace,model,demo,project_id) VALUES('session','/workspace','model',0,'project')", []).unwrap();
        for statement in [
            "INSERT INTO runs(session_id,prompt,status) VALUES('session','prompt','unknown')",
            "INSERT INTO project_tasks(project_id,title) VALUES('project','')",
            "INSERT INTO project_tasks(project_id,title,priority) VALUES('project','task',101)",
            "INSERT INTO project_tasks(project_id,title,status) VALUES('project','task','unknown')",
            "INSERT INTO session_memories(session_id,category,title,content) VALUES('session','unknown','title','content')",
            "INSERT INTO session_memories(session_id,category,title,content,active) VALUES('session','fact','title','content',2)",
            "INSERT INTO project_plugins(project_id,marketplace_source,plugin_name,package_source) VALUES('project','source','','package')",
            "INSERT INTO agent_sessions(session_id,role,task) VALUES('session','unknown','task')",
            "INSERT INTO agent_messages(session_id,content,received) VALUES('session','input',2)",
        ] {
            let error = db.execute(statement, []).unwrap_err();
            assert!(
                matches!(error, rusqlite::Error::SqliteFailure(code, _) if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_CHECK),
                "{error}"
            );
        }
        let error = db.execute("INSERT INTO session_memories(session_id,category,title,content) VALUES('session','fact','title',?1)", ["x".repeat(8193)]).unwrap_err();
        assert!(
            matches!(error, rusqlite::Error::SqliteFailure(code, _) if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_CHECK)
        );
    }
}

#[tokio::test]
async fn every_released_schema_upgrades_without_losing_history_and_reopens_idempotently() {
    let reference = Connection::open_in_memory().unwrap();
    support::legacy_schema(&reference, 27);
    let expected_schema = schema(&reference);
    for version in 0..=27 {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        let legacy = Connection::open(&path).unwrap();
        support::legacy_schema(&legacy, version);
        if version > 0 {
            if version >= 2 {
                legacy
                    .execute(
                        "INSERT INTO projects(id,path) VALUES('project','/workspace')",
                        [],
                    )
                    .unwrap();
            }
            legacy.execute("INSERT INTO sessions(id,workspace,model,demo,title,history) VALUES('session','/workspace','model',0,'Keep me','[]')", []).unwrap();
            if version >= 2 {
                legacy
                    .execute("UPDATE sessions SET project_id='project'", [])
                    .unwrap();
            }
            legacy.execute("INSERT INTO runs(id,session_id,prompt,status) VALUES(7,'session','original prompt','completed')", []).unwrap();
            legacy
                .execute(
                    "INSERT INTO events(run_id,payload) VALUES(7,?1)",
                    [
                        serde_json::to_string(&orbit_agent::events::AgentEvent::TurnStarted {
                            turn: 1,
                        })
                        .unwrap(),
                    ],
                )
                .unwrap();
        }
        drop(legacy);
        for _ in 0..2 {
            let store = Store::open(&path)
                .await
                .unwrap_or_else(|e| panic!("v{version}: {e:#}"));
            if version > 0 {
                let snapshot = store.load("session").await.unwrap();
                assert_eq!(snapshot.session.title, "Keep me", "v{version}");
                assert_eq!(snapshot.runs[0].prompt, "original prompt", "v{version}");
                assert_eq!(snapshot.runs[0].events.len(), 1);
                let (gap, _) = store.canonical_messages("session", None, 1).await.unwrap();
                assert_eq!(gap, version < 14);
            }
            drop(store);
        }
        let db = Connection::open(&path).unwrap();
        assert_eq!(
            schema(&db),
            expected_schema,
            "schema from version {version}"
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM seaql_migrations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            27
        );
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            27
        );
    }
}

#[tokio::test]
async fn upgrade_failure_rolls_back_schema_data_version_and_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken.db");
    let db = Connection::open(&path).unwrap();
    support::legacy_schema(&db, 1);
    db.execute(
        "INSERT INTO sessions(id,workspace,model,demo) VALUES('session','/workspace','model',0)",
        [],
    )
    .unwrap();
    // Force the v5 migration to fail after earlier migrations changed schema and data.
    db.execute_batch("CREATE VIEW project_tasks AS SELECT 1;")
        .unwrap();
    drop(db);
    assert!(Store::open(&path).await.is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    for table in ["projects", "project_paths", "seaql_migrations"] {
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name=?1",
                [table],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    assert_eq!(
        db.query_row(
            "SELECT workspace FROM sessions WHERE id='session'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "/workspace"
    );
    db.execute_batch("DROP VIEW project_tasks").unwrap();
    drop(db);
    assert!(Store::open(&path).await.is_ok());
}

#[tokio::test]
async fn inconsistent_ledger_and_broken_references_are_rejected_without_adopting_them() {
    for broken_reference in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("invalid.db");
        if broken_reference {
            let db = Connection::open(&path).unwrap();
            support::legacy_schema(&db, 27);
            db.pragma_update(None, "foreign_keys", false).unwrap();
            db.execute(
                "INSERT INTO runs(session_id,prompt,status) VALUES(?1,'prompt','completed')",
                params!["missing"],
            )
            .unwrap();
        } else {
            drop(Store::open(&path).await.unwrap());
            Connection::open(&path)
                .unwrap()
                .execute(
                    "DELETE FROM seaql_migrations WHERE version LIKE '%projects'",
                    [],
                )
                .unwrap();
        }
        let error = Store::open(&path)
            .await
            .err()
            .expect("invalid state rejected");
        assert!(
            error.to_string().contains(if broken_reference {
                "broken references"
            } else {
                "ledger"
            }),
            "{error:#}"
        );
        let db = Connection::open(&path).unwrap();
        let ledgers: i64 = db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='seaql_migrations'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(ledgers, if broken_reference { 0 } else { 1 });
    }
}
