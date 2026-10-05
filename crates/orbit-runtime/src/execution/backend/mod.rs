pub mod linux;

use std::pin::Pin;

use tokio_util::sync::CancellationToken;

use crate::{
    error::RuntimeError,
    execution::{
        environment::ExecutionEnvironment,
        output::OutputLog,
        policy::ExecutionPolicy,
        request::{ExecutionRequest, ExecutionResult},
    },
};

pub use linux::BwrapBackend;

/// Isolation backends must fail closed rather than fall back to host execution.
pub trait ExecutionBackend: Send + Sync {
    fn name(&self) -> &'static str;

    fn is_available(&self) -> Result<(), RuntimeError>;

    fn execute<'a>(
        &'a self,
        request: &'a ExecutionRequest,
        policy: &'a ExecutionPolicy,
        env: &'a ExecutionEnvironment,
        output_log: Option<OutputLog>,
        cancellation: CancellationToken,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>>;
}
