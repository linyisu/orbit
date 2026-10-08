//! Versioned Rust migrations. Released names remain stable in the SeaORM ledger.
//! Schema changes use SeaQuery; data transforms use typed query builders.
use crate::database::sqlite;
use anyhow::{Result, ensure};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseConnection, EntityTrait, Set, TransactionTrait,
};
use sea_orm_migration::{prelude::*, seaql_migrations};
mod backfill;
mod m00000000000001_sessions;
mod m00000000000002_projects;
mod m00000000000003_run_duration;
mod m00000000000004_reasoning;
mod m00000000000005_tasks_memories;
mod m00000000000006_event_time;
mod m00000000000007_undo;
mod m00000000000008_git_baseline;
mod m00000000000009_plugins;
mod m00000000000010_session_memory;
mod m00000000000011_archives;
mod m00000000000012_checkpoints;
mod m00000000000013_workspace_baseline;
mod m00000000000014_message_log;
mod m00000000000015_compaction;
mod m00000000000016_plugin_revision;
mod m00000000000017_workspace_boundary;
mod m00000000000018_tabs;
mod m00000000000019_runtime;
mod m00000000000020_file_grants;
mod m00000000000021_subagents;
mod m00000000000022_side_questions;
mod m00000000000023_request_receipts;
mod m00000000000024_subagent_parent;
mod m00000000000025_questions;
mod m00000000000026_plans;
mod m00000000000027_input_sequence;
const LEGACY_VERSION: i64 = 27;
fn timestamp_default() -> SimpleExpr {
    // SQLite expression defaults need parentheses; a single-item tuple supplies them.
    Expr::tuple([Func::cust(Alias::new("unixepoch")).into()]).into()
}
pub(crate) struct Migrator;
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m00000000000001_sessions::Migration),
            Box::new(m00000000000002_projects::Migration),
            Box::new(m00000000000003_run_duration::Migration),
            Box::new(m00000000000004_reasoning::Migration),
            Box::new(m00000000000005_tasks_memories::Migration),
            Box::new(m00000000000006_event_time::Migration),
            Box::new(m00000000000007_undo::Migration),
            Box::new(m00000000000008_git_baseline::Migration),
            Box::new(m00000000000009_plugins::Migration),
            Box::new(m00000000000010_session_memory::Migration),
            Box::new(m00000000000011_archives::Migration),
            Box::new(m00000000000012_checkpoints::Migration),
            Box::new(m00000000000013_workspace_baseline::Migration),
            Box::new(m00000000000014_message_log::Migration),
            Box::new(m00000000000015_compaction::Migration),
            Box::new(m00000000000016_plugin_revision::Migration),
            Box::new(m00000000000017_workspace_boundary::Migration),
            Box::new(m00000000000018_tabs::Migration),
            Box::new(m00000000000019_runtime::Migration),
            Box::new(m00000000000020_file_grants::Migration),
            Box::new(m00000000000021_subagents::Migration),
            Box::new(m00000000000022_side_questions::Migration),
            Box::new(m00000000000023_request_receipts::Migration),
            Box::new(m00000000000024_subagent_parent::Migration),
            Box::new(m00000000000025_questions::Migration),
            Box::new(m00000000000026_plans::Migration),
            Box::new(m00000000000027_input_sequence::Migration),
        ]
    }
}
fn forward_only() -> Result<(), DbErr> {
    Err(DbErr::Migration(
        "Conversation migrations are forward-only; automatic rollback would discard history".into(),
    ))
}
async fn copy(
    manager: &SchemaManager<'_>,
    from: &str,
    to: &str,
    columns: &[&str],
) -> Result<(), DbErr> {
    let selection = Query::select()
        .columns(columns.iter().map(|s| Alias::new(*s)))
        .from(Alias::new(from))
        .to_owned();
    let insert = Query::insert()
        .into_table(Alias::new(to))
        .columns(columns.iter().map(|s| Alias::new(*s)))
        .select_from(selection)
        .map_err(|e| DbErr::Custom(e.to_string()))?
        .to_owned();
    manager
        .get_connection()
        .execute(manager.get_database_backend().build(&insert))
        .await?;
    Ok(())
}
pub(crate) async fn run(connection: &DatabaseConnection) -> Result<()> {
    sqlite::foreign_keys(connection, false).await?;
    let result = upgrade(connection).await;
    let restored = sqlite::foreign_keys(connection, true).await;
    result?;
    restored?;
    Ok(())
}

async fn upgrade(connection: &DatabaseConnection) -> Result<()> {
    let transaction = connection.begin().await?;
    let result: Result<()> = async {
        let version = sqlite::schema_version(&transaction).await?;
        let migrations = Migrator::migrations();
        let current_version: i64 = migrations.len().try_into()?;
        ensure!(
            (0..=current_version).contains(&version),
            "database schema {version} is newer than this Orbit supports"
        );
        Migrator::install(&transaction).await?;
        let applied = seaql_migrations::Entity::find().all(&transaction).await?;
        if !applied.is_empty() {
            let mut names = applied
                .iter()
                .map(|row| row.version.as_str())
                .collect::<Vec<_>>();
            names.sort_unstable();
            let mut expected = migrations
                .iter()
                .take(version as usize)
                .map(|migration| migration.name())
                .collect::<Vec<_>>();
            expected.sort_unstable();
            ensure!(
                names == expected,
                "database migration ledger does not match schema version {version}"
            );
        }
        if applied.is_empty() && version > 0 {
            ensure!(
                version <= LEGACY_VERSION,
                "database migration ledger is missing"
            );
            for migration in migrations.iter().take(version as usize) {
                seaql_migrations::ActiveModel {
                    version: Set(migration.name().into()),
                    applied_at: Set(crate::database::now()),
                }
                .insert(&transaction)
                .await?;
            }
        }
        // Validate the historical baseline before applying future Rust migrations,
        // which may deliberately replace its columns or tables.
        if applied.is_empty() {
            Migrator::up(&transaction, Some((LEGACY_VERSION - version) as u32)).await?;
            let manager = SchemaManager::new(&transaction);
            for &(table, columns) in LEGACY_TABLES {
                ensure!(
                    manager.has_table(table).await?,
                    "database schema missing table {table}"
                );
                for &column in columns {
                    ensure!(
                        manager.has_column(table, column).await?,
                        "database schema missing {table}.{column}"
                    );
                }
            }
        }
        Migrator::up(&transaction, None).await?;
        ensure!(
            sqlite::references_valid(&transaction).await?,
            "database migration contains broken references"
        );
        sqlite::set_schema_version(&transaction, current_version.try_into()?).await?;
        Ok(())
    }
    .await;
    match result {
        Ok(()) => transaction.commit().await.map_err(Into::into),
        Err(error) => {
            transaction.rollback().await?;
            Err(error)
        }
    }
}

const LEGACY_TABLES: &[(&str, &[&str])] = &[
    (
        "sessions",
        &[
            "id",
            "workspace",
            "model",
            "demo",
            "title",
            "updated_at",
            "history",
            "project_id",
            "reasoning_effort",
            "workspace_since_run",
        ],
    ),
    (
        "runs",
        &["id", "session_id", "prompt", "status", "elapsed_ms"],
    ),
    ("events", &["id", "run_id", "payload", "created_at_ms"]),
    ("projects", &["id", "path", "identity", "strong", "legacy"]),
    ("project_paths", &["project_id", "path"]),
    (
        "project_tasks",
        &[
            "id",
            "project_id",
            "parent_id",
            "title",
            "details",
            "status",
            "priority",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "project_memories",
        &[
            "id",
            "project_id",
            "category",
            "title",
            "content",
            "source_session_id",
            "active",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "workspace_undos",
        &["session_id", "run_id", "call_id", "path", "undone_at"],
    ),
    (
        "session_git_baselines",
        &["session_id", "payload", "captured_at"],
    ),
    (
        "project_plugins",
        &[
            "project_id",
            "marketplace_source",
            "plugin_name",
            "package_source",
            "version",
            "enabled_at",
            "marketplace_revision",
        ],
    ),
    (
        "session_memories",
        &[
            "id",
            "session_id",
            "category",
            "title",
            "content",
            "active",
            "created_at",
            "updated_at",
        ],
    ),
    (
        "context_archives",
        &["id", "session_id", "created_at", "content_sha256"],
    ),
    (
        "context_archive_messages",
        &["archive_id", "ordinal", "payload"],
    ),
    ("execution_checkpoints", &["run_id", "payload"]),
    ("session_workspace_baselines", &["session_id", "payload"]),
    (
        "session_messages",
        &["id", "session_id", "run_id", "ordinal", "payload", "sha256"],
    ),
    ("session_log_gaps", &["session_id", "reason"]),
    (
        "compaction_progress",
        &["session_id", "source_fingerprint", "payload", "updated_at"],
    ),
    ("tui_tabs", &["position", "session_id"]),
    (
        "tui_session_state",
        &["session_id", "permission_mode", "run_id", "runtime"],
    ),
    ("tui_file_grants", &["session_id", "path"]),
    (
        "agent_invocations",
        &[
            "id",
            "parent_session_id",
            "parent_run_id",
            "child_session_id",
            "call",
            "result",
            "delivered",
        ],
    ),
    (
        "side_questions",
        &["id", "session_id", "payload", "created_at"],
    ),
    (
        "workspace_requests",
        &["session_id", "request_id", "fingerprint", "error"],
    ),
    (
        "agent_sessions",
        &["session_id", "parent_session_id", "role", "task"],
    ),
    (
        "user_inputs",
        &["id", "session_id", "request_id", "payload"],
    ),
    ("session_plans", &["session_id", "generation", "payload"]),
    (
        "agent_messages",
        &["id", "session_id", "content", "received"],
    ),
];
