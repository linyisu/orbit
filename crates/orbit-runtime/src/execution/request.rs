use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::verification::{VerificationEvidence, WorkspaceFingerprint};

/// A command and its execution budget. Paths and variables belong to `ExecutionEnvironment`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionRequest {
    pub command: String,
    pub timeout: Duration,
    pub fingerprint: Option<WorkspaceFingerprint>,
    pub workspace_files: usize,
    pub skipped_entries: usize,
}

impl ExecutionRequest {
    pub fn new(command: impl Into<String>, timeout: Duration) -> Self {
        Self {
            command: command.into(),
            timeout,
            fingerprint: None,
            workspace_files: 0,
            skipped_entries: 0,
        }
    }

    pub fn with_verification(
        mut self,
        fingerprint: Option<WorkspaceFingerprint>,
        workspace_files: usize,
        skipped_entries: usize,
    ) -> Self {
        self.fingerprint = fingerprint;
        self.workspace_files = workspace_files;
        self.skipped_entries = skipped_entries;
        self
    }
}

/// Execution outcome with bounded output and optional verification evidence.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub signal: Option<i32>,
    #[serde(default)]
    pub core_dumped: bool,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_output_error: Option<String>,
    pub sandbox: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_error: Option<String>,
    #[serde(skip)]
    pub verification: Option<VerificationEvidence>,
}

impl From<super::process::CommandResult> for ExecutionResult {
    fn from(result: super::process::CommandResult) -> Self {
        Self {
            exit_code: result.exit_code,
            signal: result.signal,
            core_dumped: result.core_dumped,
            elapsed: Duration::from_millis(result.elapsed_ms),
            stdout: result.stdout,
            stderr: result.stderr,
            timed_out: result.timed_out,
            cancelled: result.cancelled,
            output_truncated: result.output_truncated,
            stdout_total_bytes: result.stdout_total_bytes,
            stderr_total_bytes: result.stderr_total_bytes,
            output_id: result.output_id,
            retained_output_truncated: result.retained_output_truncated,
            retained_output_error: result.retained_output_error,
            sandbox: result.sandbox,
            verification_error: result.verification_error,
            verification: result.verification,
        }
    }
}

impl ExecutionResult {
    pub fn display_summary(&self) -> String {
        let code_str = match (self.exit_code, self.signal) {
            _ if self.cancelled => "cancelled".to_string(),
            _ if self.timed_out => "timed out".to_string(),
            (Some(code), _) => format!("exit {code}"),
            (None, Some(signal)) => format!("signal {signal}"),
            (None, None) => "terminated".to_string(),
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
    fn execution_result_summary() {
        let success = ExecutionResult {
            exit_code: Some(0),
            signal: None,
            core_dumped: false,
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
            retained_output_error: None,
            sandbox: "native",
            verification_error: None,
            verification: None,
        };

        assert_eq!(success.display_summary(), "exit 0 · 7 ms");
        assert!(success.is_success());

        let timeout = ExecutionResult {
            exit_code: None,
            elapsed: Duration::from_secs(10),
            timed_out: true,
            ..success
        };

        assert_eq!(timeout.display_summary(), "timed out · 10000 ms");
        assert!(!timeout.is_success());
    }
}
