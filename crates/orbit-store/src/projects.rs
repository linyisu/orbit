use crate::{
    project::{Directory, Project},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use std::path::Path;

pub(crate) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE projects (
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
    PRAGMA user_version=2;")?;
    Ok(())
}

impl Store {
    pub async fn projects(&self) -> Result<Vec<Project>> {
        self.access(|conn| {
            let mut query = conn.prepare("SELECT id,path FROM projects ORDER BY path,id")?;
            Ok(query
                .query_map([], |row| {
                    Ok(Project {
                        id: row.get(0)?,
                        path: row.get(1)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    pub async fn resolve_project(&self, path: impl AsRef<Path>) -> Result<Project> {
        let directory = Directory::read(path.as_ref())?;
        self.access(move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            // Strong identity wins over path: names can be reused by a different directory.
            let mut matches = Vec::new();
            if directory.strong {
                let mut query =
                    tx.prepare("SELECT id FROM projects WHERE identity=?1 AND strong=1")?;
                matches = query
                    .query_map([&directory.identity], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<_>>()?;
            }
            ensure!(
                matches.len() <= 1,
                "project identity is ambiguous; use projects link explicitly"
            );
            let known = tx
                .query_row(
                    "SELECT id,identity,legacy FROM projects WHERE path=?1",
                    [&directory.path],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, bool>(2)?,
                        ))
                    },
                )
                .optional()?;
            let id = if let Some(id) = matches.pop() {
                id
            } else if let Some((id, identity, legacy)) = &known {
                if *legacy || *identity == directory.identity {
                    id.clone()
                } else {
                    new_project(&tx)?
                }
            } else {
                new_project(&tx)?
            };
            // Preserve the previous project and its sessions when its old path is reused.
            tx.execute(
                "UPDATE projects SET path=NULL WHERE path=?1 AND id<>?2",
                params![directory.path, id],
            )?;
            tx.execute(
                "UPDATE projects SET path=?2,identity=?3,strong=?4,legacy=0 WHERE id=?1",
                params![id, directory.path, directory.identity, directory.strong],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO project_paths(project_id,path) VALUES(?1,?2)",
                params![id, directory.path],
            )?;
            tx.commit()?;
            Ok(Project {
                id,
                path: Some(directory.path),
            })
        })
        .await
    }

    /// Explicitly reconnect a moved project. Never merges two populated histories.
    pub async fn link_project(&self, id: &str, path: impl AsRef<Path>) -> Result<Project> {
        let directory = Directory::read(path.as_ref())?;
        let id = id.to_owned();
        let lock_dir = self.path().with_extension("session-locks");
        self.access(move |conn| {
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let old: Option<String> = tx.query_row("SELECT path FROM projects WHERE id=?1", [&id], |r| r.get(0)).optional()?.ok_or_else(|| anyhow::anyhow!("project not found"))?;
            if let Some(old) = old.as_deref().filter(|old| *old != directory.path) {
                ensure!(matches!(std::fs::metadata(old), Err(e) if e.kind() == std::io::ErrorKind::NotFound), "old project directory still exists or cannot be checked; refusing to merge a copy");
            }
            let others: i64 = tx.query_row("SELECT count(*) FROM runs JOIN sessions ON sessions.id=runs.session_id JOIN projects ON projects.id=sessions.project_id WHERE projects.path=?1 AND projects.id<>?2", params![directory.path,id], |r| r.get(0))?;
            ensure!(others == 0, "this directory already has another project's conversations; refusing to merge histories");
            // Hold every affected writer lease through commit; do not interrupt runs.
            let session_ids = {
                let mut query = tx.prepare("SELECT sessions.id FROM sessions JOIN projects ON projects.id=sessions.project_id WHERE projects.id=?1 OR projects.path=?2")?;
                query.query_map(params![id,directory.path], |r| r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?
            };
            std::fs::create_dir_all(&lock_dir)?;
            let mut leases = Vec::new();
            for session in session_ids {
                ensure!(session.len() == 32 && session.bytes().all(|b| b.is_ascii_hexdigit()), "invalid stored session ID");
                let file = crate::store::private_file(&lock_dir.join(session))?;
                file.try_lock().context("project has an open session; close its TUI before linking")?;
                leases.push(file);
            }
            tx.execute("UPDATE projects SET path=NULL,identity=NULL,strong=0 WHERE path=?1 AND id<>?2", params![directory.path,id])?;
            tx.execute("UPDATE projects SET path=?2,identity=?3,strong=?4,legacy=0 WHERE id=?1", params![id,directory.path,directory.identity,directory.strong])?;
            tx.execute("INSERT OR IGNORE INTO project_paths(project_id,path) VALUES(?1,?2)", params![id,directory.path])?;
            tx.commit()?;
            Ok(Project { id, path: Some(directory.path) })
        }).await
    }
}

fn new_project(conn: &rusqlite::Connection) -> Result<String> {
    let id: String = conn.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?;
    conn.execute("INSERT INTO projects(id) VALUES(?1)", [&id])?;
    Ok(id)
}
