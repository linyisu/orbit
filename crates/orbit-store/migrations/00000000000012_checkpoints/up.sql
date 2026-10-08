CREATE TABLE execution_checkpoints(run_id INTEGER PRIMARY KEY REFERENCES runs(id) ON DELETE CASCADE, payload TEXT NOT NULL); PRAGMA user_version=12;
