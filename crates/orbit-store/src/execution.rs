use sea_orm::sea_query::Expr;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::entities::*;
use anyhow::ensure;
use orbit_agent::{
    checkpoint::{CheckpointFuture, ExecutionCheckpoint, ExecutionJournal},
    error::AgentError,
    events::AgentEvent,
};
use orbit_model::types::ChatMessage;
use sea_orm::*;
use sha2::{Digest, Sha256};

use crate::store::Store;

fn journal_lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) struct Journal {
    pub store: Store,
    pub run: i64,
    pub session: String,
    pub commit_lock: tokio::sync::Mutex<()>,
    pub pending: std::sync::Mutex<Vec<AgentEvent>>,
    pub pending_messages: std::sync::Mutex<Vec<ChatMessage>>,
}

impl Journal {
    async fn commit(
        &self,
        checkpoint: Option<ExecutionCheckpoint>,
        event: Option<AgentEvent>,
    ) -> Result<(), AgentError> {
        let _guard = self.commit_lock.lock().await;
        let run = self.run;
        let session = self.session.clone();
        // Keep queued facts until the SQLite transaction commits. A failed
        // checkpoint must not silently discard the only copy of new messages.
        let events = journal_lock(&self.pending).clone();
        let messages = journal_lock(&self.pending_messages).clone();
        let event_count = events.len();
        let message_count = messages.len();
        if checkpoint.is_none() && event.is_none() && event_count == 0 && message_count == 0 {
            return Ok(());
        }
        let mut events = events;
        events.extend(event);
        self.store
            .write_access(move |conn| async move {
                let transaction = conn.begin().await?;
                let active = runs::Entity::find_by_id(run)
                    .filter(runs::Column::SessionId.eq(&session))
                    .filter(runs::Column::Status.eq("running"))
                    .one(&transaction)
                    .await?
                    .is_some();
                ensure!(active, "checkpoint writer no longer owns an active run");
                if let Some(checkpoint) = checkpoint {
                    execution_checkpoints::Entity::insert(execution_checkpoints::ActiveModel {
                        run_id: Set(run),
                        payload: Set(serde_json::to_string(&checkpoint)?),
                    })
                    .on_conflict(
                        sea_query::OnConflict::column(execution_checkpoints::Column::RunId)
                            .update_column(execution_checkpoints::Column::Payload)
                            .to_owned(),
                    )
                    .exec(&transaction)
                    .await?;
                }
                if !messages.is_empty() {
                    let first = session_messages::Entity::find()
                        .filter(session_messages::Column::RunId.eq(run))
                        .order_by_desc(session_messages::Column::Ordinal)
                        .one(&transaction)
                        .await?
                        .map_or(0, |row| row.ordinal + 1);
                    for (ordinal, message) in (first..).zip(messages) {
                        let payload = serde_json::to_string(&message)?;
                        let hash = Sha256::digest(payload.as_bytes())
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<String>();
                        session_messages::ActiveModel {
                            session_id: Set(session.clone()),
                            run_id: Set(run),
                            ordinal: Set(ordinal),
                            payload: Set(payload),
                            sha256: Set(hash),
                            ..Default::default()
                        }
                        .insert(&transaction)
                        .await?;
                    }
                }
                for event in &events {
                    match event {
                        AgentEvent::InputReceived { message_id, .. } => {
                            agent_messages::Entity::update_many()
                                .col_expr(agent_messages::Column::Received, Expr::value(true))
                                .filter(agent_messages::Column::Id.eq(*message_id))
                                .filter(agent_messages::Column::SessionId.eq(&session))
                                .exec(&transaction)
                                .await?;
                        }
                        AgentEvent::ToolFinished { call, content, .. }
                            if matches!(
                                call.function.name.as_str(),
                                "spawn_agent" | "send_agent_message"
                            ) =>
                        {
                            if let Ok(value) = serde_json::from_str::<serde_json::Value>(content)
                                && let Some(id) =
                                    value.get("invocation_id").and_then(|id| id.as_i64())
                            {
                                agent_invocations::Entity::update_many()
                                    .col_expr(
                                        agent_invocations::Column::Delivered,
                                        Expr::value(true),
                                    )
                                    .filter(agent_invocations::Column::Id.eq(id))
                                    .filter(agent_invocations::Column::ParentSessionId.eq(&session))
                                    .filter(agent_invocations::Column::Result.is_not_null())
                                    .exec(&transaction)
                                    .await?;
                            }
                        }
                        _ => {}
                    }
                }
                crate::event_log::append(&transaction, run, events).await?;
                transaction.commit().await?;
                Ok(())
            })
            .await
            .map_err(|e| AgentError::Checkpoint(e.to_string()))?;
        journal_lock(&self.pending).drain(..event_count);
        journal_lock(&self.pending_messages).drain(..message_count);

        Ok(())
    }
}

impl ExecutionJournal for Journal {
    fn record(&self, event: AgentEvent) {
        // These receipts were already committed together with their context
        // before emission. All other events retain their original stream order.
        if !matches!(
            event,
            AgentEvent::ToolStarted { .. }
                | AgentEvent::ToolBackgrounded { .. }
                | AgentEvent::ToolFinished { .. }
                | AgentEvent::ToolFailed { .. }
                | AgentEvent::InputReceived { .. }
        ) {
            journal_lock(&self.pending).push(event);
        }
    }

    fn record_message(&self, message: ChatMessage) {
        journal_lock(&self.pending_messages).push(message);
    }

    fn flush(&self) -> CheckpointFuture<'_> {
        Box::pin(self.commit(None, None))
    }

    fn save<'a>(
        &'a self,
        checkpoint: ExecutionCheckpoint,
        event: Option<AgentEvent>,
    ) -> CheckpointFuture<'a> {
        Box::pin(self.commit(Some(checkpoint), event))
    }
}
