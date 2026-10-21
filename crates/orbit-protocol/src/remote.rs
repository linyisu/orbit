//! Authenticated remote control envelope. Business requests retain their existing schema.

use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const COMPUTER_SCOPE: &str = "remote_computer_scope_v1";
pub const WORKSPACE_SCOPE: &str = "remote_workspace_scope_v1";
pub const SERVER_NAME: &str = "orbit-host";
pub const MAX_FRAME_BYTES: usize = 2 * 1_024 * 1_024;
pub const ATTACHMENT_CHUNK_BYTES: usize = 128 * 1_024;
pub const ATTACHMENT_TOTAL_BYTES: usize = 20 * 1_024 * 1_024;
pub const ALPN: &[u8] = b"orbit/remote/1";

/// Local-only management. The remote request envelope cannot carry these actions.
#[derive(Debug, Serialize, Deserialize)]
pub enum HostAction {
    Status,
    Enable,
    Disable,
    Invite { permissions: Permissions },
    Confirm { id: String, allow: bool },
    Revoke { device: String },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HostStatus {
    pub enabled: bool,
    pub workspace: String,
    pub invitation: Option<String>,
    pub pending: Vec<String>,
    pub devices: Vec<String>,
    #[serde(default)]
    pub device_names: std::collections::BTreeMap<String, String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Invitation {
    pub version: u32,
    pub address: String,
    pub certificate: String,
    pub token: String,
}

/// Provisioned on the computer and transferred privately to one controller.
/// Debug output intentionally excludes the bearer credential.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pairing {
    #[serde(default)]
    pub host_name: String,
    pub version: u32,
    pub address: String,
    pub certificate: Vec<u8>,
    pub device: String,
    pub secret: String,
}

#[derive(Serialize, Deserialize)]
pub struct Authentication {
    #[serde(default)]
    pub device_name: String,
    pub version: u32,
    pub device: String,
    pub secret: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Permissions {
    pub control: bool,
    pub approve: bool,
}

#[derive(Serialize, Deserialize)]
pub enum Welcome {
    Paired(Pairing),
    Connected {
        workspace: String,
        permissions: Permissions,
    },
    Denied,
}

/// Computer-scoped peers may select directories and forward permitted desktop operations.
/// Host lifecycle and configuration injection remain local-only.
#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    /// Select an existing directory on the paired computer and create a draft session.
    Workspace {
        path: String,
        #[serde(default)]
        resume: Option<String>,
    },
    /// Desktop operations retain their typed request contracts.
    Action {
        request: Box<crate::Request>,
    },
    /// Uploads are connection-owned and discarded on disconnect or submission.
    UploadBegin {
        session: String,
        id: String,
        name: String,
        bytes: usize,
        sha256: String,
        image: bool,
    },
    UploadChunk {
        id: String,
        offset: usize,
        data: String,
    },
    Submit {
        session: String,
        request_id: String,
        prompt: String,
        attachments: Vec<String>,
    },
    Sessions,
    Browse {
        resume: Option<String>,
    },
    Poll {
        session: String,
        after: usize,
    },
    Start {
        session: String,
        request_id: String,
        prompt: String,
    },
    Cancel {
        session: String,
    },
    Approve {
        session: String,
        approval: u64,
        allow: bool,
    },
    AnswerUserInput {
        session: String,
        request: String,
        answers: Option<Vec<crate::user_input::Answer>>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Read,
    Control,
    Approve,
}

/// Closed operation policy, shared by the gateway and the attached workspace service.
/// Host configuration injection, service shutdown and remote-host administration are excluded.
pub fn access(request: &crate::Request) -> Option<(Access, Option<&str>)> {
    use crate::Request as R;
    use Access::*;
    Some(match request {
        R::Sessions | R::AllSessions => (Read, None),
        R::Admin {
            action:
                crate::admin::Action::Projects
                | crate::admin::Action::Sessions
                | crate::admin::Action::SessionStorage
                | crate::admin::Action::InspectSession { .. },
        } => (Read, None),
        R::Admin { .. } | R::Library { .. } => (Control, None),
        R::ReadFile { session, .. }
        | R::ReadToolOutput { session, .. }
        | R::ReadSubagent { session, .. }
        | R::Usage { session }
        | R::UserInputHistory { session, .. }
        | R::Complete { session, .. }
        | R::ListSessions { session }
        | R::ReadQuestions { session, .. }
        | R::Poll { session, .. } => (Read, Some(session)),
        R::ConfigureModels {
            session,
            configuration: None,
        } => (Read, Some(session)),
        R::Files {
            session,
            action: crate::files::Action::List { .. },
            ..
        }
        | R::Review {
            session,
            action:
                crate::review::Action::Status { .. }
                | crate::review::Action::History { .. }
                | crate::review::Action::Commit { .. }
                | crate::review::Action::CommitFile { .. }
                | crate::review::Action::List { .. }
                | crate::review::Action::Diff { .. },
            ..
        } => (Read, Some(session)),
        R::Approve { session, .. } | R::SetMode { session, .. } => (Approve, Some(session)),
        R::Files { session, .. }
        | R::Review { session, .. }
        | R::Tasks { session, .. }
        | R::Subagents { session, .. }
        | R::InputQueue { session, .. }
        | R::Plugins { session, .. }
        | R::Resources { session, .. }
        | R::SetModel { session, .. }
        | R::Plan { session, .. }
        | R::SessionAction { session, .. }
        | R::Undo { session, .. }
        | R::Ask { session, .. }
        | R::SendSubagent { session, .. }
        | R::OpenQuestions { session }
        | R::CloseQuestions { session }
        | R::CancelQuestion { session, .. }
        | R::Steer { session, .. }
        | R::Cancel { session }
        | R::AnswerUserInput { session, .. }
        | R::EnableUserInput { session } => (Control, Some(session)),
        _ => return None,
    })
}

pub type Response = crate::Response<serde_json::Value, serde_json::Value>;

/// Reject oversized remote frames before allocating their body.
pub async fn read<T: serde::de::DeserializeOwned>(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
) -> std::io::Result<T> {
    use tokio::io::AsyncReadExt;
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "remote frame exceeds budget",
        ));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body).map_err(std::io::Error::other)
}

/// Encode before writing; callers must discard the stream on any write failure.
pub async fn write<T: Serialize>(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    value: &T,
) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "remote frame exceeds budget",
        ));
    }
    writer
        .write_u32(bytes.len() as u32)
        .await?;
    writer.write_all(&bytes).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_operation_access_keeps_host_management_local() {
        assert!(access(&crate::Request::Shutdown).is_none());
        assert!(
            access(&crate::Request::RemoteHost {
                action: HostAction::Enable
            })
            .is_none()
        );
        assert_eq!(
            access(&crate::Request::AllSessions),
            Some((Access::Read, None))
        );
        assert_eq!(
            access(&crate::Request::ReadFile {
                session: "synthetic-session".into(),
                path: "sample.txt".into(),
                allow_missing: false,
            }),
            Some((Access::Read, Some("synthetic-session")))
        );
        assert_eq!(
            access(&crate::Request::SetMode {
                session: "synthetic-session".into(),
                mode: crate::Mode::Auto,
            }),
            Some((Access::Approve, Some("synthetic-session")))
        );
    }
}
