use crate::entities::*;
use anyhow::{Context, Result};
use orbit_agent::{checkpoint::ExecutionCheckpoint, events::AgentEvent};
use orbit_model::types::{ChatMessage, StreamEvent};
use sea_orm::*;

use crate::store::Store;

impl Store {
    /// Snapshot the current request without recovery receipts or acquiring its writer lease.
    pub async fn live_context(&self, session: &str) -> Result<Vec<ChatMessage>> {
        let session = session.to_owned();
        self.read_access(move |conn| async move {
            let tx = conn.begin().await?;
            let payload = sessions::Entity::find_by_id(&session)
                .one(&tx)
                .await?
                .context("session not found")?
                .history;
            let mut history: Vec<ChatMessage> = serde_json::from_str(&payload)?;
            let active = runs::Entity::find()
                .filter(runs::Column::SessionId.eq(&session))
                .order_by_desc(runs::Column::Id)
                .one(&tx)
                .await?;
            if let Some(row) = active
                && row.status != "completed"
            {
                let run = row.id;
                let prompt = row.prompt;
                let checkpoint = execution_checkpoints::Entity::find_by_id(run)
                    .one(&tx)
                    .await?
                    .map(|row| row.payload);
                if let Some(checkpoint) = checkpoint {
                    let checkpoint = serde_json::from_str::<ExecutionCheckpoint>(&checkpoint)?;
                    history = checkpoint.history;
                    if !checkpoint.attachments.is_empty() {
                        history.push(ChatMessage::user_with_attachments(
                            "[Images read by the main agent]",
                            checkpoint.attachments,
                        ));
                    }
                } else {
                    history.push(ChatMessage::user(prompt));
                }

                let mut text = String::new();
                let mut finished = false;
                let mut failure = None;
                for event in events::Entity::find()
                    .filter(events::Column::RunId.eq(run))
                    .order_by_asc(events::Column::Id)
                    .all(&tx)
                    .await?
                {
                    match serde_json::from_str::<AgentEvent>(&event.payload)? {
                        AgentEvent::TurnStarted { .. } => {
                            text.clear();
                            finished = false;
                        }
                        AgentEvent::Model(StreamEvent::TextDelta(delta)) => text.push_str(&delta),
                        AgentEvent::Model(StreamEvent::Completed { .. }) => finished = true,
                        AgentEvent::Failed { error } => failure = Some(error),
                        _ => {}
                    }
                }

                let already_saved = finished
                    && history
                        .iter()
                        .rev()
                        .find_map(|message| {
                            if let ChatMessage::Assistant { content, .. } = message {
                                Some(content.as_deref().unwrap_or_default() == text)
                            } else {
                                None
                            }
                        })
                        .unwrap_or(false);
                if !text.is_empty() && !already_saved {
                    history.push(ChatMessage::assistant(text));
                }
                if let Some(error) = failure {
                    history.push(ChatMessage::user(format!("[Main task failed] {error}")));
                }
            }

            Ok(history)
        })
        .await
    }
}
