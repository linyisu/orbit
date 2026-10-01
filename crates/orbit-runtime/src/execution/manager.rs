use std::{path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

use crate::{
    error::RuntimeError,
    execution::{
        backend::{BwrapBackend, ExecutionBackend},
        environment::ExecutionEnvironment,
        output::OutputLog,
        policy::ExecutionPolicy,
        request::{ExecutionRequest, ExecutionResult},
    },
};

/// ExecutionManager orchestrates command launches, cancellation, logging, and cleanup
/// across supported execution backends.
pub struct ExecutionManager {
    backend: Arc<dyn ExecutionBackend>,
}

impl ExecutionManager {
    pub fn new(backend: Arc<dyn ExecutionBackend>) -> Self {
        Self { backend }
    }

    /// Initializes an ExecutionManager using the default Linux Bubblewrap backend.
    pub fn linux() -> Result<Self, RuntimeError> {
        let backend = BwrapBackend::new()?;
        Ok(Self::new(Arc::new(backend)))
    }

    pub fn backend(&self) -> &dyn ExecutionBackend {
        self.backend.as_ref()
    }

    /// Runs a command synchronously or with foreground timeout, returning the execution result.
    pub async fn execute(
        &self,
        request: &ExecutionRequest,
        policy: &ExecutionPolicy,
        env: &ExecutionEnvironment,
        output_directory: Option<&Path>,
        cancellation: CancellationToken,
    ) -> Result<ExecutionResult, RuntimeError> {
        let output_log = match output_directory {
            Some(dir) => Some(OutputLog::create(dir).await?),
            None => None,
        };

        self.backend
            .execute(request, policy, env, output_log, cancellation)
            .await
    }
}
