//! Optional durable storage supplied by the host, paired with a model-facing
//! reader. The agent never removes tool evidence before this write succeeds.
use std::{future::Future, pin::Pin};

use orbit_model::types::ChatMessage;

use crate::error::AgentError;

pub type ArchiveFuture<'a> = Pin<Box<dyn Future<Output = Result<String, AgentError>> + Send + 'a>>;

pub trait ContextArchive: Send + Sync {
    /// Persist a complete snapshot and return model-readable retrieval
    /// instructions. The host must expose the referenced reader in this run.
    fn save<'a>(&'a self, messages: &'a [ChatMessage]) -> ArchiveFuture<'a>;
}
