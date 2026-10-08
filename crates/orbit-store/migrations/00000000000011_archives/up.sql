CREATE TABLE context_archives (
                id INTEGER PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                created_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE INDEX context_archives_session ON context_archives(session_id,id);
            CREATE TABLE context_archive_messages (
                archive_id INTEGER NOT NULL REFERENCES context_archives(id) ON DELETE CASCADE,
                ordinal INTEGER NOT NULL,
                payload TEXT NOT NULL,
                PRIMARY KEY(archive_id,ordinal)
            );
            PRAGMA user_version=11;
