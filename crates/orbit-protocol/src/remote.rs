//! Authenticated remote control envelope. Business requests retain their existing schema.

use serde::{Deserialize, Serialize};

pub const VERSION: u32 = 1;
pub const WORKSPACE_SCOPE: &str = "remote_workspace_scope_v1";
pub const SERVER_NAME: &str = "orbit-host";
pub const MAX_FRAME_BYTES: usize = 2 * 1_024 * 1_024;

/// Provisioned on the computer and transferred privately to one controller.
/// Debug output intentionally excludes the bearer credential.
#[derive(Clone, Serialize, Deserialize)]
pub struct Pairing {
    pub version: u32,
    pub address: String,
    pub certificate: Vec<u8>,
    pub device: String,
    pub secret: String,
}

#[derive(Serialize, Deserialize)]
pub struct Authentication {
    pub version: u32,
    pub device: String,
    pub secret: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Permissions {
    pub control: bool,
    pub approve: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Welcome {
    Connected {
        workspace: String,
        permissions: Permissions,
    },
    Denied,
}

/// Remote peers cannot supply host configuration, filesystem paths or administrative actions.
#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
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
