CREATE TABLE session_git_baselines (
                session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                payload TEXT NOT NULL,
                captured_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            PRAGMA user_version=8;
