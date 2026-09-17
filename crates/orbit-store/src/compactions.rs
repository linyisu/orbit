//! Read-only audit view over durable compaction events. No project-side files.
use anyhow::Result;
use orbit_agent::memory::CompactionRecord;
use serde::{Deserialize, Serialize};

use crate::store::Store;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredCompaction {
    pub event_id: i64,
    pub run_id: i64,
    /// Only completed runs publish their final model context. A record in a
    /// failed/interrupted run is still useful evidence, not a resume checkpoint.
    pub run_status: String,
    pub record: CompactionRecord,
}

impl Store {
    pub async fn compactions(&self, session_id: &str) -> Result<Vec<StoredCompaction>> {
        let id = session_id.to_owned();
        self.access(move |conn| {
            conn.query_row("SELECT id FROM sessions WHERE id=?1", [&id], |_| Ok(()))?;
            let mut query = conn.prepare("SELECT e.id,e.run_id,r.status,json_extract(e.payload,'$.CompactionRecorded.record') FROM events e JOIN runs r ON r.id=e.run_id WHERE r.session_id=?1 AND json_type(e.payload,'$.CompactionRecorded.record')='object' ORDER BY e.id")?;
            let rows = query.query_map([id], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?)))?;
            let mut records = Vec::new();
            for row in rows {
                let (event_id, run_id, run_status, record) = row?;
                records.push(StoredCompaction { event_id, run_id, run_status, record: serde_json::from_str(&record)? });
            }
            Ok(records)
        }).await
    }
}
