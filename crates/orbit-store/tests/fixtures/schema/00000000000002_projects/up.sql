CREATE TABLE projects (
        id TEXT PRIMARY KEY, path TEXT, identity TEXT, strong INTEGER NOT NULL DEFAULT 0,
        legacy INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX projects_identity ON projects(identity);
    CREATE TABLE project_paths (project_id TEXT NOT NULL REFERENCES projects(id), path TEXT NOT NULL,
        PRIMARY KEY(project_id,path));
    ALTER TABLE sessions ADD COLUMN project_id TEXT REFERENCES projects(id);
    INSERT INTO projects(id,path,legacy) SELECT lower(hex(randomblob(16))), workspace, 1 FROM sessions GROUP BY workspace;
    INSERT INTO project_paths(project_id,path) SELECT id,path FROM projects;
    UPDATE sessions SET project_id=(SELECT id FROM projects WHERE path=sessions.workspace);
    CREATE INDEX sessions_project ON sessions(project_id);
    PRAGMA user_version=2;
