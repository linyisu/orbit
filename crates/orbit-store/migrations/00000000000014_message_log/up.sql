CREATE TABLE session_messages (
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
            PRAGMA user_version=14;
