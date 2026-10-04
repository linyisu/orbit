//! Ordered SQLite migrations. Historical version numbers are persistent IDs.
use anyhow::{Result, ensure};
use rusqlite::Connection;

const CURRENT_SCHEMA_VERSION: u32 = 22;

pub(crate) fn migrate(connection: &Connection) -> Result<()> {
    connection.execute_batch("BEGIN IMMEDIATE;")?;
    let version: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    ensure!(
        version <= CURRENT_SCHEMA_VERSION,
        "database schema {version} is newer than this Orbit supports"
    );

    if version == 0 {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY, workspace TEXT NOT NULL, model TEXT NOT NULL,
            demo INTEGER NOT NULL, title TEXT NOT NULL DEFAULT '',
            updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
            history TEXT NOT NULL DEFAULT '[]'
        );
        CREATE TABLE IF NOT EXISTS runs (
            id INTEGER PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
            prompt TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'running'
                CHECK(status IN ('running','completed','failed','cancelled','interrupted')),
            elapsed_ms INTEGER
        );
        CREATE INDEX IF NOT EXISTS runs_session ON runs(session_id, id);
        CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY, run_id INTEGER NOT NULL REFERENCES runs(id),
            payload TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS events_run ON events(run_id, id);
        PRAGMA user_version = 1;",
        )?;
    }

    if version < 2 {
        crate::projects::migrate(connection)?;
    }

    if version == 0 {
        connection.execute_batch("PRAGMA user_version=3;")?;
    } else if version < 3 {
        connection.execute_batch(
            "ALTER TABLE runs ADD COLUMN elapsed_ms INTEGER;
             PRAGMA user_version=3;",
        )?;
    }

    if version < 4 {
        connection.execute_batch(
            "ALTER TABLE sessions ADD COLUMN reasoning_effort TEXT;
             PRAGMA user_version=4;",
        )?;
    }

    if version < 5 {
        connection.execute_batch(
            "CREATE TABLE project_tasks (
                id INTEGER PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                parent_id INTEGER REFERENCES project_tasks(id) ON DELETE SET NULL,
                title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 512),
                details TEXT NOT NULL DEFAULT '' CHECK(length(details) <= 8192),
                status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','in_progress','blocked','completed','cancelled')),
                priority INTEGER NOT NULL DEFAULT 0 CHECK(priority BETWEEN -100 AND 100),
                created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                updated_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE INDEX project_tasks_project_status ON project_tasks(project_id,status,priority DESC,id);
            CREATE TABLE project_memories (
                id INTEGER PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                category TEXT NOT NULL CHECK(category IN ('constraint','decision','fact','preference')),
                title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 512),
                content TEXT NOT NULL CHECK(length(content) BETWEEN 1 AND 8192),
                source_session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
                active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
                created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                updated_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE INDEX project_memories_project_active ON project_memories(project_id,active,updated_at DESC,id DESC);
            PRAGMA user_version=5;",
        )?;
    }

    if version < 6 {
        connection.execute_batch(
            "ALTER TABLE events ADD COLUMN created_at_ms INTEGER;
             PRAGMA user_version=6;",
        )?;
    }

    if version < 7 {
        connection.execute_batch(
            "CREATE TABLE workspace_undos (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
                call_id TEXT NOT NULL,
                path TEXT NOT NULL,
                undone_at INTEGER NOT NULL DEFAULT (unixepoch()),
                PRIMARY KEY(session_id,run_id,call_id,path)
            );
            CREATE INDEX workspace_undos_session ON workspace_undos(session_id,run_id);
            PRAGMA user_version=7;",
        )?;
    }

    if version < 8 {
        connection.execute_batch(
            "CREATE TABLE session_git_baselines (
                session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                payload TEXT NOT NULL,
                captured_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            PRAGMA user_version=8;",
        )?;
    }

    if version < 9 {
        connection.execute_batch(
            "CREATE TABLE project_plugins (
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                marketplace_source TEXT NOT NULL CHECK(length(marketplace_source) BETWEEN 1 AND 4096),
                plugin_name TEXT NOT NULL CHECK(length(plugin_name) BETWEEN 1 AND 256),
                package_source TEXT NOT NULL CHECK(length(package_source) BETWEEN 1 AND 4096),
                version TEXT CHECK(version IS NULL OR length(version) BETWEEN 1 AND 256),
                enabled_at INTEGER NOT NULL DEFAULT (unixepoch()),
                PRIMARY KEY(project_id, marketplace_source, plugin_name)
            );
            CREATE INDEX project_plugins_project_enabled ON project_plugins(project_id,enabled_at DESC,plugin_name);
            PRAGMA user_version=9;",
        )?;
    }

    if version < 10 {
        // Project memories from older Orbit builds were unsafe: every
        // session for a project received them. Preserve the legacy
        // table for auditability, but migrate only records with a
        // known owner or exactly one possible owner. Ambiguous
        // records must never leak into a new session.
        connection.execute_batch(
            "CREATE TABLE session_memories (
                id INTEGER PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                category TEXT NOT NULL CHECK(category IN ('constraint','decision','fact','preference')),
                title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 512),
                content TEXT NOT NULL CHECK(length(content) BETWEEN 1 AND 8192),
                active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
                created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                updated_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE INDEX session_memories_session_active ON session_memories(session_id,active,updated_at DESC,id DESC);
            INSERT INTO session_memories(session_id,category,title,content,active,created_at,updated_at)
            SELECT memories.source_session_id, memories.category, memories.title, memories.content,
                   memories.active, memories.created_at, memories.updated_at
            FROM project_memories AS memories
            JOIN sessions AS sessions
              ON sessions.id=memories.source_session_id
             AND sessions.project_id=memories.project_id
            WHERE memories.source_session_id IS NOT NULL
            UNION ALL
            SELECT sessions.id, memories.category, memories.title, memories.content,
                   memories.active, memories.created_at, memories.updated_at
            FROM project_memories AS memories
            JOIN sessions AS sessions ON sessions.project_id=memories.project_id
            WHERE memories.source_session_id IS NULL
              AND 1=(SELECT count(*) FROM sessions AS scoped
                     WHERE scoped.project_id=memories.project_id);
            PRAGMA user_version=10;",
        )?;
    }

    if version < 11 {
        connection.execute_batch(
            "CREATE TABLE context_archives (
                id INTEGER PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE INDEX context_archives_session ON context_archives(session_id,id);
            CREATE TABLE context_archive_messages (
                archive_id INTEGER NOT NULL REFERENCES context_archives(id) ON DELETE CASCADE,
                ordinal INTEGER NOT NULL,
                payload TEXT NOT NULL,
                PRIMARY KEY(archive_id,ordinal)
            );
            PRAGMA user_version=11;",
        )?;
    }

    if version < 12 {
        connection.execute_batch("CREATE TABLE execution_checkpoints(run_id INTEGER PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE, payload TEXT NOT NULL); PRAGMA user_version=12;")?;
    }

    if version < 13 {
        connection.execute_batch("CREATE TABLE session_workspace_baselines(session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,payload TEXT NOT NULL); PRAGMA user_version=13;")?;
    }

    if version < 14 {
        connection.execute_batch(
            "CREATE TABLE session_messages (
                id INTEGER PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
                ordinal INTEGER NOT NULL,
                payload TEXT NOT NULL,
                sha256 TEXT NOT NULL,
                UNIQUE(run_id, ordinal)
            );
            CREATE INDEX session_messages_session ON session_messages(session_id,id);
            CREATE TABLE session_log_gaps (
                session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                reason TEXT NOT NULL
            );
            INSERT INTO session_log_gaps(session_id,reason)
            SELECT id,'session predates the canonical message log'
            FROM sessions;
            PRAGMA user_version=14;",
        )?;
    }

    if version < 15 {
        let has_content_sha256: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('context_archives') WHERE name='content_sha256')",
            [],
            |row| row.get(0),
        )?;

        if !has_content_sha256 {
            connection
                .execute_batch("ALTER TABLE context_archives ADD COLUMN content_sha256 TEXT;")?;
        }

        connection.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS context_archives_content
               ON context_archives(session_id,content_sha256)
               WHERE content_sha256 IS NOT NULL;
             CREATE TABLE IF NOT EXISTS compaction_progress (
                 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 source_fingerprint TEXT NOT NULL,
                 payload TEXT NOT NULL,
                 updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
                 PRIMARY KEY(session_id,source_fingerprint)
             );
             PRAGMA user_version=15;",
        )?;
    }

    if version < 16 {
        let has_revision: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('project_plugins') WHERE name='marketplace_revision')",
            [],
            |row| row.get(0),
        )?;

        if !has_revision {
            connection.execute_batch(
                "ALTER TABLE project_plugins ADD COLUMN marketplace_revision TEXT;",
            )?;
        }

        connection.execute_batch("PRAGMA user_version=16;")?;
    }

    if version < 17 {
        let has_boundary: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('sessions') WHERE name='workspace_since_run')",
            [], |row| row.get(0),
        )?;

        if !has_boundary {
            connection.execute_batch(
                "ALTER TABLE sessions ADD COLUMN workspace_since_run INTEGER NOT NULL DEFAULT 0;",
            )?;
        }

        connection.execute_batch("PRAGMA user_version=17;")?;
    }

    if version < 18 {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS tui_tabs (
            position INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE
        ); PRAGMA user_version=18;",
        )?;
    }

    if version < 19 {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS tui_session_state (
            session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
            permission_mode TEXT NOT NULL DEFAULT 'auto',
            run_id INTEGER REFERENCES runs(id) ON DELETE CASCADE,
            runtime TEXT
        ); PRAGMA user_version=19;",
        )?;
    }

    // Early schema 19 databases contain presentation state but no file grants.
    // Keep this a separate versioned migration, including for existing grants.
    if version < 20 {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS tui_file_grants (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            path TEXT NOT NULL,
            PRIMARY KEY(session_id,path)
        ); PRAGMA user_version=20;",
        )?;
    }

    if version < 21 {
        crate::subagents::migrate(connection)?;
    }
    if version < 22 {
        crate::side_questions::migrate(connection)?;
    }
    connection.execute_batch("COMMIT;")?;

    Ok(())
}
