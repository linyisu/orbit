CREATE TABLE IF NOT EXISTS tui_file_grants (
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            path TEXT NOT NULL,
            PRIMARY KEY(session_id,path)
        ); PRAGMA user_version=20;
