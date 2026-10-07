//! Append-only session planning snapshots, with compare-and-swap updates.
use crate::store::Store;
use anyhow::{Result, ensure};
use orbit_protocol::plan::State;
use rusqlite::{OptionalExtension, params};
impl Store {
    pub async fn planning(&self, session: &str) -> Result<State> {
        let session = session.to_owned();
        self.read_access(move |conn| {
            let value: Option<String> = conn.query_row("SELECT payload FROM session_plans WHERE session_id=?1 ORDER BY generation DESC LIMIT 1", [session], |r| r.get(0)).optional()?;
            Ok(value.map(|s| serde_json::from_str(&s)).transpose()?.unwrap_or_default())
        }).await
    }
    pub async fn save_planning(
        &self,
        session: &str,
        expected: u64,
        state: &State,
    ) -> Result<State> {
        ensure!(
            Some(state.generation) == expected.checked_add(1),
            "invalid planning generation"
        );
        let session = session.to_owned();
        let mut state = state.clone();
        let generation = i64::try_from(state.generation)?;
        self.access(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let current: i64 = tx.query_row(
                "SELECT COALESCE(MAX(generation),0) FROM session_plans WHERE session_id=?1",
                [&session],
                |r| r.get(0),
            )?;
            ensure!(
                u64::try_from(current)? == expected,
                "计划已更新，请查看最新版本"
            );
            if let Some(plan) = &mut state.plan
                && plan.status == orbit_protocol::plan::Status::Approved
                && plan.task_ids.is_empty()
            {
                let project: String = tx.query_row(
                    "SELECT project_id FROM sessions WHERE id=?1",
                    [&session],
                    |r| r.get(0),
                )?;
                for (i, step) in plan.steps.iter().enumerate() {
                    let title: String = step.chars().take(512).collect();
                    let details = format!(
                        "Plan {} revision {} step {}\n{}",
                        plan.id,
                        plan.revision,
                        i + 1,
                        step
                    );
                    tx.execute(
                        "INSERT INTO project_tasks(project_id,title,details) VALUES(?1,?2,?3)",
                        params![project, title, details],
                    )?;
                    plan.task_ids.push(tx.last_insert_rowid());
                }
            }
            let payload = serde_json::to_string(&state)?;
            tx.execute(
                "INSERT INTO session_plans(session_id,generation,payload) VALUES(?1,?2,?3)",
                params![session, generation, payload],
            )?;
            tx.commit()?;
            Ok(state)
        })
        .await
    }
}
pub(crate) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch("CREATE TABLE IF NOT EXISTS session_plans(session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE, generation INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(session_id,generation)); PRAGMA user_version=26;")?;
    Ok(())
}
