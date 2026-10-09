//! Connection policy and per-database write scheduling, independent of domain repositories.
pub(crate) mod sqlite;
use anyhow::{Context, Result};
use sea_orm::{ConnectOptions, ConnectionTrait, DatabaseConnection};
use std::{
    future::Future,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone)]
pub(crate) struct Database {
    writer: DatabaseConnection,
    reader: DatabaseConnection,
    lock: PathBuf,
}

impl Database {
    pub async fn open(path: &Path) -> Result<Self> {
        let lock = path.with_extension("write-lock");
        let _lease = write_lease(lock.clone()).await?;
        let writer = connect(path, false).await?;
        if let Err(error) = crate::migrations::run(&writer).await {
            writer.close().await?;
            return Err(error);
        }
        let reader = connect(path, true).await?;
        Ok(Self {
            writer,
            reader,
            lock,
        })
    }

    pub async fn write<T, F: Future<Output = Result<T>>>(
        &self,
        operation: impl FnOnce(DatabaseConnection) -> F,
    ) -> Result<T> {
        // OS leases disappear when their handle/process closes; the lock file itself is not ownership.
        // Serialize read-modify-write transactions across clients without blocking WAL readers.
        let _lease = write_lease(self.lock.clone()).await?;
        operation(self.writer.clone()).await
    }

    pub async fn read<T, F: Future<Output = Result<T>>>(
        &self,
        operation: impl FnOnce(DatabaseConnection) -> F,
    ) -> Result<T> {
        operation(self.reader.clone()).await
    }

    pub async fn reclaim(&self) -> Result<()> {
        self.write(|connection| async move {
            connection
                .execute_unprepared("PRAGMA wal_checkpoint(TRUNCATE); VACUUM;")
                .await?;
            Ok(())
        })
        .await
    }
}

async fn connect(path: &Path, read_only: bool) -> Result<DatabaseConnection> {
    let path = path.to_owned();
    let mut options = ConnectOptions::new("sqlite::memory:");
    options
        .max_connections(if read_only { 4 } else { 1 })
        .min_connections(1)
        .connect_timeout(Duration::from_secs(3))
        .acquire_timeout(Duration::from_secs(3))
        .sqlx_logging(false)
        .map_sqlx_sqlite_opts(move |options| {
            options
                .filename(&path)
                .in_memory(false)
                .shared_cache(false)
                .create_if_missing(!read_only)
                .read_only(read_only)
                .foreign_keys(true)
                .busy_timeout(Duration::from_secs(3))
                .pragma("journal_mode", "WAL")
                // Keep committed conversation/approval receipts durable across
                // power loss. WAL + NORMAL preserves consistency, but may lose
                // recent commits after an OS crash; optimize transaction volume
                // before weakening this guarantee. See examples/write_latency.rs.
                .pragma("synchronous", "FULL")
        });
    Ok(sea_orm::Database::connect(options).await?)
}

async fn write_lease(path: PathBuf) -> Result<std::fs::File> {
    let file = tokio::task::spawn_blocking(move || crate::store::private_file(&path)).await??;
    let started = tokio::time::Instant::now();
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock)
                if started.elapsed() < Duration::from_secs(3) =>
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(error) => return Err(anyhow::anyhow!(error)).context("database writer is busy"),
        }
    }
}

pub(crate) fn new_id() -> Result<String> {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow::anyhow!("random ID generation failed: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .min(i64::MAX as u64) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::projects;
    use sea_orm::{ActiveModelTrait, EntityTrait, Set, TransactionTrait};

    #[tokio::test]
    async fn cancelled_writer_rolls_back_and_releases_the_cross_connection_lease() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cancel.db");
        let database = Database::open(&path).await.unwrap();
        let other = Database::open(&path).await.unwrap();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let writer = tokio::spawn(async move {
            database
                .write(|conn| async move {
                    let tx = conn.begin().await?;
                    projects::ActiveModel {
                        id: Set("cancelled".into()),
                        ..Default::default()
                    }
                    .insert(&tx)
                    .await?;
                    entered.send(()).unwrap();
                    std::future::pending::<()>().await;
                    tx.commit().await?;
                    Ok(())
                })
                .await
        });
        ready.await.unwrap();
        writer.abort();
        assert!(writer.await.unwrap_err().is_cancelled());
        other
            .write(|conn| async move {
                let tx = conn.begin().await?;
                assert!(
                    projects::Entity::find_by_id("cancelled")
                        .one(&tx)
                        .await?
                        .is_none()
                );
                projects::ActiveModel {
                    id: Set("committed".into()),
                    ..Default::default()
                }
                .insert(&tx)
                .await?;
                tx.commit().await?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(
            other
                .read(|conn| async move {
                    Ok(projects::Entity::find_by_id("committed")
                        .one(&conn)
                        .await?
                        .is_some())
                })
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn reader_pool_enforces_read_only_and_writer_enforces_foreign_keys() {
        let dir = tempfile::tempdir().unwrap();
        let database = Database::open(&dir.path().join("policy.db")).await.unwrap();
        let error = database
            .read(|conn| async move {
                projects::ActiveModel {
                    id: Set("forbidden".into()),
                    ..Default::default()
                }
                .insert(&conn)
                .await?;
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("readonly"), "{error:#}");
        let error = database
            .write(|conn| async move {
                crate::entities::runs::ActiveModel {
                    session_id: Set("missing".into()),
                    prompt: Set("prompt".into()),
                    ..Default::default()
                }
                .insert(&conn)
                .await?;
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("FOREIGN KEY"), "{error:#}");
    }
}
