CREATE TABLE IF NOT EXISTS workspace_requests (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                request_id TEXT NOT NULL,
                fingerprint TEXT NOT NULL,
                error TEXT,
                PRIMARY KEY(session_id, request_id)
            ); PRAGMA user_version=23;
