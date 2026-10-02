pub mod discovery;
pub mod mount;
pub mod policy;

pub use discovery::{GitRepoKind, GitRepositoryInfo};
pub use mount::GitMountPlan;
pub use policy::{GitCommandAnalysis, GitOperationKind};
