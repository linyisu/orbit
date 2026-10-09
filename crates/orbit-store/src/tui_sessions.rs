//! Project-scoped open tabs and inexpensive change detection for TUI readers.
use crate::entities::*;
use anyhow::Result;
use sea_orm::*;

use crate::store::Store;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRevision {
    pub run: i64,
    pub event: i64,
    pub status: String,
    pub title: String,
    pub model: String,
    pub effort: Option<String>,
    pub workspace: String,
}

/// Presentation sampled by the request owner, never inferred from a reader's replay clock.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TuiRuntime {
    pub model: String,
    pub effort: Option<String>,
    pub context_used: usize,
    pub context_limit: usize,
    pub input_limit: usize,
    pub max_output: u32,
    pub elapsed_ms: u64,
    pub sampled_at_ms: u64,
    pub paused: bool,
    pub status: String,
    pub turn: usize,
    pub tool_calls: usize,
}

fn tabs_query() -> Select<tui_tabs::Entity> {
    tui_tabs::Entity::find()
        .join(JoinType::InnerJoin, tui_tabs::Relation::Session.def())
        .filter(
            sessions::Column::Id.in_subquery(
                runs::Entity::find()
                    .select_only()
                    .column(runs::Column::SessionId)
                    .into_query(),
            ),
        )
        .order_by_asc(tui_tabs::Column::Position)
}
impl Store {
    pub async fn tui_tab_title(&self, id: &str) -> Result<Option<String>> {
        let id = id.to_owned();
        self.read_access(move |conn| async move {
            Ok(sessions::Entity::find_by_id(id)
                .select_only()
                .column(sessions::Column::Title)
                .into_tuple::<String>()
                .one(&conn)
                .await?)
        })
        .await
    }
    pub async fn grant_tui_file(&self, id: &str, path: &str) -> Result<()> {
        let (id, path) = (id.to_owned(), path.to_owned());
        self.write_access(move |conn| async move {
            tui_file_grants::Entity::insert(tui_file_grants::ActiveModel {
                session_id: Set(id),
                path: Set(path),
            })
            .on_conflict(
                sea_query::OnConflict::columns([
                    tui_file_grants::Column::SessionId,
                    tui_file_grants::Column::Path,
                ])
                .do_nothing()
                .to_owned(),
            )
            .do_nothing()
            .exec(&conn)
            .await?;
            Ok(())
        })
        .await
    }
    pub async fn tui_file_grants(&self, id: &str) -> Result<std::collections::HashSet<String>> {
        let id = id.to_owned();
        self.read_access(move |conn| async move {
            Ok(tui_file_grants::Entity::find()
                .filter(tui_file_grants::Column::SessionId.eq(id))
                .all(&conn)
                .await?
                .into_iter()
                .map(|r| r.path)
                .collect())
        })
        .await
    }
    pub async fn set_tui_mode(&self, id: &str, mode: &str) -> Result<()> {
        anyhow::ensure!(
            matches!(mode, "ask" | "auto" | "YOLO"),
            "invalid permission mode"
        );
        let (id, mode) = (id.to_owned(), mode.to_owned());
        self.write_access(move |conn| async move {
            tui_session_state::Entity::insert(tui_session_state::ActiveModel {
                session_id: Set(id),
                permission_mode: Set(mode),
                ..Default::default()
            })
            .on_conflict(
                sea_query::OnConflict::column(tui_session_state::Column::SessionId)
                    .update_column(tui_session_state::Column::PermissionMode)
                    .to_owned(),
            )
            .exec(&conn)
            .await?;
            Ok(())
        })
        .await
    }
    pub async fn tui_mode(&self, id: &str) -> Result<String> {
        let id = id.to_owned();
        self.read_access(move |conn| async move {
            Ok(tui_session_state::Entity::find_by_id(id)
                .one(&conn)
                .await?
                .map_or_else(|| "auto".into(), |r| r.permission_mode))
        })
        .await
    }
    pub async fn publish_tui_runtime(&self, id: &str, run: i64, runtime: TuiRuntime) -> Result<()> {
        let id = id.to_owned();
        let status = if runtime.status == "cancelling" {
            "running"
        } else {
            &runtime.status
        }
        .to_owned();
        let payload = serde_json::to_string(&runtime)?;
        self.write_access(move |conn| async move {
            let tx = conn.begin().await?;
            // Validate ownership and freshness in the same transaction as the update.
            let latest = runs::Entity::find()
                .select_only()
                .columns([runs::Column::Id, runs::Column::Status])
                .filter(runs::Column::SessionId.eq(&id))
                .order_by_desc(runs::Column::Id)
                .into_tuple::<(i64, String)>()
                .one(&tx)
                .await?;
            if latest.is_some_and(|(id, state)| id == run && state == status) {
                tui_session_state::Entity::insert(tui_session_state::ActiveModel {
                    session_id: Set(id),
                    run_id: Set(Some(run)),
                    runtime: Set(Some(payload)),
                    ..Default::default()
                })
                .on_conflict(
                    sea_query::OnConflict::column(tui_session_state::Column::SessionId)
                        .update_columns([
                            tui_session_state::Column::RunId,
                            tui_session_state::Column::Runtime,
                        ])
                        .to_owned(),
                )
                .exec(&tx)
                .await?;
            }
            tx.commit().await?;
            Ok(())
        })
        .await
    }
    pub async fn tui_runtime(&self, id: &str) -> Result<Option<TuiRuntime>> {
        let id = id.to_owned();
        self.read_access(move |conn| async move {
            let tx = conn.begin().await?;
            let latest = runs::Entity::find()
                .select_only()
                .columns([runs::Column::Id, runs::Column::Status])
                .filter(runs::Column::SessionId.eq(&id))
                .order_by_desc(runs::Column::Id)
                .into_tuple::<(i64, String)>()
                .one(&tx)
                .await?;
            let state = tui_session_state::Entity::find_by_id(&id).one(&tx).await?;
            let payload = match (latest, state) {
                (Some((id, status)), Some(state))
                    if state.run_id == Some(id) && status != "interrupted" =>
                {
                    state.runtime
                }
                _ => None,
            };
            payload
                .map(|value| serde_json::from_str(&value).map_err(Into::into))
                .transpose()
        })
        .await
    }
    pub async fn session_revision(&self, id: &str) -> Result<Option<SessionRevision>> {
        let id = id.to_owned();
        self.read_access(move |conn| async move {
            let tx = conn.begin().await?;
            let Some((title, model, effort, workspace)) = sessions::Entity::find_by_id(&id)
                .select_only()
                .columns([
                    sessions::Column::Title,
                    sessions::Column::Model,
                    sessions::Column::ReasoningEffort,
                    sessions::Column::Workspace,
                ])
                .into_tuple::<(String, String, Option<String>, String)>()
                .one(&tx)
                .await?
            else {
                return Ok(None);
            };
            let latest = runs::Entity::find()
                .select_only()
                .column(runs::Column::Id)
                .column(runs::Column::Status)
                .filter(runs::Column::SessionId.eq(&id))
                .order_by_desc(runs::Column::Id)
                .into_tuple::<(i64, String)>()
                .one(&tx)
                .await?;
            let (run, status) = latest.unwrap_or_default();
            let event = events::Entity::find()
                .select_only()
                .column(events::Column::Id)
                .filter(events::Column::RunId.eq(run))
                .order_by_desc(events::Column::Id)
                .into_tuple::<i64>()
                .one(&tx)
                .await?
                .unwrap_or(0);
            Ok(Some(SessionRevision {
                run,
                event,
                status,
                title,
                model,
                effort,
                workspace,
            }))
        })
        .await
    }
    pub async fn run_events_after(
        &self,
        session: &str,
        run: i64,
        count: usize,
    ) -> Result<Vec<orbit_agent::events::AgentEvent>> {
        let session = session.to_owned();
        let offset = i64::try_from(count)? as u64;
        self.read_access(move |conn| async move {
            events::Entity::find()
                .join(JoinType::InnerJoin, events::Relation::Run.def())
                .filter(runs::Column::SessionId.eq(session))
                .filter(runs::Column::Id.eq(run))
                .order_by_asc(events::Column::Id)
                .limit(i64::MAX as u64)
                .offset(offset)
                .all(&conn)
                .await?
                .into_iter()
                .map(|r| serde_json::from_str(&r.payload).map_err(Into::into))
                .collect()
        })
        .await
    }
    pub async fn remember_tui_tab(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.write_access(move |conn| async move {
            let tx = conn.begin().await?;
            if sessions::Entity::find_by_id(&id).count(&tx).await? > 0 {
                tui_tabs::Entity::insert(tui_tabs::ActiveModel {
                    session_id: Set(id),
                    ..Default::default()
                })
                .on_conflict(
                    sea_query::OnConflict::column(tui_tabs::Column::SessionId)
                        .do_nothing()
                        .to_owned(),
                )
                .do_nothing()
                .exec(&tx)
                .await?;
            }
            tx.commit().await?;
            Ok(())
        })
        .await
    }
    pub async fn forget_tui_tab(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.write_access(move |conn| async move {
            tui_tabs::Entity::delete_many()
                .filter(tui_tabs::Column::SessionId.eq(id))
                .exec(&conn)
                .await?;
            Ok(())
        })
        .await
    }
    pub async fn all_tui_tabs(&self) -> Result<Vec<String>> {
        self.read_access(move |conn| async move {
            Ok(tabs_query()
                .select_only()
                .column(tui_tabs::Column::SessionId)
                .into_tuple()
                .all(&conn)
                .await?)
        })
        .await
    }
    pub async fn tui_tabs(&self, project: &str) -> Result<Vec<String>> {
        let projects = self.equivalent_project_ids(project).await?;
        self.read_access(move |conn| async move {
            Ok(tabs_query()
                .filter(sessions::Column::ProjectId.is_in(projects))
                .select_only()
                .column(tui_tabs::Column::SessionId)
                .into_tuple()
                .all(&conn)
                .await?)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn tabs_survive_reopening_and_ignore_empty_and_other_projects() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let db = dir.path().join("sessions.db");
        let store = Store::open(&db).await.unwrap();
        let project = store.resolve_project(&a).await.unwrap();
        let mut first = store
            .create(a.to_string_lossy().into_owned(), "m".into())
            .await
            .unwrap();
        first.begin("hello".into(), "m".into()).await.unwrap();
        first
            .finish("completed", vec![], Some(vec![]))
            .await
            .unwrap();
        let empty = store
            .create(a.to_string_lossy().into_owned(), "m".into())
            .await
            .unwrap();
        let mut other = store
            .create(b.to_string_lossy().into_owned(), "m".into())
            .await
            .unwrap();
        other.begin("hello".into(), "m".into()).await.unwrap();
        other
            .finish("completed", vec![], Some(vec![]))
            .await
            .unwrap();
        for id in [first.id(), empty.id(), other.id()] {
            store.remember_tui_tab(id).await.unwrap();
        }
        let peer = Store::open(db).await.unwrap();
        assert_eq!(peer.tui_tabs(&project.id).await.unwrap(), vec![first.id()]);
        peer.forget_tui_tab(first.id()).await.unwrap();
        assert!(store.tui_tabs(&project.id).await.unwrap().is_empty());
    }
}
