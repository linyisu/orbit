CREATE TABLE IF NOT EXISTS tui_session_state (
            session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
            permission_mode TEXT NOT NULL DEFAULT 'auto',
            run_id INTEGER REFERENCES runs(id) ON DELETE CASCADE,
            runtime TEXT
        ); PRAGMA user_version=19;
