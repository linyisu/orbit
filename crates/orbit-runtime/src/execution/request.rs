use std::{path::PathBuf, time::Duration};

use serde::{Deserialize, Serialize};

use crate::verification::VerificationEvidence;

/// ExecutionRequest encapsulates a command execution request with its working
/// directory, timeouts, and optional session scratch storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionRequest {
    pub command: String,
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub foreground_wait: Option<Duration>,
    pub scratch: Option<PathBuf>,
}

impl ExecutionRequest {
    pub fn new(command: impl Into<String>, cwd: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            command: command.into(),
            cwd: cwd.into(),
            timeout,
            foreground_wait: None,
            scratch: None,
        }
    }

    pub fn with_foreground_wait(mut self, wait: Duration) -> Self {
        self.foreground_wait = Some(wait);
        self
    }

    pub fn with_scratch(mut self, scratch: impl Into<PathBuf>) -> Self {
        self.scratch = Some(scratch.into());
        self
    }
}

/// ExecutionResult captures the outcome of a command execution, including
/// exit status, timing, output streams, truncation, and optional verification evidence.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub exit_code: Option<i32>,
    pub elapsed: Duration,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
    pub output_truncated: bool,
    pub stdout_total_bytes: u64,
    pub stderr_total_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_id: Option<String>,
    pub retained_output_truncated: bool,
    pub sandbox: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_error: Option<String>,
    #[serde(skip)]
    pub verification: Option<VerificationEvidence>,
}

impl ExecutionResult {
    /// Formats a concise human-readable summary without leaking redundant internal flags.
    pub fn display_summary(&self) -> String {
        let code_str = match self.exit_code {
            Some(code) => format!("exit {code}"),
            None if self.timed_out => "timed out".to_string(),
            None if self.cancelled => "cancelled".to_string(),
            None => "terminated".to_string(),
        };

        format!("{code_str} · {} ms", self.elapsed.as_millis())
    }

    pub fn is_success(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out && !self.cancelled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_request_builders() {
        let req = ExecutionRequest::new("cargo test", "/workspace", Duration::from_secs(30))
            .with_foreground_wait(Duration::from_millis(500))
            .with_scratch("/scratch");

        assert_eq!(req.command, "cargo test");
        assert_eq!(req.cwd, PathBuf::from("/workspace"));
        assert_eq!(req.timeout, Duration::from_secs(30));
        assert_eq!(req.foreground_wait, Some(Duration::from_millis(500)));
        assert_eq!(req.scratch, Some(PathBuf::from("/scratch")));
    }

    #[test]
    fn execution_result_summary() {
        let success = ExecutionResult {
            exit_code: Some(0),
            elapsed: Duration::from_millis(7),
            stdout: "hello".into(),
            stderr: String::new(),
            timed_out: false,
            cancelled: false,
            output_truncated: false,
            stdout_total_bytes: 5,
            stderr_total_bytes: 0,
            output_id: None,
            retained_output_truncated: false,
            sandbox: "bwrap",
            verification_error: None,
            verification: None,
        };

        assert_eq!(success.display_summary(), "exit 0 · 7 ms");
        assert!(success.is_success());

        let timeout = ExecutionResult {
            exit_code: None,
            elapsed: Duration::from_secs(10),
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
            cancelled: false,
            output_truncated: false,
            stdout_total_bytes: 0,
            stderr_total_bytes: 0,
            output_id: None,
            retained_output_truncated: false,
            sandbox: "bwrap",
            verification_error: None,
            verification: None,
        };

        assert_eq!(timeout.display_summary(), "timed out · 10000 ms");
        assert!(!timeout.is_success());
    }
}
