pub mod admin;
pub mod plugins;
pub mod project;
pub mod search;

use std::io;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const VERSION: u32 = 6;
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
    RestoreSessions,
    Complete {
        session: String,
        input: String,
    },
    Hello {
        version: u32,
        #[serde(default)]
        workspace: Option<String>,
    },
    Open {
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        policy: SessionPolicy,
        resume: Option<String>,
        #[serde(default)]
        configuration: Option<Configuration>,
    },
    Sessions,
    ListSessions {
        session: String,
    },
    Plugins {
        session: String,
        action: plugins::Action,
    },
    Admin {
        action: admin::Action,
    },
    Start {
        session: String,
        request_id: String,
        prompt: String,
        #[serde(default)]
        images: Vec<ImageUpload>,
        #[serde(default)]
        compact: bool,
        mode: Mode,
        model: Option<String>,
    },
    Ask {
        session: String,
        question: String,
        follow_up: Option<usize>,
    },
    ReadQuestions {
        session: String,
        after: Option<u64>,
    },
    CancelQuestion {
        session: String,
        index: usize,
    },
    SessionAction {
        session: String,
        action: SessionAction,
    },
    ConfigureModels {
        session: String,
        configuration: Option<Configuration>,
    },
    SetMode {
        session: String,
        mode: Mode,
    },
    SetModel {
        session: String,
        model: String,
        effort: Option<String>,
    },
    Steer {
        session: String,
        message: String,
    },
    Poll {
        session: String,
        after: usize,
    },
    Undo {
        session: String,
        path: Option<String>,
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
    ReadSubagent {
        session: String,
        agent: String,
        after: usize,
    },
    SendSubagent {
        session: String,
        agent: String,
        message: String,
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
    pub title: String,
    pub workspace: String,
    #[serde(default)]
    pub home: Option<String>,
    pub model: String,
    pub models: Vec<String>,
    #[serde(default)]
    pub model_catalog: Vec<ModelSummary>,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub cursor_trail: bool,
    #[serde(default)]
    pub auto_compact: bool,
    #[serde(default)]
    pub skills: Vec<(String, String)>,
    pub supports_vision: bool,
    pub reasoning_effort: Option<String>,
    pub reasoning_efforts: Vec<String>,
    pub context_tokens: usize,
    pub input_context_tokens: usize,
    pub max_output_tokens: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ModelSummary {
    pub provider: String,
    pub model: String,
    pub supports_vision: bool,
    pub reasoning_effort: Option<String>,
    pub reasoning_efforts: Vec<String>,
}

impl ModelSummary {
    pub fn qualified_name(&self) -> String {
        format!("{}::{}", self.provider, self.model)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RunPresentation {
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default)]
    pub compact: bool,
    #[serde(default)]
    pub queued_input: bool,
    #[serde(default)]
    pub replayed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunTiming {
    pub elapsed_ms: u64,
    pub paused: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Update<E> {
    Profile(Box<SessionInfo>),
    Report(Report),
    Started {
        prompt: String,
        #[serde(default)]
        presentation: RunPresentation,
    },
    Agent(Box<E>),
    Finished {
        status: String,
        #[serde(default)]
        elapsed_ms: Option<u64>,
    },
}

impl<E> Update<E> {
    pub fn agent(event: E) -> Self {
        Self::Agent(Box::new(event))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Approval<A> {
    pub id: u64,
    pub preview: A,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Page<E, A> {
    #[serde(default)]
    pub timing: Option<RunTiming>,
    pub next: usize,
    pub more: bool,
    pub busy: bool,
    pub model: String,
    pub updates: Vec<Update<E>>,
    pub approvals: Vec<Approval<A>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Response<E, A> {
    Completions {
        input: String,
        entries: Vec<Completion>,
    },
    Hello {
        version: u32,
        workspace: String,
        capabilities: Capabilities,
    },
    Opened(SessionInfo),
    Sessions(Vec<Session>),
    Page(Page<E, A>),
    Subagent(Page<E, A>),
    Questions(Questions),
    Report(Report),
    Plugins(plugins::Catalog),
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
                    model: Some("test::model".into()),
                    policy: Default::default(),
                    configuration: None,
                    resume: Some("中文\n会话".into()),
                },
            )
            .await
            .unwrap();
            write(
                &mut writer,
                &Request::Hello {
                    version: VERSION,
                    workspace: None,
                },
            )
            .await
            .unwrap();
        });
        assert!(
            matches!(read::<Request>(&mut reader).await.unwrap(), Request::Open { resume: Some(id), .. } if id == "中文\n会话")
        );
        assert!(matches!(
            read::<Request>(&mut reader).await.unwrap(),
            Request::Hello {
                version: VERSION,
                workspace: None
            }
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub workspace: String,
    pub project_id: String,
    pub model: String,
    /// The selected reasoning setting, stored independently from provider
    /// configuration so a resumed conversation keeps its own preference.
    pub reasoning_effort: Option<String>,
    pub demo: bool,
    pub title: String,
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize)]
pub struct Configuration {
    pub content: String,
    pub credentials: std::collections::BTreeMap<String, serde_json::Value>,
}

impl std::fmt::Debug for Configuration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Configuration([redacted])")
    }
}

pub fn request_id() -> String {
    use std::hash::BuildHasher;
    let state = std::collections::hash_map::RandomState::new();
    format!(
        "{:016x}{:016x}",
        state.hash_one(std::time::SystemTime::now()),
        state.hash_one(std::process::id())
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Capabilities {
    pub system: String,
    pub architecture: String,
    pub shell: String,
    pub native_processes: bool,
    pub files: bool,
    pub sandbox: Availability,
    pub features: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", content = "reason", rename_all = "snake_case")]
pub enum Availability {
    Available,
    Unavailable(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageUpload {
    pub name: String,
    pub data: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SideQuestion {
    #[serde(default)]
    pub thread_id: String,
    pub model: String,
    pub question: String,
    pub answer: String,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Questions {
    pub revision: u64,
    pub selected: Option<usize>,
    pub running: bool,
    pub records: Option<Vec<SideQuestion>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SessionAction {
    ChangeDirectory { path: String },
    RefreshSkills,
    Task { task: project::TaskAction },
    Memory { memory: project::MemoryAction },
    Inspect { section: Inspection },
    Rename { title: String },
    LoadSkill { name: String },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "section", rename_all = "snake_case")]
pub enum Inspection {
    ExportPatch,
    Status,
    Context,
    Skills,
    Mcp,
    Diagnostics,
    Changes,
    Diff { path: Option<String> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Report {
    pub title: String,
    pub text: String,
}
/// Capability categories omitted from both tool registration and model requests.
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Every optional tool group below.
    All,
    /// Independent read-only delegated sessions.
    Subagents,
    /// web_search and web_fetch.
    Web,
    /// Configured local MCP processes and their tools.
    Mcp,
    /// Installed skill catalogue and load_skill.
    Skills,
    /// Native image attachments and read_image.
    Image,
    /// read_document.
    Document,
    /// read_file, list_files, search_text and project_overview.
    Files,
    /// git_inspect and git_export_patch.
    Git,
    /// write_file and edit_file.
    Write,
    /// request_capability for paths outside the workspace.
    Capability,
    /// run_command, read_command_output, verification_status and export_artifact.
    Command,
    /// Archived context retrieval and tool-output pruning.
    History,
    /// task and durable-memory tools.
    ProjectState,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionPolicy {
    #[serde(default)]
    pub read_only: bool,
    #[serde(default)]
    pub disabled: Vec<Capability>,
}

impl SessionPolicy {
    pub fn disabled(&self, capability: Capability) -> bool {
        self.disabled.contains(&Capability::All) || self.disabled.contains(&capability)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Completion {
    pub name: String,
    pub display: Option<String>,
    pub description: String,
    pub continue_input: bool,
}
