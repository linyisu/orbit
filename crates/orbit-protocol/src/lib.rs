//! Versioned workspace requests, events, pagination and transport framing.
//! Schema changes must preserve the declared compatibility floor.

pub mod admin;
pub mod files;
pub mod input_queue;
pub mod plan;
pub mod plugins;
pub mod project;
pub mod resources;
pub mod review;
pub mod search;
pub mod subagents;
pub mod usage;
pub mod user_input;

use std::io;

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Current request and event schema version advertised during the handshake.
pub const VERSION: u32 = 27;
/// Oldest peer schema accepted by this implementation.
pub const MIN_COMPATIBLE_VERSION: u32 = 15;

/// Accept peers whose schema version lies within the supported compatibility range.
pub fn supports_version(version: u32) -> bool {
    (MIN_COMPATIBLE_VERSION..=VERSION).contains(&version)
}
/// Stable transport marker used before schema compatibility is negotiated.
pub const PREAMBLE: &[u8] = b"\0ORBIT-WORKSPACE/1\n";
/// Maximum encoded JSON payload accepted for one transport frame.
pub const MAX_FRAME_BYTES: usize = 32 * 1_024 * 1_024;
/// Default byte budget for paginated history responses.
pub const PAGE_BYTES: usize = 1_024 * 1_024;
/// Default event count requested in one history page.
pub const PAGE_EVENTS: usize = 128;

const MAX_PREAMBLE_SCAN_BYTES: usize = 16 * 1_024;

/// Find the transport marker after bounded startup noise from SSH or shell initialization.
pub async fn read_preamble(reader: &mut (impl AsyncRead + Unpin)) -> io::Result<()> {
    let mut matched = 0;
    for _ in 0..MAX_PREAMBLE_SCAN_BYTES {
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
    Usage {
        session: String,
    },
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
    /// All resumable conversations on this host, across project directories.
    AllSessions,
    ListSessions {
        session: String,
    },
    Plugins {
        session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_workspace: Option<String>,
        action: plugins::Action,
    },
    Resources {
        session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_workspace: Option<String>,
        action: resources::Action,
    },
    /// Structured project tasks for desktop clients. SessionAction::Task
    /// continues to return a report for terminal clients.
    Tasks {
        session: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_workspace: Option<String>,
        action: project::TaskAction,
    },
    Admin {
        action: admin::Action,
    },
    Review {
        session: String,
        expected_workspace: String,
        action: review::Action,
    },
    Subagents {
        session: String,
        expected_workspace: String,
        action: subagents::Action,
    },
    InputQueue {
        session: String,
        expected_workspace: String,
        action: input_queue::Action,
    },
    Files {
        session: String,
        expected_workspace: Option<String>,
        action: files::Action,
    },
    /// Submit client-owned files with the same durable start or queue semantics as images.
    SubmitAttachments {
        session: String,
        request_id: String,
        prompt: String,
        images: Vec<ImageUpload>,
        files: Vec<FileUpload>,
        steer: bool,
    },
    Start {
        session: String,
        request_id: String,
        prompt: String,
        #[serde(default)]
        images: Vec<ImageUpload>,
        /// Compact existing history instead of submitting a prompt; empty text is valid.
        #[serde(default)]
        compact: bool,
        model: Option<String>,
    },
    Ask {
        session: String,
        question: String,
        follow_up: Option<usize>,
    },
    Plan {
        session: String,
        action: plan::Action,
    },
    EnableUserInput {
        session: String,
    },
    AnswerUserInput {
        session: String,
        request: String,
        answers: Option<Vec<user_input::Answer>>,
    },
    UserInputHistory {
        session: String,
        before: Option<i64>,
    },
    OpenQuestions {
        session: String,
    },
    CloseQuestions {
        session: String,
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
    SteerImages {
        session: String,
        message: String,
        images: Vec<ImageUpload>,
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
        /// Opt in to FileMissing for absent files; omitted by older peers.
        #[serde(default)]
        allow_missing: bool,
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

impl Mode {
    /// Parse the stable permission-mode names, independent of presentation labels.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "ask" => Some(Self::Ask),
            "auto" => Some(Self::Auto),
            "yolo" => Some(Self::Yolo),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    /// False until the in-memory draft has gained durable content.
    #[serde(default)]
    pub persisted: bool,
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
    pub mode_revision: u64,
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
    /// History and tool schemas at publication, using the selected model's calibration.
    /// Older services omit this snapshot; clients may fall back to event evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_used_tokens: Option<usize>,
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

/// An image stored on the session's host and readable through its file client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageImage {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RunPresentation {
    #[serde(default)]
    pub file_assets: Vec<MessageImage>,
    #[serde(default)]
    pub image_assets: Vec<MessageImage>,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolProgress {
    pub call_id: String,
    pub stdout: Option<String>,
    pub elapsed_ms: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Page<E, A> {
    #[serde(default)]
    pub planning: plan::State,
    #[serde(default)]
    pub tool_progress: Vec<ToolProgress>,
    #[serde(default)]
    pub timing: Option<RunTiming>,
    pub next: usize,
    pub more: bool,
    pub busy: bool,
    pub model: String,
    pub updates: Vec<Update<E>>,
    pub approvals: Vec<Approval<A>>,
    #[serde(default)]
    pub user_inputs: Vec<user_input::Record>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Response<E, A> {
    Usage(usage::Snapshot),
    Completions {
        input: String,
        entries: Vec<Completion>,
    },
    Hello {
        version: u32,
        workspace: String,
        capabilities: Capabilities,
        /// Build epoch in milliseconds; legacy services report zero.
        #[serde(default)]
        build_generation: u64,
    },
    Opened(SessionInfo),
    Profile(Box<SessionInfo>),
    Sessions(Vec<Session>),
    Page(Page<E, A>),
    Subagent(Page<E, A>),
    Questions(Questions),
    UserInputs(user_input::History),
    Report(Report),
    Plugins(plugins::Catalog),
    Resources(resources::Snapshot),
    Tasks(project::TaskSnapshot),
    Review(review::Snapshot),
    Subagents(subagents::Snapshot),
    InputQueue(input_queue::Snapshot),
    Files(files::Snapshot),
    File(String),
    /// Sent only in response to ReadFile with allow_missing set.
    FileMissing,
    InputQueued {
        message_id: i64,
    },
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
    EncodedFrame::new(value)?
        .write(writer)
        .await
}

/// A size-checked payload prepared before a caller enters a transport transaction.
pub struct EncodedFrame(Vec<u8>);

impl EncodedFrame {
    /// Serialize without I/O, rejecting invalid or oversized payloads before any bytes are sent.
    pub fn new<T: Serialize>(value: &T) -> io::Result<Self> {
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > MAX_FRAME_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "workspace frame exceeds limit",
            ));
        }
        Ok(Self(bytes))
    }

    /// Write and flush one complete frame; cancellation may leave the transport partially written.
    pub async fn write(&self, writer: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        writer
            .write_u32(self.0.len() as u32)
            .await?;
        writer.write_all(&self.0).await?;
        writer.flush().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_workspace_guards_are_optional_for_legacy_clients() {
        for legacy in [
            serde_json::json!({
                "method": "resources",
                "session": "session",
                "action": "Skills"
            }),
            serde_json::json!({
                "method": "plugins",
                "session": "session",
                "action": {
                    "action": "list"
                }
            }),
            serde_json::json!({
                "method": "tasks",
                "session": "session",
                "action": {
                    "action": "list",
                    "include_completed": true
                }
            }),
        ] {
            let request: Request = serde_json::from_value(legacy.clone()).unwrap();
            assert!(match &request {
                Request::Resources {
                    expected_workspace, ..
                }
                | Request::Plugins {
                    expected_workspace, ..
                }
                | Request::Tasks {
                    expected_workspace, ..
                } => expected_workspace.is_none(),
                _ => unreachable!(),
            });
            assert_eq!(serde_json::to_value(request).unwrap(), legacy);

            let mut guarded = legacy;
            guarded["expected_workspace"] = "/project".into();
            let request: Request = serde_json::from_value(guarded.clone()).unwrap();
            assert!(match &request {
                Request::Resources {
                    expected_workspace, ..
                }
                | Request::Plugins {
                    expected_workspace, ..
                }
                | Request::Tasks {
                    expected_workspace, ..
                } => {
                    expected_workspace.as_deref() == Some("/project")
                }
                _ => unreachable!(),
            });
            assert_eq!(serde_json::to_value(request).unwrap(), guarded);
        }
    }

    #[test]
    fn legacy_hello_has_unknown_build_generation() {
        let hello = serde_json::json!({
            "type": "hello",
            "value": {
                "version": VERSION,
                "workspace": "/project",
                "capabilities": {
                    "system": "Linux",
                    "architecture": "x86_64",
                    "shell": "sh",
                    "native_processes": true,
                    "files": true,
                    "features": []
                }
            }
        });
        let hello: Response<(), ()> = serde_json::from_value(hello).unwrap();
        assert!(matches!(
            hello,
            Response::Hello {
                build_generation: 0,
                ..
            }
        ));
    }

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
        assert!(matches!(
            read::<Request>(&mut reader)
                .await
                .unwrap(),
            Request::Open {
                resume: Some(id), ..
            } if id == "中文\n会话"
        ));
        assert!(matches!(
            read::<Request>(&mut reader)
                .await
                .unwrap(),
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
    pub features: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileUpload {
    pub name: String,
    pub data: String,
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
    /// Git operations.
    Git,
    /// write_file and edit_file.
    Write,
    /// request_capability for paths outside the workspace.
    Capability,
    /// run_command, read_command_output, verification_status.
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

#[cfg(test)]
mod mode_tests {
    use super::Mode;
    #[test]
    fn parses_supported_permission_modes() {
        assert_eq!(Mode::default(), Mode::Auto);
        for (text, mode) in [
            ("ask", Mode::Ask),
            ("auto", Mode::Auto),
            ("yolo", Mode::Yolo),
            ("YOLO", Mode::Yolo),
        ] {
            assert_eq!(Mode::parse(text), Some(mode));
        }
        assert_eq!(Mode::parse("unknown"), None);
    }
}

#[cfg(test)]
mod file_request_compatibility_tests {
    use super::*;

    #[test]
    fn legacy_file_read_does_not_opt_into_new_responses() {
        let legacy =
            serde_json::json!({"method": "read_file", "session": "fixture", "path": "notes.txt"});
        assert!(matches!(
            serde_json::from_value::<Request>(legacy).unwrap(),
            Request::ReadFile {
                allow_missing: false,
                ..
            }
        ));
        let request = Request::ReadFile {
            session: "fixture".into(),
            path: "notes.txt".into(),
            allow_missing: true,
        };
        assert_eq!(
            serde_json::to_value(request).unwrap()["allow_missing"],
            true
        );
        let response: Response<(), ()> = Response::FileMissing;
        assert!(matches!(
            serde_json::from_value::<Response<(), ()>>(serde_json::to_value(response).unwrap())
                .unwrap(),
            Response::FileMissing
        ));
    }
}
