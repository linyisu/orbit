//! Bounded live delivery cache. Cold records retain their original cursors in
//! anonymous temporary files, independently of the compacted durable ledger.
use std::{
    collections::VecDeque,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    sync::{Arc, Mutex},
};

use super::Update;
use anyhow::{Context, Result, ensure};
#[cfg(test)]
use orbit_protocol::PAGE_BYTES;
use orbit_protocol::PAGE_EVENTS;

pub(super) const MAX_CACHED_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_CACHED_EVENTS: usize = 2048;

#[derive(Default)]
pub(super) struct Log {
    cache: VecDeque<(Update, usize)>,
    bytes: usize,
    base: usize,
    disk: Option<Arc<Mutex<Spool>>>,
    fault: Option<String>,
}

struct Spool {
    data: File,
    index: File,
    end: u64,
}

impl Spool {
    fn new() -> Result<Self> {
        // tempfile honors the platform temporary directory and unlinks on
        // creation where supported. No draft or per-session artifact directory.
        Ok(Self {
            data: tempfile::tempfile()?,
            index: tempfile::tempfile()?,
            end: 0,
        })
    }

    fn append(&mut self, ordinal: usize, update: &Update) -> Result<()> {
        let bytes = serde_json::to_vec(update)?;
        self.data.seek(SeekFrom::Start(self.end))?;
        self.data.write_all(&bytes)?;
        self.index.seek(SeekFrom::Start(ordinal as u64 * 16))?;
        self.index.write_all(&self.end.to_le_bytes())?;
        self.index.write_all(&(bytes.len() as u64).to_le_bytes())?;
        self.end += bytes.len() as u64;
        Ok(())
    }

    fn read(&mut self, ordinal: usize) -> Result<(Update, usize)> {
        self.index.seek(SeekFrom::Start(ordinal as u64 * 16))?;
        let mut entry = [0; 16];
        self.index.read_exact(&mut entry)?;
        let offset = u64::from_le_bytes(entry[..8].try_into()?);
        let length = u64::from_le_bytes(entry[8..].try_into()?) as usize;
        self.data.seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; length];
        self.data.read_exact(&mut bytes)?;
        Ok((serde_json::from_slice(&bytes)?, length))
    }
}

pub(super) struct ReadPage {
    disk: Option<Arc<Mutex<Spool>>>,
    range: std::ops::Range<usize>,
    cached: Vec<(Update, usize)>,
}

impl ReadPage {
    pub(super) fn read(self, limit: usize) -> Result<Vec<Update>> {
        let mut updates = Vec::new();
        let mut bytes = 0;
        for ordinal in self.range {
            let (update, size) = self
                .disk
                .as_ref()
                .context("missing delivery spool")?
                .lock()
                .map_err(|_| anyhow::anyhow!("delivery spool lock poisoned"))?
                .read(ordinal)?;
            if bytes > 0 && bytes + size > limit {
                return Ok(updates);
            }
            bytes += size;
            updates.push(update);
        }
        for (update, size) in self.cached {
            if bytes > 0 && bytes + size > limit {
                break;
            }
            bytes += size;
            updates.push(update);
        }
        Ok(updates)
    }
}

impl Log {
    pub(super) fn len(&self) -> usize {
        self.base + self.cache.len()
    }

    pub(super) fn push(&mut self, update: Update, published: usize) {
        if self.fault.is_some() {
            return;
        }
        if let Err(error) = self.append(update, published) {
            // Never silently drop a cursor or panic on ENOSPC. Poll reports the
            // error; reopening replays the canonical ledger, which is separate.
            self.fault = Some(format!("实时投递缓存写入失败，请重新打开会话：{error:#}"));
        }
    }

    fn append(&mut self, update: Update, published: usize) -> Result<()> {
        let mut merged = false;
        if self.len() > published
            && let Some((Update::Agent(previous), size)) = self.cache.back_mut()
            && let Update::Agent(next) = &update
        {
            let added = match next.as_ref() {
                orbit_agent::events::AgentEvent::Model(
                    orbit_model::types::StreamEvent::TextDelta(text)
                    | orbit_model::types::StreamEvent::ReasoningDelta(text),
                ) => Some(serde_json::to_vec(text)?.len().saturating_sub(2)),
                _ => None,
            };
            if orbit_agent::events::coalesce(previous, next) {
                self.bytes -= *size;
                *size = match added {
                    Some(added) => *size + added,
                    None => {
                        serde_json::to_vec(&orbit_protocol::Update::agent(previous.as_ref()))?.len()
                    }
                };
                self.bytes += *size;
                merged = true;
            }
        }
        if !merged {
            let size = serde_json::to_vec(&update)?.len();
            self.bytes += size;
            self.cache.push_back((update, size));
        }
        while self.bytes > MAX_CACHED_BYTES || self.cache.len() > MAX_CACHED_EVENTS {
            if self.disk.is_none() {
                self.disk = Some(Arc::new(Mutex::new(Spool::new()?)));
            }
            let (update, _) = self.cache.front().context("empty delivery cache")?;
            self.disk
                .as_ref()
                .unwrap()
                .lock()
                .map_err(|_| anyhow::anyhow!("delivery spool lock poisoned"))?
                .append(self.base, update)?;
            let (_, size) = self.cache.pop_front().unwrap();
            self.bytes -= size;
            self.base += 1;
        }
        Ok(())
    }

    pub(super) fn reader(&self, after: usize) -> Result<ReadPage> {
        ensure!(
            after <= self.len(),
            "event cursor no longer exists; reopen the session"
        );
        if let Some(error) = &self.fault {
            anyhow::bail!("{error}");
        }
        let disk_end = self.base.min(after.saturating_add(PAGE_EVENTS));
        let range = after..disk_end.max(after);
        let remaining = PAGE_EVENTS - range.len();
        let cached = self
            .cache
            .iter()
            .skip(after.saturating_sub(self.base))
            .take(remaining)
            .cloned()
            .collect();
        Ok(ReadPage {
            disk: self.disk.clone(),
            range,
            cached,
        })
    }

    #[cfg(test)]
    pub(super) fn page(&self, after: usize) -> Result<Vec<Update>> {
        self.reader(after)?.read(PAGE_BYTES)
    }

    #[cfg(test)]
    pub(super) fn cached(&self) -> &VecDeque<(Update, usize)> {
        &self.cache
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_agent::events::AgentEvent;
    use orbit_model::types::StreamEvent;

    #[test]
    fn slow_viewers_replay_evicted_records_and_readers_survive_concurrent_eviction() {
        let mut log = Log::default();
        let mut expected = Vec::new();
        for index in 0..MAX_CACHED_EVENTS + 300 {
            let event = Update::agent(AgentEvent::ReasoningFinished {
                elapsed_ms: index as u64,
            });
            expected.push(serde_json::to_value(&event).unwrap());
            log.push(event, log.len());
        }
        assert!(log.disk.is_some());
        let reader = log.reader(log.base - 2).unwrap();
        let read_start = log.base - 2;
        for index in 0..300 {
            let event = Update::agent(AgentEvent::Model(StreamEvent::TextDelta(format!(
                "{index}:{}",
                "中".repeat(24_000)
            ))));
            expected.push(serde_json::to_value(&event).unwrap());
            log.push(event, log.len());
            assert!(log.bytes <= MAX_CACHED_BYTES);
            assert!(log.cache.len() <= MAX_CACHED_EVENTS);
            assert!(log.fault.is_none());
        }
        let earlier = reader.read(PAGE_BYTES).unwrap();
        let earlier = serde_json::to_value(&earlier).unwrap();
        assert_eq!(
            earlier,
            serde_json::to_value(&expected[read_start..read_start + PAGE_EVENTS]).unwrap()
        );
        let mut actual = Vec::new();
        let mut cursor = 0;
        while cursor < log.len() {
            let page = log.reader(cursor).unwrap().read(PAGE_BYTES / 2).unwrap();
            assert!(!page.is_empty());
            cursor += page.len();
            actual.extend(
                page.into_iter()
                    .map(|event| serde_json::to_value(event).unwrap()),
            );
        }
        assert_eq!(actual, expected);
        assert!(log.page(cursor).unwrap().is_empty());
        assert!(log.page(cursor + 1).is_err());
    }
}
