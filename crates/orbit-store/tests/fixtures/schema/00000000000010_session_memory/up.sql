CREATE TABLE session_memories (
                id INTEGER PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                category TEXT NOT NULL CHECK(category IN ('constraint','decision','fact','preference')),
                title TEXT NOT NULL CHECK(length(title) BETWEEN 1 AND 512),
                content TEXT NOT NULL CHECK(length(content) BETWEEN 1 AND 8192),
                active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
                created_at INTEGER NOT NULL DEFAULT (unixepoch()),
                updated_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE INDEX session_memories_session_active ON session_memories(session_id,active,updated_at DESC,id DESC);
            INSERT INTO session_memories(session_id,category,title,content,active,created_at,updated_at)
            SELECT memories.source_session_id, memories.category, memories.title, memories.content,
                   memories.active, memories.created_at, memories.updated_at
            FROM project_memories AS memories
            JOIN sessions AS sessions
              ON sessions.id=memories.source_session_id
             AND sessions.project_id=memories.project_id
            WHERE memories.source_session_id IS NOT NULL
            UNION ALL
            SELECT sessions.id, memories.category, memories.title, memories.content,
                   memories.active, memories.created_at, memories.updated_at
            FROM project_memories AS memories
            JOIN sessions AS sessions ON sessions.project_id=memories.project_id
            WHERE memories.source_session_id IS NULL
              AND 1=(SELECT count(*) FROM sessions AS scoped
                     WHERE scoped.project_id=memories.project_id);
            PRAGMA user_version=10;
