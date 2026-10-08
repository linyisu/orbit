ALTER TABLE sessions ADD COLUMN workspace_since_run INTEGER NOT NULL DEFAULT 0;
PRAGMA user_version=17;
