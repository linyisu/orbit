CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY, workspace TEXT NOT NULL, model TEXT NOT NULL,
            demo INTEGER NOT NULL, title TEXT NOT NULL DEFAULT '',
            updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
            history TEXT NOT NULL DEFAULT '[]'
        );
        CREATE TABLE IF NOT EXISTS runs (
            id INTEGER PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
            prompt TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'running'
                CHECK(status IN ('running','completed','failed','cancelled','interrupted'))
        );
        CREATE INDEX IF NOT EXISTS runs_session ON runs(session_id, id);
        CREATE TABLE IF NOT EXISTS events (
            id INTEGER PRIMARY KEY, run_id INTEGER NOT NULL REFERENCES runs(id),
            payload TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS events_run ON events(run_id, id);
        PRAGMA user_version = 1;
