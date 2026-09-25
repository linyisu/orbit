use crate::{
    project::{Directory, Project, same_directory_identity},
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

fn affected_sessions(conn: &rusqlite::Connection, id: &str, path: &str) -> Result<Vec<String>> {
    let mut query = conn.prepare(
        "SELECT sessions.id FROM sessions
         JOIN projects ON projects.id=sessions.project_id
         WHERE projects.id=?1 OR projects.path=?2
         ORDER BY sessions.id",
    )?;
    let sessions = query
        .query_map(params![id, path], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(sessions)
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
            // An unchanged inode and birth time at the same recorded path is
            // still the same project when a mount changes its device number.
            // Prefer the project with the established session history if an
            // older build already created a duplicate for the new device.
            let mut matches = Vec::new();
            if directory.strong {
                let mut query = tx.prepare(
                    "SELECT p.id,p.identity,p.strong,pp.project_id IS NOT NULL,
                                      (SELECT count(*) FROM sessions s WHERE s.project_id=p.id)
                               FROM projects p LEFT JOIN project_paths pp
                                 ON pp.project_id=p.id AND pp.path=?1
                               WHERE p.identity=?2 OR pp.project_id IS NOT NULL",
                )?;
                let rows = query.query_map(params![directory.path, directory.identity], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, bool>(3)?,
                        row.get::<_, i64>(4)?,
                    ))
                })?;
                for row in rows {
                    let (id, identity, strong, same_path, sessions) = row?;
                    let Some(identity) = identity else { continue };
                    let exact = directory.identity.as_deref() == Some(identity.as_str());
                    if strong
                        && (exact
                            || (same_path
                                && same_directory_identity(
                                    &identity,
                                    directory.identity.as_deref().unwrap_or_default(),
                                )))
                    {
                        matches.push((id, sessions, exact));
                    }
                }
            }
            matches.sort_by(|a, b| {
                b.1.cmp(&a.1)
                    .then_with(|| b.2.cmp(&a.2))
                    .then_with(|| a.0.cmp(&b.0))
            });
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
            let id = if let Some((id, _, _)) = matches.into_iter().next() {
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
        for _ in 0..3 {
            let scan_id = id.clone();
            let scan_path = directory.path.clone();
            let session_ids = self
                .read_access(move |conn| affected_sessions(conn, &scan_id, &scan_path))
                .await?;

            // Acquire file leases without holding the database mutex. Acquire
            // takes leases before its final DB update, so the reverse order
            // could incorrectly report an in-flight open as an active TUI.
            let expected = session_ids.clone();
            let lease_dir = lock_dir.clone();
            let leases = tokio::task::spawn_blocking(move || -> Result<Vec<std::fs::File>> {
                std::fs::create_dir_all(&lease_dir)?;
                let mut leases = Vec::with_capacity(session_ids.len());
                for session in session_ids {
                    ensure!(
                        session.len() == 32 && session.bytes().all(|byte| byte.is_ascii_hexdigit()),
                        "invalid stored session ID"
                    );
                    let file = crate::store::private_file(&lease_dir.join(session))?;
                    file.try_lock()
                        .context("project has an open session; close its TUI before linking")?;
                    leases.push(file);
                }
                Ok(leases)
            })
            .await??;

            let link_id = id.clone();
            let link_path = directory.path.clone();
            let identity = directory.identity.clone();
            let strong = directory.strong;
            let linked = self
                .access(move |conn| {
                    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                    let actual = affected_sessions(&tx, &link_id, &link_path)?;
                    if actual != expected {
                        return Ok(None);
                    }

                    let old: Option<String> = tx
                        .query_row("SELECT path FROM projects WHERE id=?1", [&link_id], |row| row.get(0))
                        .optional()?
                        .ok_or_else(|| anyhow::anyhow!("project not found"))?;
                    if let Some(old) = old.as_deref().filter(|old| *old != link_path) {
                        ensure!(
                            matches!(std::fs::metadata(old), Err(error) if error.kind() == std::io::ErrorKind::NotFound),
                            "old project directory still exists or cannot be checked; refusing to merge a copy"
                        );
                    }
                    let others: i64 = tx.query_row(
                        "SELECT count(*) FROM runs JOIN sessions ON sessions.id=runs.session_id JOIN projects ON projects.id=sessions.project_id WHERE projects.path=?1 AND projects.id<>?2",
                        params![link_path, link_id],
                        |row| row.get(0),
                    )?;
                    ensure!(others == 0, "this directory already has another project's conversations; refusing to merge histories");

                    tx.execute("UPDATE projects SET path=NULL,identity=NULL,strong=0 WHERE path=?1 AND id<>?2", params![link_path,link_id])?;
                    tx.execute("UPDATE projects SET path=?2,identity=?3,strong=?4,legacy=0 WHERE id=?1", params![link_id,link_path,identity,strong])?;
                    tx.execute("INSERT OR IGNORE INTO project_paths(project_id,path) VALUES(?1,?2)", params![link_id,link_path])?;
                    tx.commit()?;
                    Ok(Some(Project { id: link_id, path: Some(link_path) }))
                })
                .await?;
            drop(leases);
            if let Some(project) = linked {
                return Ok(project);
            }
        }
        anyhow::bail!("project sessions changed while linking; retry the operation")
    }

    /// IDs proven to be the same directory at a recorded path, including a
    /// duplicate created after the mount's device number changed.
    pub async fn equivalent_project_ids(&self, project_id: &str) -> Result<Vec<String>> {
        let project_id = project_id.to_owned();
        self.access(move |conn| {
            let (identity, strong): (Option<String>, bool) = conn.query_row(
                "SELECT identity,strong FROM projects WHERE id=?1",
                [&project_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let Some(identity) = identity else {
                return Ok(vec![project_id]);
            };
            if !strong {
                return Ok(vec![project_id]);
            }
            let mut query = conn.prepare(
                "SELECT DISTINCT p.id,p.identity,p.strong
                 FROM project_paths source
                 JOIN project_paths other ON other.path=source.path
                 JOIN projects p ON p.id=other.project_id
                 WHERE source.project_id=?1",
            )?;
            let mut ids = Vec::new();
            for row in query.query_map([&project_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, bool>(2)?,
                ))
            })? {
                let (id, candidate, candidate_strong) = row?;
                if candidate_strong
                    && candidate
                        .as_deref()
                        .is_some_and(|value| same_directory_identity(&identity, value))
                {
                    ids.push(id);
                }
            }
            if !ids.contains(&project_id) {
                ids.push(project_id);
            }
            Ok(ids)
        })
        .await
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mount_device_change_recovers_established_project_and_duplicate_sessions() {
        let temporary = tempfile::tempdir().unwrap();
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let store = Store::open(temporary.path().join("orbit.db"))
            .await
            .unwrap();
        let original = store.resolve_project(&workspace).await.unwrap();
        for _ in 0..2 {
            drop(
                store
                    .create(workspace.display().to_string(), "model".into(), false)
                    .await
                    .unwrap(),
            );
        }
        let original_id = original.id.clone();
        let workspace_text = workspace.display().to_string();
        store
            .access(move |conn| {
                let current: String = conn.query_row(
                    "SELECT identity FROM projects WHERE id=?1",
                    [&original_id],
                    |row| row.get(0),
                )?;
                let fields: Vec<_> = current.split(':').collect();
                let previous_mount = format!("linux:999999:{}:{}", fields[2], fields[3]);
                conn.execute(
                    "UPDATE projects SET identity=?1,path=NULL WHERE id=?2",
                    params![previous_mount, original_id],
                )?;
                conn.execute(
                    "INSERT INTO projects(id,path,identity,strong,legacy) VALUES('duplicate',?1,?2,1,0)",
                    params![workspace_text, current],
                )?;
                conn.execute(
                    "INSERT INTO project_paths(project_id,path) VALUES('duplicate',?1)",
                    [&workspace_text],
                )?;
                conn.execute(
                    "INSERT INTO sessions(id,workspace,model,demo,title,project_id) VALUES('new-session',?1,'model',0,'new','duplicate')",
                    [&workspace_text],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let related = store.equivalent_project_ids("duplicate").await.unwrap();
        assert!(related.contains(&original.id));
        assert!(related.contains(&"duplicate".to_owned()));
        assert_eq!(
            store.resolve_project(&workspace).await.unwrap().id,
            original.id
        );
        assert_eq!(store.list().await.unwrap().len(), 3);
    }
}

fn new_project(conn: &rusqlite::Connection) -> Result<String> {
    let id: String = conn.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?;
    conn.execute("INSERT INTO projects(id) VALUES(?1)", [&id])?;
    Ok(id)
}
