use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Debug)]
pub struct WritePreview {
    pub path: String,
    pub before: Option<String>,
    pub after: String,
}

#[derive(Clone, Debug)]
pub struct DeletePreview {
    pub path: String,
    pub before: String,
}

#[derive(Clone, Debug)]
pub struct CommandPreview {
    pub writable_snapshot: bool,
    pub command: String,
    pub timeout_seconds: u64,
}

/// An MCP server is an external program with capabilities Orbit cannot
/// reliably infer from its advertised schema. Keep every invocation behind
/// the same explicit approval boundary as other side-effecting tools.
#[derive(Clone, Debug)]
pub struct McpPreview {
    pub server: String,
    pub tool: String,
    pub arguments: String,
}

#[derive(Clone, Debug)]
pub struct CapabilityPreview {
    pub environment: orbit_runtime::capability::EnvironmentId,
    pub path: std::path::PathBuf,
    pub access: orbit_runtime::capability::FilesystemAccess,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub enum ApprovalPreview {
    Write(WritePreview),
    Delete(DeletePreview),
    Command(CommandPreview),
    Mcp(McpPreview),
    Capability(CapabilityPreview),
}

pub struct ApprovalRequest {
    pub preview: ApprovalPreview,
    pub decision: oneshot::Sender<bool>,
}

pub type ApprovalSender = mpsc::Sender<ApprovalRequest>;
pub type ApprovalReceiver = mpsc::Receiver<ApprovalRequest>;

pub fn channel() -> (ApprovalSender, ApprovalReceiver) {
    mpsc::channel(1)
}
