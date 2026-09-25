//! Optional durable storage supplied by the host, paired with a model-facing
//! reader. The agent never removes tool evidence before this write succeeds.
use std::{future::Future, pin::Pin};

use orbit_model::types::ChatMessage;

use crate::error::AgentError;
use crate::memory::SummaryProgress;

pub type ArchiveFuture<'a> = Pin<Box<dyn Future<Output = Result<String, AgentError>> + Send + 'a>>;

pub type ProgressLoadFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<SummaryProgress>, AgentError>> + Send + 'a>>;

pub type ProgressSaveFuture<'a> = Pin<Box<dyn Future<Output = Result<(), AgentError>> + Send + 'a>>;

pub trait ContextArchive: Send + Sync {
    /// Persist a complete snapshot and return model-readable retrieval
    /// instructions. The host must expose the referenced reader in this run.
    fn save<'a>(&'a self, messages: &'a [ChatMessage]) -> ArchiveFuture<'a>;

    fn load_progress<'a>(&'a self, _fingerprint: &'a str) -> ProgressLoadFuture<'a> {
        Box::pin(async { Ok(None) })
    }

    fn save_progress<'a>(&'a self, _progress: &'a SummaryProgress) -> ProgressSaveFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}
