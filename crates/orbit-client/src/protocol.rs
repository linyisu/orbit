pub use orbit_protocol::{
    Configuration, Mode, PAGE_BYTES, PAGE_EVENTS, PREAMBLE, Request, SessionInfo, VERSION, read,
    read_preamble, write,
};
pub type Update = orbit_protocol::Update<orbit_agent::events::AgentEvent>;
pub type Approval = orbit_protocol::Approval<orbit_tools::approval::ApprovalPreview>;
pub type Page =
    orbit_protocol::Page<orbit_agent::events::AgentEvent, orbit_tools::approval::ApprovalPreview>;
pub type Response = orbit_protocol::Response<
    orbit_agent::events::AgentEvent,
    orbit_tools::approval::ApprovalPreview,
>;

/// Event and capability data consumed by presentation code; execution stays in the service.
pub use orbit_agent::events;
pub use orbit_model::types as model;
pub mod calibration {
    pub use orbit_agent::calibration::TokenCalibration;
}
pub mod compaction {
    pub use orbit_agent::memory::{
        CompactionMode, CompactionRecord, CompactionStrategy, FileActivity, TaskSummary,
    };
}
pub mod approval {
    pub use orbit_tools::approval::{
        ApprovalPreview, CapabilityPreview, CommandPreview, CommandWorkspaceAccess, WritePreview,
    };
}
pub mod capability {
    pub use orbit_runtime::capability::{EnvironmentId, FilesystemAccess};
}
pub mod git {
    pub use orbit_runtime::git::{GitCommandAnalysis, GitOperationKind};
}

/// Decode the transport encoding of a workspace file without interpreting its contents.
pub fn file_bytes(encoded: impl AsRef<[u8]>) -> anyhow::Result<Vec<u8>> {
    use base64::Engine;
    Ok(base64::engine::general_purpose::STANDARD.decode(encoded)?)
}
