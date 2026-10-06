use anyhow::{Context, Result, ensure};
use orbit_agent::{checkpoint::ExecutionCheckpoint, events::AgentEvent};
use orbit_model::types::{ChatMessage, StreamEvent};
use rusqlite::{OptionalExtension, params};

use crate::store::Store;

pub use orbit_protocol::SideQuestion;

pub(crate) fn migrate(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS side_questions (
        id INTEGER PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
        payload TEXT NOT NULL,
        created_at INTEGER NOT NULL DEFAULT (unixepoch())
    );
    CREATE INDEX IF NOT EXISTS side_questions_session ON side_questions(session_id,id);
    PRAGMA user_version=22;",
    )?;
    Ok(())
}

impl Store {
    pub async fn new_side_question_thread(&self) -> Result<String> {
        self.read_access(|conn| {
            Ok(conn.query_row("SELECT lower(hex(randomblob(16)))", [], |r| r.get(0))?)
        })
        .await
    }

    pub async fn save_side_question(&self, session: &str, question: SideQuestion) -> Result<()> {
        ensure!(
            matches!(
                question.status.as_str(),
                "completed" | "failed" | "cancelled"
            ),
            "side question is not finished"
        );
        ensure!(
            !question.question.trim().is_empty(),
            "side question is empty"
        );
        let session = session.to_owned();
        let payload = serde_json::to_string(&question)?;
        self.access(move |conn| {
            conn.execute(
                "INSERT INTO side_questions(session_id,payload) VALUES(?1,?2)",
                params![session, payload],
            )?;
            Ok(())
        })
        .await
    }

    pub async fn side_questions(&self, session: &str) -> Result<Vec<SideQuestion>> {
        let session = session.to_owned();
        self.read_access(move |conn| {
            let mut query = conn
                .prepare("SELECT id,payload FROM side_questions WHERE session_id=?1 ORDER BY id")?;
            query
                .query_map([session], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })?
                .map(|row| {
                    let (id, payload) = row?;
                    let mut record: SideQuestion = serde_json::from_str(&payload)?;
                    if record.thread_id.is_empty() {
                        record.thread_id = format!("legacy-{id}");
                    }
                    Ok(record)
                })
                .collect()
        })
        .await
    }

    /// Snapshot the current request without recovery receipts or acquiring its writer lease.
    pub async fn side_question_context(&self, session: &str) -> Result<Vec<ChatMessage>> {
        let session = session.to_owned();
        self.read_access(move |conn| {
            let tx = conn.transaction()?;
            let payload: String = tx.query_row("SELECT history FROM sessions WHERE id=?1", [&session], |r|r.get(0)).context("session not found")?;
            let mut history: Vec<ChatMessage> = serde_json::from_str(&payload)?;
            let active: Option<(i64,String,String)> = tx.query_row("SELECT id,prompt,status FROM runs WHERE session_id=?1 ORDER BY id DESC LIMIT 1",[&session],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if let Some((run, prompt, status)) = active && status != "completed" {
                let checkpoint: Option<String> = tx.query_row("SELECT payload FROM execution_checkpoints WHERE run_id=?1", [run],|r|r.get(0)).optional()?;
                if let Some(checkpoint) = checkpoint {
                    let checkpoint = serde_json::from_str::<ExecutionCheckpoint>(&checkpoint)?;
                    history = checkpoint.history;
                    if !checkpoint.attachments.is_empty() {
                        history.push(ChatMessage::user_with_attachments("[Images read by the main agent]", checkpoint.attachments));
                    }
                } else {
                    history.push(ChatMessage::user(prompt));
                }

                let mut text = String::new();
                let mut finished = false;
                let mut failure = None;
                let mut query = tx.prepare("SELECT payload FROM events WHERE run_id=?1 ORDER BY id")?;
                for event in query.query_map([run],|r|r.get::<_,String>(0))? {
                    match serde_json::from_str::<AgentEvent>(&event?)? {
                        AgentEvent::TurnStarted { .. } => { text.clear(); finished = false; }
                        AgentEvent::Model(StreamEvent::TextDelta(delta)) => text.push_str(&delta),
                        AgentEvent::Model(StreamEvent::Completed { .. }) => finished = true,
                        AgentEvent::Failed { error } => failure = Some(error),
                        _ => {}
                    }
                }

                let already_saved = finished && history.iter().rev().find_map(|message| {
                    if let ChatMessage::Assistant { content, .. } = message { Some(content.as_deref().unwrap_or_default() == text) } else { None }
                }).unwrap_or(false);
                if !text.is_empty() && !already_saved {
                    history.push(ChatMessage::assistant(text));
                }
                if let Some(error) = failure { history.push(ChatMessage::user(format!("[Main task failed] {error}"))); }
            }

            Ok(history)
        }).await
    }
}
