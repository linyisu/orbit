CREATE TABLE session_workspace_baselines(session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,payload TEXT NOT NULL); PRAGMA user_version=13;
