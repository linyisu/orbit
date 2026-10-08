CREATE TABLE IF NOT EXISTS side_questions (
        id INTEGER PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        payload TEXT NOT NULL,
        created_at INTEGER NOT NULL DEFAULT (unixepoch())
    );
    CREATE INDEX IF NOT EXISTS side_questions_session ON side_questions(session_id,id);
    PRAGMA user_version=22;
