use sea_orm::sea_query::Expr;
use std::path::Path;

use crate::entities::*;
use anyhow::{Context, Result, ensure};
use sea_orm::*;

use crate::{
    project::{Directory, Project, same_directory_identity},
    store::Store,
};

async fn affected_sessions<C: ConnectionTrait>(
    conn: &C,
    id: &str,
    path: &str,
) -> Result<Vec<String>> {
    Ok(sessions::Entity::find()
        .select_only()
        .column(sessions::Column::Id)
        .join(JoinType::InnerJoin, sessions::Relation::Project.def())
        .filter(
            Condition::any()
                .add(projects::Column::Id.eq(id))
                .add(projects::Column::Path.eq(path)),
        )
        .order_by_asc(sessions::Column::Id)
        .into_tuple()
        .all(conn)
        .await?)
}
async fn record_path<C: ConnectionTrait>(conn: &C, id: &str, path: &str) -> Result<()> {
    project_paths::Entity::insert(project_paths::ActiveModel {
        project_id: Set(id.to_owned()),
        path: Set(path.to_owned()),
    })
    .on_conflict(
        sea_query::OnConflict::columns([
            project_paths::Column::ProjectId,
            project_paths::Column::Path,
        ])
        .do_nothing()
        .to_owned(),
    )
    .do_nothing()
    .exec(conn)
    .await?;
    Ok(())
}
async fn update_directory<C: ConnectionTrait>(
    conn: &C,
    id: &str,
    path: &str,
    identity: Option<String>,
    strong: bool,
) -> Result<()> {
    projects::Entity::update_many()
        .col_expr(projects::Column::Path, Expr::value(path))
        .col_expr(projects::Column::Identity, Expr::value(identity))
        .col_expr(projects::Column::Strong, Expr::value(strong))
        .col_expr(projects::Column::Legacy, Expr::value(false))
        .filter(projects::Column::Id.eq(id))
        .exec(conn)
        .await?;
    record_path(conn, id, path).await
}
fn can_complete_identity(row: &projects::Model, directory: &Directory) -> bool {
    directory.strong
        && !row.strong
        && row.path.as_deref() == Some(&directory.path)
        && row
            .identity
            .as_deref()
            .and_then(|s| s.strip_suffix(":unknown"))
            .is_some_and(|previous| {
                directory
                    .identity
                    .as_deref()
                    .and_then(|s| s.rsplit_once(':'))
                    .map(|(object, _)| object)
                    == Some(previous)
            })
}

async fn find_project(
    conn: &impl ConnectionTrait,
    directory: &Directory,
) -> Result<Option<String>> {
    let mut matches = Vec::new();
    let known = projects::Entity::find()
        .filter(projects::Column::Path.eq(&directory.path))
        .one(conn)
        .await?;
    if directory.strong {
        let same_path: std::collections::HashSet<String> = project_paths::Entity::find()
            .filter(project_paths::Column::Path.eq(&directory.path))
            .all(conn)
            .await?
            .into_iter()
            .map(|r| r.project_id)
            .collect();
        let rows = projects::Entity::find()
            .filter(
                Condition::any()
                    .add(projects::Column::Identity.eq(directory.identity.clone()))
                    .add(projects::Column::Path.eq(&directory.path))
                    .add(projects::Column::Id.is_in(same_path.iter().cloned())),
            )
            .all(conn)
            .await?;
        for mut row in rows {
            // Read-only lookup uses the completed identity without changing stored rows.
            if can_complete_identity(&row, directory) {
                row.identity = directory.identity.clone();
                row.strong = true;
            }
            let Some(identity) = row.identity else {
                continue;
            };
            let exact = directory.identity.as_deref() == Some(identity.as_str());
            if row.strong
                && (exact
                    || (same_path.contains(&row.id)
                        && same_directory_identity(
                            &identity,
                            directory.identity.as_deref().unwrap_or_default(),
                        )))
            {
                let count = sessions::Entity::find()
                    .filter(sessions::Column::ProjectId.eq(&row.id))
                    .count(conn)
                    .await?;
                matches.push((row.id, count, exact));
            }
        }
    }
    matches.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(&b.0))
    });
    Ok(matches.into_iter().next().map(|(id, _, _)| id).or_else(|| {
        known
            .filter(|row| row.legacy || row.identity == directory.identity)
            .map(|row| row.id)
    }))
}

impl Store {
    pub async fn projects(&self) -> Result<Vec<Project>> {
        self.read_access(|conn| async move {
            Ok(projects::Entity::find()
                .order_by_asc(projects::Column::Path)
                .order_by_asc(projects::Column::Id)
                .all(&conn)
                .await?
                .into_iter()
                .map(|row| Project {
                    id: row.id,
                    path: row.path,
                })
                .collect())
        })
        .await
    }

    /// Resolve existing project identity without registering paths or creating rows.
    pub async fn project_at(&self, path: impl AsRef<Path>) -> Result<Option<Project>> {
        let directory = Directory::read(path.as_ref())?;
        self.read_access(move |conn| async move {
            let tx = conn.begin().await?;
            Ok(find_project(&tx, &directory).await?.map(|id| Project {
                id,
                path: Some(directory.path),
            }))
        })
        .await
    }

    pub async fn resolve_project(&self, path: impl AsRef<Path>) -> Result<Project> {
        let directory = Directory::read(path.as_ref())?;

        self.write_access(move |conn| async move {
            let tx = conn.begin().await?;
            // On a real write, preserve equivalence with an older partial-identity row
            // even when another project holds the established conversation history.
            if let Some(row) = projects::Entity::find()
                .filter(projects::Column::Path.eq(&directory.path))
                .one(&tx)
                .await?
                && can_complete_identity(&row, &directory)
            {
                let mut row = row.into_active_model();
                row.identity = Set(directory.identity.clone());
                row.strong = Set(true);
                row.update(&tx).await?;
            }
            let id = match find_project(&tx, &directory).await? {
                Some(id) => id,
                None => new_project(&tx).await?,
            };
            // Reusing a path never merges its previous conversation history.
            projects::Entity::update_many()
                .col_expr(projects::Column::Path, Expr::value(Option::<String>::None))
                .filter(projects::Column::Path.eq(&directory.path))
                .filter(projects::Column::Id.ne(&id))
                .exec(&tx)
                .await?;
            update_directory(
                &tx,
                &id,
                &directory.path,
                directory.identity,
                directory.strong,
            )
            .await?;
            tx.commit().await?;

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
                .read_access(move |conn| async move {
                    affected_sessions(&conn, &scan_id, &scan_path).await
                })
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
                .write_access(move |conn| async move {
                    let tx = conn.begin().await?;
                    let actual = affected_sessions(&tx, &link_id, &link_path).await?;

                    if actual != expected {
                        return Ok(None);
                    }

                    let old = projects::Entity::find_by_id(&link_id).one(&tx).await?.context("project not found")?.path;

                    if let Some(old) = old.as_deref().filter(|old| *old != link_path) {
                        ensure!(
                            matches!(std::fs::metadata(old), Err(error) if error.kind() == std::io::ErrorKind::NotFound),
                            "old project directory still exists or cannot be checked; refusing to merge a copy"
                        );
                    }

                    let others = runs::Entity::find()
                        .join(JoinType::InnerJoin, runs::Relation::Session.def())
                        .join(JoinType::InnerJoin, sessions::Relation::Project.def())
                        .filter(projects::Column::Path.eq(&link_path))
                        .filter(projects::Column::Id.ne(&link_id))
                        .count(&tx).await?;
                    ensure!(others == 0, "this directory already has another project's conversations; refusing to merge histories");
                    projects::Entity::update_many()
                        .col_expr(projects::Column::Path, Expr::value(Option::<String>::None))
                        .col_expr(projects::Column::Identity, Expr::value(Option::<String>::None))
                        .col_expr(projects::Column::Strong, Expr::value(false))
                        .filter(projects::Column::Path.eq(&link_path))
                        .filter(projects::Column::Id.ne(&link_id))
                        .exec(&tx).await?;
                    update_directory(&tx, &link_id, &link_path, identity, strong).await?;
                    tx.commit().await?;

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

        self.read_access(move |conn| async move {
            let tx = conn.begin().await?;
            let own = projects::Entity::find_by_id(&project_id)
                .one(&tx)
                .await?
                .context("project not found")?;
            let Some(identity) = own.identity.filter(|_| own.strong) else {
                return Ok(vec![project_id]);
            };
            let paths = project_paths::Entity::find()
                .select_only()
                .column(project_paths::Column::Path)
                .filter(project_paths::Column::ProjectId.eq(&project_id))
                .into_query();
            let ids = project_paths::Entity::find()
                .select_only()
                .column(project_paths::Column::ProjectId)
                .filter(project_paths::Column::Path.in_subquery(paths))
                .into_query();
            let rows = projects::Entity::find()
                .filter(projects::Column::Id.in_subquery(ids))
                .all(&tx)
                .await?;
            let mut ids = rows
                .into_iter()
                .filter(|row| {
                    row.strong
                        && row
                            .identity
                            .as_deref()
                            .is_some_and(|value| same_directory_identity(&identity, value))
                })
                .map(|row| row.id)
                .collect::<Vec<_>>();
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
    use rusqlite::params;

    // Construct legacy identity states independently of the ORM being tested.
    async fn fixture(
        store: &Store,
        change: impl FnOnce(&rusqlite::Connection) -> Result<()> + Send + 'static,
    ) -> Result<()> {
        let path = store.path().to_owned();
        tokio::task::spawn_blocking(move || change(&rusqlite::Connection::open(path)?)).await?
    }

    #[tokio::test]
    async fn established_history_survives_a_duplicate_without_birth_time() {
        let temporary = tempfile::tempdir().unwrap();
        let store = Store::open(temporary.path().join("orbit.db"))
            .await
            .unwrap();
        let original = store.resolve_project(temporary.path()).await.unwrap();
        drop(
            store
                .create(temporary.path().display().to_string(), "model".into())
                .await
                .unwrap(),
        );
        let id = original.id.clone();
        let path = temporary.path().display().to_string();
        fixture(&store, move |conn| {
            let identity: String =
                conn.query_row("SELECT identity FROM projects WHERE id=?1", [&id], |row| {
                    row.get(0)
                })?;
            assert!(!identity.ends_with(":unknown"));
            let (object, _) = identity.rsplit_once(':').unwrap();
            conn.execute("UPDATE projects SET path=NULL WHERE id=?1", [&id])?;
            conn.execute(
                "INSERT INTO projects(id,path,identity,strong) VALUES('weak',?1,?2,0)",
                params![path, format!("{object}:unknown")],
            )?;
            conn.execute(
                "INSERT INTO project_paths(project_id,path) VALUES('weak',?1)",
                [&path],
            )?;
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(
            store.resolve_project(temporary.path()).await.unwrap().id,
            original.id
        );
        assert_eq!(store.projects().await.unwrap().len(), 2);
        let related = store.equivalent_project_ids(&original.id).await.unwrap();
        assert!(related.contains(&"weak".to_owned()));
    }

    #[tokio::test]
    async fn completing_a_weak_identity_keeps_its_history_without_adopting_other_inodes() {
        for same_inode in [true, false] {
            let temporary = tempfile::tempdir().unwrap();
            let store = Store::open(temporary.path().join("orbit.db"))
                .await
                .unwrap();
            let original = store.resolve_project(temporary.path()).await.unwrap();
            let mut writer = store
                .create(temporary.path().display().to_string(), "model".into())
                .await
                .unwrap();
            writer
                .begin("saved conversation".into(), "model".into())
                .await
                .unwrap();
            writer.finish("failed", Vec::new(), None).await.unwrap();
            drop(writer);
            let id = original.id.clone();
            fixture(&store, move |conn| {
                let identity: String =
                    conn.query_row("SELECT identity FROM projects WHERE id=?1", [&id], |row| {
                        row.get(0)
                    })?;
                let mut fields: Vec<_> = identity.split(':').map(str::to_owned).collect();
                if !same_inode {
                    fields[2] = "0".into();
                }
                fields[3] = "unknown".into();
                conn.execute(
                    "UPDATE projects SET identity=?2,strong=0 WHERE id=?1",
                    params![id, fields.join(":")],
                )?;
                Ok(())
            })
            .await
            .unwrap();
            let resolved = store.resolve_project(temporary.path()).await.unwrap();
            assert_eq!(resolved.id == original.id, same_inode);
            assert_eq!(store.list_resumable().await.unwrap().len(), 1);
        }
    }

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
                    .create(workspace.display().to_string(), "model".into())
                    .await
                    .unwrap(),
            );
        }

        let original_id = original.id.clone();
        let workspace_text = workspace.display().to_string();
        fixture(&store, move |conn| {
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
                    "INSERT INTO sessions(id,workspace,model,title,project_id) VALUES('new-session',?1,'model','new','duplicate')",
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

async fn new_project<C: ConnectionTrait>(conn: &C) -> Result<String> {
    let id = crate::database::new_id()?;
    projects::ActiveModel {
        id: Set(id.clone()),
        ..Default::default()
    }
    .insert(conn)
    .await?;
    Ok(id)
}
