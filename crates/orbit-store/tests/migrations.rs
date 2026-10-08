mod support;
use orbit_store::store::Store;
use rusqlite::{Connection, params};

#[tokio::test]
async fn every_released_schema_upgrades_without_losing_history_and_reopens_idempotently() {
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
