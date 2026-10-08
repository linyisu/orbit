CREATE TABLE workspace_undos (
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                run_id INTEGER NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
                call_id TEXT NOT NULL,
                path TEXT NOT NULL,
                undone_at INTEGER NOT NULL DEFAULT (unixepoch()),
                PRIMARY KEY(session_id,run_id,call_id,path)
            );
            CREATE INDEX workspace_undos_session ON workspace_undos(session_id,run_id);
            PRAGMA user_version=7;
