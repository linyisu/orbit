CREATE TABLE project_tasks (
                id INTEGER PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                parent_id INTEGER REFERENCES project_tasks(id) ON DELETE SET NULL,
                title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 512),
                details TEXT NOT NULL DEFAULT '' CHECK(length(details) <= 8192),
                status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','in_progress','blocked','completed','cancelled')),
                priority INTEGER NOT NULL DEFAULT 0 CHECK(priority BETWEEN -100 AND 100),
                created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                updated_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE INDEX project_tasks_project_status ON project_tasks(project_id,status,priority DESC,id);
            CREATE TABLE project_memories (
                id INTEGER PRIMARY KEY,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                category TEXT NOT NULL CHECK(category IN ('constraint','decision','fact','preference')),
                title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 512),
                content TEXT NOT NULL CHECK(length(content) BETWEEN 1 AND 8192),
                source_session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
                active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
                created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                updated_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE INDEX project_memories_project_active ON project_memories(project_id,active,updated_at DESC,id DESC);
            PRAGMA user_version=5;
