pub mod discovery;
pub mod policy;
pub mod process;

pub use discovery::{GitRepoKind, GitRepositoryInfo};
pub use policy::{GitCommandAnalysis, GitOperationKind};
