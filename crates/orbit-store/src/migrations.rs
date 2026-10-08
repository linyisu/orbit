//! Versioned migrations and legacy adoption share one transaction.
//!
//! The 27 historical SQL steps are frozen compatibility artifacts. Append future
//! changes as Rust `MigrationTrait` implementations in `Migrator::migrations()`;
//! never edit a released step. The runner synchronizes `user_version` with the
//! ledger so older Orbit executables reject schemas they cannot understand.
use anyhow::{Result, ensure};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, Set, Statement,
    TransactionTrait,
};
use sea_orm_migration::{prelude::*, seaql_migrations};

const HISTORICAL: &[(&str, &str)] = &[
    (
        "00000000000001_sessions",
        include_str!("../migrations/00000000000001_sessions/up.sql"),
    ),
    (
        "00000000000002_projects",
        include_str!("../migrations/00000000000002_projects/up.sql"),
    ),
    (
        "00000000000003_run_duration",
        include_str!("../migrations/00000000000003_run_duration/up.sql"),
    ),
    (
        "00000000000004_reasoning",
        include_str!("../migrations/00000000000004_reasoning/up.sql"),
    ),
    (
        "00000000000005_tasks_memories",
        include_str!("../migrations/00000000000005_tasks_memories/up.sql"),
    ),
    (
        "00000000000006_event_time",
        include_str!("../migrations/00000000000006_event_time/up.sql"),
    ),
    (
        "00000000000007_undo",
        include_str!("../migrations/00000000000007_undo/up.sql"),
    ),
    (
        "00000000000008_git_baseline",
        include_str!("../migrations/00000000000008_git_baseline/up.sql"),
    ),
    (
        "00000000000009_plugins",
        include_str!("../migrations/00000000000009_plugins/up.sql"),
    ),
    (
        "00000000000010_session_memory",
        include_str!("../migrations/00000000000010_session_memory/up.sql"),
    ),
    (
        "00000000000011_archives",
        include_str!("../migrations/00000000000011_archives/up.sql"),
    ),
    (
        "00000000000012_checkpoints",
        include_str!("../migrations/00000000000012_checkpoints/up.sql"),
    ),
    (
        "00000000000013_workspace_baseline",
        include_str!("../migrations/00000000000013_workspace_baseline/up.sql"),
    ),
    (
        "00000000000014_message_log",
        include_str!("../migrations/00000000000014_message_log/up.sql"),
    ),
    (
        "00000000000015_compaction",
        include_str!("../migrations/00000000000015_compaction/up.sql"),
    ),
    (
        "00000000000016_plugin_revision",
        include_str!("../migrations/00000000000016_plugin_revision/up.sql"),
    ),
    (
        "00000000000017_workspace_boundary",
        include_str!("../migrations/00000000000017_workspace_boundary/up.sql"),
    ),
    (
        "00000000000018_tabs",
        include_str!("../migrations/00000000000018_tabs/up.sql"),
    ),
    (
        "00000000000019_runtime",
        include_str!("../migrations/00000000000019_runtime/up.sql"),
    ),
    (
        "00000000000020_file_grants",
        include_str!("../migrations/00000000000020_file_grants/up.sql"),
    ),
    (
        "00000000000021_subagents",
        include_str!("../migrations/00000000000021_subagents/up.sql"),
    ),
    (
        "00000000000022_side_questions",
        include_str!("../migrations/00000000000022_side_questions/up.sql"),
    ),
    (
        "00000000000023_request_receipts",
        include_str!("../migrations/00000000000023_request_receipts/up.sql"),
    ),
    (
        "00000000000024_subagent_parent",
        include_str!("../migrations/00000000000024_subagent_parent/up.sql"),
    ),
    (
        "00000000000025_questions",
        include_str!("../migrations/00000000000025_questions/up.sql"),
    ),
    (
        "00000000000026_plans",
        include_str!("../migrations/00000000000026_plans/up.sql"),
    ),
    (
        "00000000000027_input_sequence",
        include_str!("../migrations/00000000000027_input_sequence/up.sql"),
    ),
];
const LEGACY_VERSION: i64 = 27;

pub(crate) struct Migrator;
#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        HISTORICAL
            .iter()
            .map(|&(name, sql)| Box::new(Historical { name, sql }) as Box<dyn MigrationTrait>)
            .collect()
    }
}
struct Historical {
    name: &'static str,
    sql: &'static str,
}
impl MigrationName for Historical {
    fn name(&self) -> &str {
        self.name
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Historical {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let mut sql = self.sql.to_owned();
        // Released pre-versioned patches added these columns early. Retain their data.
        for (version, table, column, ddl) in [
            (
                15,
                "context_archives",
                "content_sha256",
                "ALTER TABLE context_archives ADD COLUMN content_sha256 TEXT;",
            ),
            (
                16,
                "project_plugins",
                "marketplace_revision",
                "ALTER TABLE project_plugins ADD COLUMN marketplace_revision TEXT;",
            ),
            (
                17,
                "sessions",
                "workspace_since_run",
                "ALTER TABLE sessions ADD COLUMN workspace_since_run INTEGER NOT NULL DEFAULT 0;",
            ),
        ] {
            if self.name.starts_with(&format!("{version:014}_"))
                && manager.has_column(table, column).await?
            {
                sql = sql.replace(ddl, "");
            }
        }
        manager.get_connection().execute_unprepared(&sql).await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration("Historical Orbit migrations are forward-only; automatic rollback would discard conversation data".into()))
    }
}

pub(crate) async fn run(connection: &DatabaseConnection) -> Result<()> {
    connection
        .execute_unprepared("PRAGMA foreign_keys=OFF")
        .await?;
    let result = upgrade(connection).await;
    let restored = connection
        .execute_unprepared("PRAGMA foreign_keys=ON")
        .await;
    result?;
    restored?;
    Ok(())
}

async fn upgrade(connection: &DatabaseConnection) -> Result<()> {
    let transaction = connection.begin().await?;
    let result: Result<()> = async {
        let row = transaction
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA user_version",
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("missing schema version"))?;
        let version: i64 = row.try_get("", "user_version")?;
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
            for &(name, _) in HISTORICAL.iter().take(version as usize) {
                seaql_migrations::ActiveModel {
                    version: Set(name.into()),
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
        let violations = transaction
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "PRAGMA foreign_key_check",
            ))
            .await?;
        ensure!(
            violations.is_empty(),
            "database migration contains broken references"
        );
        transaction
            .execute_unprepared(&format!("PRAGMA user_version={current_version}"))
            .await?;
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
