CREATE TABLE IF NOT EXISTS user_inputs (
        id INTEGER PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        request_id TEXT NOT NULL,
        payload TEXT NOT NULL,
        UNIQUE(session_id,request_id)
    ); CREATE INDEX IF NOT EXISTS user_inputs_session ON user_inputs(session_id,id); PRAGMA user_version=25;
