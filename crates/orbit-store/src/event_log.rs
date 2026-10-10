//! Compact durable stream fragments without changing live delivery or barriers.
use anyhow::Result;
#[cfg(test)]
use orbit_agent::events::MAX_STREAM_FRAGMENT_BYTES as MAX_FRAGMENT_BYTES;
use orbit_agent::events::{AgentEvent, coalesce as merge};
#[cfg(test)]
use orbit_model::types::StreamEvent;
use sea_orm::*;

use crate::entities::{events, runs};

#[derive(Default)]
pub(crate) struct Evidence {
    pub files: orbit_agent::memory::FileActivity,
    pub calibration: std::collections::BTreeMap<String, orbit_agent::calibration::TokenCalibration>,
}

pub(crate) async fn load_by_session(
    tx: &DatabaseTransaction,
    session: &str,
    first_run: i64,
    runtime: Option<i64>,
) -> Result<(std::collections::HashMap<i64, Vec<events::Model>>, Evidence)> {
    let mut grouped = std::collections::HashMap::<i64, Vec<_>>::new();
    let mut evidence = Evidence::default();
    let mut query = events::Entity::find()
        .join(JoinType::InnerJoin, events::Relation::Run.def())
        .filter(runs::Column::SessionId.eq(session))
        .filter(events::Column::RunId.gte(first_run))
        .order_by_asc(events::Column::Id);
    if runtime.is_some() {
        query = query.filter(
            Condition::any()
                .add(events::Column::Payload.starts_with("{\"RequestMetrics\":"))
                .add(events::Column::Payload.starts_with("{\"ToolFinished\":"))
                .add(events::Column::Payload.starts_with("{\"CompactionRecorded\":")),
        );
    }
    let mut after = 0;
    loop {
        let page = query
            .clone()
            .filter(events::Column::Id.gt(after))
            .limit(32)
            .all(tx)
            .await?;
        let Some(last) = page.last() else {
            break;
        };
        after = last.id;
        for event in page {
            if let Some(last_completed) = runtime {
                let receipt: AgentEvent = serde_json::from_str(&event.payload)?;
                match receipt {
                    AgentEvent::ToolFinished { call, content, .. } => {
                        evidence.files.observe(&call, &content);
                        continue;
                    }
                    AgentEvent::RequestMetrics {
                        profile,
                        estimated_input,
                        usage,
                        ..
                    } => {
                        evidence
                            .calibration
                            .entry(profile)
                            .or_default()
                            .observe(estimated_input, usage.and_then(|usage| usage.prompt_tokens));
                        continue;
                    }
                    AgentEvent::CompactionRecorded { record } => {
                        evidence.files.merge(&record.files);
                        if event.run_id != last_completed {
                            continue;
                        }
                    }
                    _ => continue,
                }
            }
            grouped.entry(event.run_id).or_default().push(event);
        }
    }
    Ok((grouped, evidence))
}

pub(crate) async fn append(
    tx: &DatabaseTransaction,
    run: i64,
    incoming: Vec<AgentEvent>,
) -> Result<()> {
    if incoming.is_empty() {
        return Ok(());
    }
    let mut compacted = Vec::new();
    for event in incoming {
        if !compacted.last_mut().is_some_and(|last| merge(last, &event)) {
            compacted.push(event);
        }
    }
    let mut incoming = compacted.into_iter().peekable();
    if let Some(row) = events::Entity::find()
        .filter(events::Column::RunId.eq(run))
        .order_by_desc(events::Column::Id)
        .one(tx)
        .await?
    {
        let mut tail: AgentEvent = serde_json::from_str(&row.payload)?;
        if incoming.peek().is_some_and(|first| merge(&mut tail, first)) {
            incoming.next();
            let mut row = row.into_active_model();
            row.payload = Set(serde_json::to_string(&tail)?);
            row.update(tx).await?;
        }
    }
    for event in incoming {
        events::ActiveModel {
            run_id: Set(run),
            payload: Set(serde_json::to_string(&event)?),
            created_at_ms: Set(Some(crate::store::now_millis())),
            ..Default::default()
        }
        .insert(tx)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[tokio::test]
    async fn flushes_preserve_recovery_text_barriers_order_and_bound_tail_rewrites() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.db");
        let store = Store::open(&path).await.unwrap();
        let mut writer = store
            .create(dir.path().display().to_string(), "model".into())
            .await
            .unwrap();
        let id = writer.id().to_owned();
        writer
            .begin("question".into(), "model".into())
            .await
            .unwrap();
        let journal = writer.execution_journal().unwrap();
        for _ in 0..3 {
            for _ in 0..1000 {
                journal.record(AgentEvent::Model(StreamEvent::TextDelta("甲".into())));
            }
            journal.flush().await.unwrap();
        }
        let partial = Store::open(&path).await.unwrap().load(&id).await.unwrap();
        assert_eq!(partial.runs[0].events.len(), 1);
        assert!(
            matches!(&partial.runs[0].events[0], AgentEvent::Model(StreamEvent::TextDelta(text)) if text == &"甲".repeat(3000))
        );
        journal.record(AgentEvent::ReasoningFinished { elapsed_ms: 12 });
        journal.record(AgentEvent::Model(StreamEvent::TextDelta(
            "after barrier".into(),
        )));
        journal.record(AgentEvent::Model(StreamEvent::TextDelta(
            "x".repeat(MAX_FRAGMENT_BYTES),
        )));
        journal.flush().await.unwrap();
        writer
            .finish("cancelled", vec![AgentEvent::Cancelled], None)
            .await
            .unwrap();
        let snapshot = Store::open(path).await.unwrap().load(&id).await.unwrap();
        assert_eq!(snapshot.runs[0].events.len(), 5);
        assert!(matches!(
            snapshot.runs[0].events[1],
            AgentEvent::ReasoningFinished { elapsed_ms: 12 }
        ));
        assert!(matches!(snapshot.runs[0].events[4], AgentEvent::Cancelled));
    }
}
