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
