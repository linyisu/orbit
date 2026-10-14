mod native;

use std::{future::Future, pin::Pin};

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

pub use native::NativeBackend;

pub fn execution_backend() -> Box<dyn ExecutionBackend> {
    Box::new(native::NativeBackend)
}

/// Process execution interface shared by command and helper workers.
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
    ) -> Pin<Box<dyn Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>>;
}
