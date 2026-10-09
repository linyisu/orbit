//! Measure SQLite WAL commit cost on the supplied filesystem, without touching
//! Orbit data. This is a diagnostic, not a timing assertion or durability test.
//! cargo run -p orbit-store --example write_latency -- target
use anyhow::{Result, ensure};
use sea_orm::{ConnectOptions, Database, Schema, Set, TransactionTrait, entity::prelude::*};
use std::{path::PathBuf, time::Instant};

mod sample {
    use sea_orm::entity::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "samples")]
    pub struct Model {
        #[sea_orm(primary_key)]
        pub id: i32,
        pub payload: String,
    }
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
    impl ActiveModelBehavior for ActiveModel {}
}

#[tokio::main]
async fn main() -> Result<()> {
    let root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    ensure!(
        root.is_dir(),
        "provide an existing directory on the filesystem to measure"
    );
    let directory = tempfile::Builder::new()
        .prefix("orbit-write-latency-")
        .tempdir_in(root)?;
    println!(
        "Temporary databases: {} (removed on completion)",
        directory.path().display()
    );
    println!("mode   batch  commits  p50 ms  p95 ms  total ms  records/s");
    // Alternate order each round to reduce warmup/order effects. All rows have
    // the same 1 KiB payload and 1024 records; report per-commit and total cost.
    for round in 0..2 {
        let modes = if round == 0 {
            ["FULL", "NORMAL"]
        } else {
            ["NORMAL", "FULL"]
        };
        for mode in modes {
            for batch in [1, 16, 64] {
                let path = directory.path().join(format!("{mode}-{batch}-{round}.db"));
                let mut options = ConnectOptions::new("sqlite::memory:");
                options
                    .max_connections(1)
                    .min_connections(1)
                    .sqlx_logging(false)
                    .map_sqlx_sqlite_opts(move |opts| {
                        opts.filename(&path)
                            .in_memory(false)
                            .create_if_missing(true)
                            .pragma("journal_mode", "WAL")
                            .pragma("synchronous", mode)
                    });
                let db = Database::connect(options).await?;
                let backend = db.get_database_backend();
                db.execute(
                    backend.build(&Schema::new(backend).create_table_from_entity(sample::Entity)),
                )
                .await?;
                let payload = "x".repeat(1024);
                let started = Instant::now();
                let mut timings = Vec::new();
                for start in (0..1024).step_by(batch) {
                    let commit_started = Instant::now();
                    let tx = db.begin().await?;
                    sample::Entity::insert_many((start..start + batch).map(|n| {
                        sample::ActiveModel {
                            id: Set(n as i32 + 1),
                            payload: Set(payload.clone()),
                        }
                    }))
                    .exec(&tx)
                    .await?;
                    tx.commit().await?;
                    timings.push(commit_started.elapsed().as_secs_f64() * 1000.0);
                }
                let elapsed = started.elapsed().as_secs_f64();
                timings.sort_by(f64::total_cmp);
                println!(
                    "{mode:6} {batch:5} {:7} {:7.3} {:7.3} {:9.2} {:10.0}",
                    timings.len(),
                    timings[timings.len() / 2],
                    timings[(timings.len() - 1) * 95 / 100],
                    elapsed * 1000.0,
                    1024.0 / elapsed
                );
                db.close().await?;
            }
        }
    }
    Ok(())
}
