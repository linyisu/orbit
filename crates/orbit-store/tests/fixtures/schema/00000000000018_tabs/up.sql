CREATE TABLE IF NOT EXISTS tui_tabs (
            position INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL UNIQUE REFERENCES sessions(id) ON DELETE CASCADE
        ); PRAGMA user_version=18;
