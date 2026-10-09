//! Host-owned projections refreshed at model-request boundaries.
use crate::error::AgentError;
use orbit_model::types::RuntimeStateKind;
use std::{future::Future, pin::Pin};

pub type StateFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<(RuntimeStateKind, String)>, AgentError>> + Send + 'a>>;
pub trait StateSource: Send + Sync {
    fn snapshot(&self) -> StateFuture<'_>;
}
