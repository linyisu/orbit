use crate::entities::*;
use anyhow::{Context, Result, ensure};
use orbit_agent::{checkpoint::ExecutionCheckpoint, events::AgentEvent};
use orbit_model::types::{ChatMessage, StreamEvent};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::store::Store;

pub use orbit_protocol::SideQuestion;

impl Store {
    pub async fn new_side_question_thread(&self) -> Result<String> {
        crate::database::new_id()
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
        self.write_access(move |conn| async move {
            side_questions::ActiveModel {
                session_id: Set(session),
                payload: Set(payload),
                ..Default::default()
            }
            .insert(&conn)
            .await?;
            Ok(())
        })
        .await
    }

    pub async fn begin_side_question(&self, session: &str, question: &SideQuestion) -> Result<i64> {
        ensure!(
            question.status == "running" && !question.question.trim().is_empty(),
            "invalid running side question"
        );
        let session = session.to_owned();
        let payload = serde_json::to_string(question)?;
        self.write_access(move |conn| async move {
            Ok(side_questions::ActiveModel {
                session_id: Set(session),
                payload: Set(payload),
                ..Default::default()
            }
            .insert(&conn)
            .await?
            .id)
        })
        .await
    }

    pub async fn checkpoint_side_question(
        &self,
        session: &str,
        id: i64,
        question: &SideQuestion,
    ) -> Result<()> {
        let session = session.to_owned();
        let payload = serde_json::to_string(question)?;
        self.write_access(move |conn| async move {
            let updated = side_questions::Entity::update_many()
                .col_expr(side_questions::Column::Payload, Expr::value(payload))
                .filter(side_questions::Column::SessionId.eq(session))
                .filter(side_questions::Column::Id.eq(id))
                .exec(&conn)
                .await?;
            ensure!(
                updated.rows_affected == 1,
                "side question checkpoint missing"
            );
            Ok(())
        })
        .await
    }
    pub async fn interrupt_side_questions(&self, session: &str) -> Result<()> {
        let session = session.to_owned();
        self.write_access(move |conn| async move {
            let payload = Expr::col(side_questions::Column::Payload);
            let status = sea_query::Func::cust(sea_query::Alias::new("json_extract"))
                .args([payload.clone().into(), Expr::value("$.status")]);
            let interrupted = sea_query::Func::cust(sea_query::Alias::new("json_set")).args([
                payload.into(),
                Expr::value("$.status"),
                Expr::value("interrupted"),
                Expr::value("$.error"),
                Expr::value("服务已中断，已保存生成内容。"),
            ]);
            side_questions::Entity::update_many()
                .col_expr(side_questions::Column::Payload, interrupted.into())
                .filter(side_questions::Column::SessionId.eq(session))
                .filter(Expr::expr(status).eq("running"))
                .exec(&conn)
                .await?;
            Ok(())
        })
        .await
    }
    pub async fn side_questions(&self, session: &str) -> Result<Vec<SideQuestion>> {
        let session = session.to_owned();
        self.read_access(move |conn| async move {
            side_questions::Entity::find()
                .filter(side_questions::Column::SessionId.eq(session))
                .order_by_asc(side_questions::Column::Id)
                .all(&conn)
                .await?
                .into_iter()
                .map(|row| {
                    let mut record: SideQuestion = serde_json::from_str(&row.payload)?;
                    if record.thread_id.is_empty() {
                        record.thread_id = format!("legacy-{}", row.id);
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
