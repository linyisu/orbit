mod access;
pub mod path_security;
#[cfg(windows)]
pub mod private;
pub mod workspace;

pub use path_security::ResolvedPath;
pub use workspace::{Workspace, WorkspaceError};

#[cfg(test)]
pub(crate) fn private_test_directory() -> std::io::Result<tempfile::TempDir> {
    #[cfg(unix)]
    {
        tempfile::tempdir()
    }
    #[cfg(windows)]
    {
        private::temporary_directory(&std::env::temp_dir(), "orbit-test-")
    }
}
