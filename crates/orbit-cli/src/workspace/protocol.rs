use std::io;

use orbit_agent::events::AgentEvent;
use orbit_tools::approval::ApprovalPreview;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u32 = 2;
// Keep framing stable so Hello can report incompatible schema versions.
pub const PREAMBLE: &[u8] = b"\0ORBIT-WORKSPACE/1\n";
pub const MAX_FRAME_BYTES: usize = 32 * 1024 * 1024;
pub const PAGE_BYTES: usize = 1024 * 1024;
pub const PAGE_EVENTS: usize = 128;

pub async fn read_preamble(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<()> {
    let mut matched = 0;
    for _ in 0..16384 {
        let byte = reader.read_u8().await?;
        matched = if byte == PREAMBLE[matched] {
            matched + 1
        } else {
            usize::from(byte == PREAMBLE[0])
        };
        if matched == PREAMBLE.len() {
            return Ok(());
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "workspace protocol marker was not found",
    ))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Request {
    Hello {
        version: u32,
    },
    Open {
        resume: Option<String>,
        #[serde(default)]
        configuration: Option<super::configuration::Configuration>,
    },
    Sessions,
    Start {
        session: String,
        request_id: String,
        prompt: String,
        mode: Mode,
        model: Option<String>,
    },
    Poll {
        session: String,
        after: usize,
    },
    Cancel {
        session: String,
    },
    Approve {
        session: String,
        approval: u64,
        allow: bool,
        remember: bool,
    },
    ReadFile {
        session: String,
        path: String,
    },
    Shutdown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Ask,
    #[default]
    Auto,
    Yolo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub workspace: String,
    pub model: String,
    pub models: Vec<String>,
    pub context_tokens: usize,
    pub input_context_tokens: usize,
    pub max_output_tokens: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Update {
    Profile(SessionInfo),
    Started { prompt: String },
    Agent(Box<AgentEvent>),
    Finished { status: String },
}

impl Update {
    pub fn agent(event: AgentEvent) -> Self {
        Self::Agent(Box::new(event))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Approval {
    pub id: u64,
    pub preview: ApprovalPreview,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Page {
    pub next: usize,
    pub more: bool,
    pub busy: bool,
    pub model: String,
    pub updates: Vec<Update>,
    pub approvals: Vec<Approval>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Response {
    Hello { version: u32, workspace: String },
    Opened(SessionInfo),
    Sessions(Vec<orbit_store::types::Session>),
    Page(Page),
    File(String),
    Ok,
    Error(String),
}

pub async fn read<T: DeserializeOwned>(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<T> {
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid workspace frame size",
        ));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub async fn write<T: Serialize>(
    writer: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "workspace frame exceeds limit",
        ));
    }
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_survive_fragmentation_and_reject_oversized_or_truncated_input() {
        let (mut writer, mut reader) = tokio::io::duplex(3);
        let task = tokio::spawn(async move {
            write(
                &mut writer,
                &Request::Open {
                    configuration: None,
                    resume: Some("中文\n会话".into()),
                },
            )
            .await
            .unwrap();
            write(&mut writer, &Request::Hello { version: VERSION })
                .await
                .unwrap();
        });
        assert!(
            matches!(read::<Request>(&mut reader).await.unwrap(), Request::Open { resume: Some(id), .. } if id == "中文\n会话")
        );
        assert!(matches!(
            read::<Request>(&mut reader).await.unwrap(),
            Request::Hello { version: VERSION }
        ));
        task.await.unwrap();
        let mut oversized = ((MAX_FRAME_BYTES + 1) as u32)
            .to_be_bytes()
            .as_slice()
            .to_owned();
        assert_eq!(
            read::<Request>(&mut oversized.as_slice())
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        oversized = vec![0, 0, 0, 10, b'{'];
        assert_eq!(
            read::<Request>(&mut oversized.as_slice())
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
