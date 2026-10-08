CREATE TABLE IF NOT EXISTS agent_sessions (
        session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
        parent_session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
        role TEXT NOT NULL CHECK(role IN ('explore','review')), task TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS agent_sessions_parent ON agent_sessions(parent_session_id);
    CREATE TABLE IF NOT EXISTS agent_invocations (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        parent_session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        parent_run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
        child_session_id TEXT NOT NULL REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
        call TEXT NOT NULL, result TEXT, delivered INTEGER NOT NULL DEFAULT 0 CHECK(delivered IN (0,1))
    );
    CREATE INDEX IF NOT EXISTS agent_invocations_delivery ON agent_invocations(parent_session_id,delivered);
    CREATE TABLE IF NOT EXISTS agent_messages (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL REFERENCES agent_sessions(session_id) ON DELETE CASCADE,
        content TEXT NOT NULL, received INTEGER NOT NULL DEFAULT 0 CHECK(received IN (0,1))
    );
    PRAGMA user_version=21;
