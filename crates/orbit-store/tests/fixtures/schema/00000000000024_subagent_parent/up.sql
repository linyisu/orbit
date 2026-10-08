CREATE TABLE agent_sessions_v24 (
                session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
                parent_session_id TEXT REFERENCES sessions(id) ON DELETE SET NULL,
                role TEXT NOT NULL CHECK(role IN ('explore','review','develop')), task TEXT NOT NULL
            );
            INSERT INTO agent_sessions_v24 SELECT session_id,parent_session_id,role,task FROM agent_sessions;
            DROP TABLE agent_sessions;
            ALTER TABLE agent_sessions_v24 RENAME TO agent_sessions;
            CREATE INDEX agent_sessions_parent ON agent_sessions(parent_session_id);
            PRAGMA user_version=24;
