//! Session accounting reads only usage receipts, in bounded pages. Streaming
//! completion chunks and their final RequestMetrics receipt count once.
use crate::{
    entities::{events, runs, sessions},
    store::Store,
};
use anyhow::{Context, Result};
use orbit_agent::events::AgentEvent;
use orbit_model::types::{StreamEvent, Usage};
use orbit_protocol::usage::Session;
use sea_orm::*;

impl Store {
    pub async fn session_usage(&self, session: &str) -> Result<Session> {
        let session = session.to_owned();
        self.read_access(move |conn| async move {
            let tx = conn.begin().await?;
            sessions::Entity::find_by_id(&session)
                .one(&tx)
                .await?
                .context("session not found")?;
            let mut result = Session {
                runs: runs::Entity::find()
                    .filter(runs::Column::SessionId.eq(&session))
                    .count(&tx)
                    .await? as usize,
                ..Default::default()
            };
            let query = events::Entity::find()
                .join(JoinType::InnerJoin, events::Relation::Run.def())
                .filter(runs::Column::SessionId.eq(&session))
                .filter(
                    Condition::any()
                        .add(events::Column::Payload.starts_with("{\"RequestMetrics\":"))
                        .add(events::Column::Payload.starts_with("{\"TurnStarted\":"))
                        .add(events::Column::Payload.starts_with("{\"Model\":{\"Completed\":")),
                )
                .order_by_asc(events::Column::RunId)
                .order_by_asc(events::Column::Id);
            let mut after = 0;
            let mut after_run = 0;
            let mut current_run = None;
            let mut run_has_usage = false;
            let mut pending = Pending::default();
            loop {
                let page = query
                    .clone()
                    .filter(
                        Condition::any()
                            .add(events::Column::RunId.gt(after_run))
                            .add(
                                Condition::all()
                                    .add(events::Column::RunId.eq(after_run))
                                    .add(events::Column::Id.gt(after)),
                            ),
                    )
                    .limit(32)
                    .all(&tx)
                    .await?;
                let Some(last) = page.last() else {
                    break;
                };
                after = last.id;
                after_run = last.run_id;
                for row in page {
                    if current_run != Some(row.run_id) {
                        run_has_usage |= pending.flush(&mut result);
                        result.runs_with_usage += usize::from(run_has_usage);
                        run_has_usage = false;
                        current_run = Some(row.run_id);
                    }
                    match serde_json::from_str::<AgentEvent>(&row.payload) {
                        Ok(AgentEvent::TurnStarted { .. }) => {
                            run_has_usage |= pending.flush(&mut result);
                        }
                        Ok(AgentEvent::Model(StreamEvent::Completed { usage, .. })) => {
                            pending.seen = true;
                            pending.merge(usage);
                        }
                        Ok(AgentEvent::RequestMetrics { usage, .. }) => {
                            pending.seen = true;
                            pending.merge(usage);
                            run_has_usage |= pending.flush(&mut result);
                        }
                        Err(_) => {
                            result.error = Some("部分用量记录无法解析；以下为已识别的数据".into())
                        }
                        _ => {}
                    }
                }
            }
            run_has_usage |= pending.flush(&mut result);
            result.runs_with_usage += usize::from(run_has_usage);
            tx.commit().await?;
            Ok(result)
        })
        .await
    }
}

#[derive(Default)]
struct Pending {
    seen: bool,
    usage: Option<Usage>,
}
impl Pending {
    fn merge(&mut self, usage: Option<Usage>) {
        let Some(usage) = usage else {
            return;
        };
        let current = self.usage.get_or_insert(Usage {
            prompt_tokens: None,
            completion_tokens: None,
            total_tokens: None,
            cached_input_tokens: None,
            cache_write_input_tokens: None,
        });
        current.prompt_tokens = usage.prompt_tokens.or(current.prompt_tokens);
        current.completion_tokens = usage.completion_tokens.or(current.completion_tokens);
        current.cached_input_tokens = usage.cached_input_tokens.or(current.cached_input_tokens);
        current.cache_write_input_tokens = usage
            .cache_write_input_tokens
            .or(current.cache_write_input_tokens);
    }
    fn flush(&mut self, result: &mut Session) -> bool {
        result.requests += usize::from(std::mem::take(&mut self.seen));
        let Some(usage) = self.usage.take() else {
            return false;
        };
        fn add(total: &mut Option<u64>, value: Option<u64>) {
            if let Some(value) = value {
                *total = Some(total.unwrap_or(0).saturating_add(value));
            }
        }
        add(&mut result.input, usage.prompt_tokens);
        add(&mut result.output, usage.completion_tokens);
        add(&mut result.cached_input, usage.cached_input_tokens);
        add(&mut result.cache_write, usage.cache_write_input_tokens);
        true
    }
}
