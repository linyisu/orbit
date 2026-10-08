ALTER TABLE context_archives ADD COLUMN content_sha256 TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS context_archives_content
               ON context_archives(session_id,content_sha256)
               WHERE content_sha256 IS NOT NULL;
             CREATE TABLE IF NOT EXISTS compaction_progress (
                 session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                 source_fingerprint TEXT NOT NULL,
                 payload TEXT NOT NULL,
                 updated_at INTEGER NOT NULL DEFAULT (unixepoch()),
                 PRIMARY KEY(session_id,source_fingerprint)
             );
             PRAGMA user_version=15;
