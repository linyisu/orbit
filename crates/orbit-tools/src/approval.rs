use tokio::sync::{mpsc, oneshot};

#[derive(Clone, Debug)]
pub struct WritePreview {
    pub path: String,
    pub before: Option<String>,
    pub after: String,
}

pub struct WriteApproval {
    pub preview: WritePreview,
    pub decision: oneshot::Sender<bool>,
}

pub type ApprovalSender = mpsc::Sender<WriteApproval>;
pub type ApprovalReceiver = mpsc::Receiver<WriteApproval>;

pub fn channel() -> (ApprovalSender, ApprovalReceiver) {
    mpsc::channel(1)
}
