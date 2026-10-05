//! Project-scoped open tabs and inexpensive change detection for TUI readers.
use anyhow::Result;
use rusqlite::{OptionalExtension, params};

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

impl Store {
    pub async fn tui_tab_title(&self, id: &str) -> Result<Option<String>> {
        let id = id.to_owned();
        self.read_access(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT title FROM sessions WHERE id=?1 AND demo=0",
                    [&id],
                    |row| row.get(0),
                )
                .optional()?)
        })
        .await
    }

    pub async fn grant_tui_file(&self, id: &str, path: &str) -> Result<()> {
        let (id, path) = (id.to_owned(), path.to_owned());
        self.access(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO tui_file_grants(session_id,path) VALUES(?1,?2)",
                params![id, path],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn tui_file_grants(&self, id: &str) -> Result<std::collections::HashSet<String>> {
        let id = id.to_owned();
        self.read_access(move |conn| {
            let mut statement =
                conn.prepare("SELECT path FROM tui_file_grants WHERE session_id=?1")?;
            Ok(statement
                .query_map([id], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
    }

    pub async fn set_tui_mode(&self, id: &str, mode: &str) -> Result<()> {
        anyhow::ensure!(
            matches!(mode, "ask" | "auto" | "YOLO"),
            "invalid permission mode"
        );
        let (id, mode) = (id.to_owned(), mode.to_owned());
        self.access(move |conn| {
            conn.execute(
                "INSERT INTO tui_session_state(session_id,permission_mode) VALUES(?1,?2)
                ON CONFLICT(session_id) DO UPDATE SET permission_mode=excluded.permission_mode",
                params![id, mode],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn tui_mode(&self, id: &str) -> Result<String> {
        let id = id.to_owned();
        self.read_access(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT permission_mode FROM tui_session_state WHERE session_id=?1",
                    [&id],
                    |row| row.get(0),
                )
                .optional()?
                .unwrap_or_else(|| "auto".into()))
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
        self.access(move |conn| {
            // A late update from a completed owner must never overwrite the next run.
            conn.execute("INSERT INTO tui_session_state(session_id,run_id,runtime)
                SELECT ?1,?2,?3 WHERE EXISTS(SELECT 1 FROM runs WHERE id=?2 AND session_id=?1 AND status=?4)
                AND ?2=(SELECT MAX(id) FROM runs WHERE session_id=?1)
                ON CONFLICT(session_id) DO UPDATE SET run_id=excluded.run_id,runtime=excluded.runtime", params![id,run,payload,status])?;
            Ok(())
        }).await
    }

    pub async fn tui_runtime(&self, id: &str) -> Result<Option<TuiRuntime>> {
        let id = id.to_owned();
        self.read_access(move |conn| {
            let payload: Option<String> = conn
                .query_row(
                    "SELECT t.runtime FROM tui_session_state t JOIN runs r ON r.id=t.run_id
                WHERE t.session_id=?1 AND r.id=(SELECT MAX(id) FROM runs WHERE session_id=?1)
                AND r.status!='interrupted' AND t.runtime IS NOT NULL",
                    [&id],
                    |row| row.get(0),
                )
                .optional()?;
            payload
                .map(|value| serde_json::from_str(&value).map_err(Into::into))
                .transpose()
        })
        .await
    }

    pub async fn session_revision(&self, id: &str) -> Result<Option<SessionRevision>> {
        let id = id.to_owned();
        self.read_access(move |conn| {
            Ok(conn.query_row(
                "SELECT COALESCE(r.id,0), COALESCE((SELECT MAX(id) FROM events WHERE run_id=r.id),0),
                 COALESCE(r.status,''),s.title,s.model,s.reasoning_effort,s.workspace
                 FROM sessions s LEFT JOIN runs r ON r.id=(SELECT MAX(id) FROM runs WHERE session_id=s.id)
                 WHERE s.id=?1", [&id], |row| Ok(SessionRevision {
                    run: row.get(0)?, event: row.get(1)?, status: row.get(2)?, title: row.get(3)?,
                    model: row.get(4)?, effort: row.get(5)?, workspace: row.get(6)?,
                 })).optional()?)
        }).await
    }

    /// Read only newly appended events for the currently running turn.
    pub async fn run_events_after(
        &self,
        session: &str,
        run: i64,
        count: usize,
    ) -> Result<Vec<orbit_agent::events::AgentEvent>> {
        let session = session.to_owned();
        self.read_access(move |conn| {
            let mut statement = conn.prepare("SELECT e.payload FROM events e JOIN runs r ON r.id=e.run_id WHERE r.session_id=?1 AND r.id=?2 ORDER BY e.id LIMIT -1 OFFSET ?3")?;
            let rows = statement.query_map(params![session, run, i64::try_from(count)?], |row| row.get::<_,String>(0))?;
            rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
        }).await
    }

    /// Add/remove individual tabs, so another window cannot overwrite the entire set.
    pub async fn remember_tui_tab(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.access(move |conn| {
            conn.execute(
                "INSERT OR IGNORE INTO tui_tabs(session_id) SELECT id FROM sessions WHERE id=?1",
                [&id],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn forget_tui_tab(&self, id: &str) -> Result<()> {
        let id = id.to_owned();
        self.access(move |conn| {
            conn.execute("DELETE FROM tui_tabs WHERE session_id=?1", [&id])?;
            Ok(())
        })
        .await
    }

    pub async fn all_tui_tabs(&self) -> Result<Vec<String>> {
        self.read_access(move |conn| {
            let mut statement = conn.prepare(
                "SELECT s.id FROM tui_tabs t JOIN sessions s ON s.id=t.session_id
                 WHERE s.demo=0 AND EXISTS(SELECT 1 FROM runs WHERE session_id=s.id) ORDER BY t.position")?;
            let rows = statement.query_map(params![], |row| row.get::<_,String>(0))?;
            let mut ids = Vec::new();
            for row in rows { ids.push(row?); }
            Ok(ids)
        }).await
    }

    pub async fn tui_tabs(&self, project: &str) -> Result<Vec<String>> {
        let projects = self.equivalent_project_ids(project).await?;
        self.read_access(move |conn| {
            let mut statement = conn.prepare(
                "SELECT s.id,s.project_id FROM tui_tabs t JOIN sessions s ON s.id=t.session_id
                 WHERE s.demo=0 AND EXISTS(SELECT 1 FROM runs WHERE session_id=s.id) ORDER BY t.position")?;
            let rows = statement.query_map(params![], |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?)))?;
            let mut ids = Vec::new();
            for row in rows { let (id, project) = row?; if projects.contains(&project) { ids.push(id); } }
            Ok(ids)
        }).await
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
            .create(a.to_string_lossy().into_owned(), "m".into(), false)
            .await
            .unwrap();
        first.begin("hello".into(), "m".into()).await.unwrap();
        first
            .finish("completed", vec![], Some(vec![]))
            .await
            .unwrap();
        let empty = store
            .create(a.to_string_lossy().into_owned(), "m".into(), false)
            .await
            .unwrap();
        let mut other = store
            .create(b.to_string_lossy().into_owned(), "m".into(), false)
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
