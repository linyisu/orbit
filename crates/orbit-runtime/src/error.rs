use crate::filesystem::WorkspaceError;
use std::io;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("execution cancelled")]
    Cancelled,

    #[error("execution failed: {0}")]
    ExecutionFailed(String),

    #[error(transparent)]
    Workspace(#[from] WorkspaceError),

    #[error(transparent)]
    Io(#[from] io::Error),
}

pub(crate) fn failed(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::ExecutionFailed(error.to_string())
}
