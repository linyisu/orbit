use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Identifies the environment where a capability or execution takes place.
/// Local is the host environment; Remote represents a named remote host (e.g. via SSH or RPC).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentId {
    Local,
    Remote(String),
}

impl EnvironmentId {
    pub fn local() -> Self {
        Self::Local
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local)
    }
}

impl std::fmt::Display for EnvironmentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Remote(id) => write!(f, "remote:{id}"),
        }
    }
}

/// Filesystem access level requested by or granted to an agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemAccess {
    Read,
    #[serde(rename = "read_write", alias = "write")]
    ReadWrite,
}

impl FilesystemAccess {
    pub fn allows_read(&self) -> bool {
        true
    }

    pub fn allows_write(&self) -> bool {
        matches!(self, Self::ReadWrite)
    }

    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::Read)
    }

    pub fn is_read_write(&self) -> bool {
        matches!(self, Self::ReadWrite)
    }

    pub fn description(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ReadWrite => "read and write",
        }
    }
}

/// A concrete filesystem capability request or descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FilesystemCapability {
    pub environment: EnvironmentId,
    pub path: PathBuf,
    pub access: FilesystemAccess,
}

impl FilesystemCapability {
    pub fn new(
        environment: EnvironmentId,
        path: impl Into<PathBuf>,
        access: FilesystemAccess,
    ) -> Self {
        Self {
            environment,
            path: path.into(),
            access,
        }
    }

    pub fn local_read(path: impl Into<PathBuf>) -> Self {
        Self::new(EnvironmentId::local(), path, FilesystemAccess::Read)
    }

    pub fn local_read_write(path: impl Into<PathBuf>) -> Self {
        Self::new(EnvironmentId::local(), path, FilesystemAccess::ReadWrite)
    }

    pub fn local_write(path: impl Into<PathBuf>) -> Self {
        Self::local_read_write(path)
    }
}

/// Network access policy for sandboxed execution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkPolicy {
    Deny,
    #[default]
    Allow,
}

impl NetworkPolicy {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_id_formatting_and_checks() {
        let local = EnvironmentId::local();

        assert!(local.is_local());
        assert_eq!(local.to_string(), "local");

        let remote = EnvironmentId::Remote("gpu-server".into());

        assert!(!remote.is_local());
        assert_eq!(remote.to_string(), "remote:gpu-server");
    }

    #[test]
    fn capability_construction() {
        let cap = FilesystemCapability::local_read("/data/models");

        assert_eq!(cap.environment, EnvironmentId::Local);
        assert_eq!(cap.path, PathBuf::from("/data/models"));
        assert_eq!(cap.access, FilesystemAccess::Read);
        assert!(cap.access.allows_read());
        assert!(!cap.access.allows_write());
        assert!(cap.access.is_read_only());
        assert!(!cap.access.is_read_write());
        assert_eq!(cap.access.description(), "read");

        let cap_rw = FilesystemCapability::local_read_write("/data/output");

        assert_eq!(cap_rw.access, FilesystemAccess::ReadWrite);
        assert!(cap_rw.access.allows_read());
        assert!(cap_rw.access.allows_write());
        assert!(!cap_rw.access.is_read_only());
        assert!(cap_rw.access.is_read_write());
        assert_eq!(cap_rw.access.description(), "read and write");

        let cap_write = FilesystemCapability::local_write("/data/output");

        assert_eq!(cap_write.access, FilesystemAccess::ReadWrite);

        let parsed_rw: FilesystemAccess = serde_json::from_str("\"read_write\"").unwrap();

        assert_eq!(parsed_rw, FilesystemAccess::ReadWrite);

        let parsed_alias: FilesystemAccess = serde_json::from_str("\"write\"").unwrap();

        assert_eq!(parsed_alias, FilesystemAccess::ReadWrite);

        let parsed_r: FilesystemAccess = serde_json::from_str("\"read\"").unwrap();

        assert_eq!(parsed_r, FilesystemAccess::Read);
    }
}
