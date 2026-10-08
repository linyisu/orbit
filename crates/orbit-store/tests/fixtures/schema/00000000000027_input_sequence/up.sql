CREATE TABLE session_inputs_v27 (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            content TEXT NOT NULL, received INTEGER NOT NULL DEFAULT 0 CHECK(received IN (0,1))
        );
        INSERT INTO session_inputs_v27 SELECT id,session_id,content,received FROM agent_messages;
        DROP TABLE agent_messages;
        ALTER TABLE session_inputs_v27 RENAME TO agent_messages;
        CREATE INDEX agent_messages_pending ON agent_messages(session_id,received);
        PRAGMA user_version=27;
