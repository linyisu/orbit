pub mod linux;
mod native;

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

pub fn execution_backend(
    sandbox: bool,
) -> Result<Box<dyn ExecutionBackend>, crate::error::RuntimeError> {
    if sandbox {
        Ok(Box::new(BwrapBackend::new()?))
    } else {
        Ok(Box::new(native::NativeBackend))
    }
}

/// An explicitly selected backend; sandbox failures never change the selection.
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
