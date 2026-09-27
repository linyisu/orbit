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
    pub command: String,
    pub timeout_seconds: u64,
}

#[derive(Clone, Debug)]
pub enum ApprovalPreview {
    Write(WritePreview),
    Delete(DeletePreview),
    Command(CommandPreview),
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
