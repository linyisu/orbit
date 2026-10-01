use serde::{Deserialize, Serialize};

pub const MIB: usize = 1024 * 1024;
pub const TMP_SIZE_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_MAX_MEMORY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const DEFAULT_MAX_FILE_SIZE_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_MAX_PROCESSES: u64 = 1024;
pub const DEFAULT_MAX_OPEN_FILES: u64 = 1024;

/// ProcessLimits provides strict resource limits enforced via OS prlimit/rlimit.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ProcessLimits {
    pub max_memory_bytes: u64,
    pub max_file_size_bytes: u64,
    pub max_processes: u64,
    pub max_open_files: u64,
}

impl ProcessLimits {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_memory_bytes < 16 * 1024 * 1024 {
            return Err("process max_memory_bytes must be at least 16 MiB");
        }

        if self.max_file_size_bytes < 1024 {
            return Err("process max_file_size_bytes must be at least 1 KiB");
        }

        if self.max_processes < 1 {
            return Err("process max_processes must be at least 1");
        }

        if self.max_open_files < 16 {
            return Err("process max_open_files must be at least 16");
        }

        Ok(())
    }
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
            max_file_size_bytes: DEFAULT_MAX_FILE_SIZE_BYTES,
            max_processes: DEFAULT_MAX_PROCESSES,
            max_open_files: DEFAULT_MAX_OPEN_FILES,
        }
    }
}

/// ResourceLimits provides compilation-friendly optional resource boundaries.
/// Unlike legacy ProcessLimits, unset limits impose no artificial hard ceiling on
/// builds or large binary artifacts.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ResourceLimits {
    pub max_memory_bytes: Option<u64>,
    pub max_file_size_bytes: Option<u64>,
    pub max_processes: Option<u64>,
    pub max_open_files: Option<u64>,
}

impl ResourceLimits {
    pub fn unconstrained() -> Self {
        Self::default()
    }

    pub fn with_memory_limit(mut self, bytes: u64) -> Self {
        self.max_memory_bytes = Some(bytes);
        self
    }

    pub fn with_file_size_limit(mut self, bytes: u64) -> Self {
        self.max_file_size_bytes = Some(bytes);
        self
    }

    pub fn to_process_limits(&self) -> ProcessLimits {
        ProcessLimits {
            max_memory_bytes: self.max_memory_bytes.unwrap_or(DEFAULT_MAX_MEMORY_BYTES),
            max_file_size_bytes: self
                .max_file_size_bytes
                .unwrap_or(DEFAULT_MAX_FILE_SIZE_BYTES),
            max_processes: self.max_processes.unwrap_or(DEFAULT_MAX_PROCESSES),
            max_open_files: self.max_open_files.unwrap_or(DEFAULT_MAX_OPEN_FILES),
        }
    }
}

impl From<ProcessLimits> for ResourceLimits {
    fn from(limits: ProcessLimits) -> Self {
        Self {
            max_memory_bytes: Some(limits.max_memory_bytes),
            max_file_size_bytes: Some(limits.max_file_size_bytes),
            max_processes: Some(limits.max_processes),
            max_open_files: Some(limits.max_open_files),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_limits_validation() {
        let valid = ProcessLimits::default();
        assert!(valid.validate().is_ok());

        let mut invalid = valid.clone();
        invalid.max_memory_bytes = 1024; // Less than 16 MiB
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn resource_limits_conversion() {
        let unconstrained = ResourceLimits::unconstrained();
        assert_eq!(unconstrained.max_memory_bytes, None);
        assert_eq!(unconstrained.max_file_size_bytes, None);

        let process_limits = unconstrained.to_process_limits();
        assert_eq!(process_limits.max_memory_bytes, DEFAULT_MAX_MEMORY_BYTES);
        assert_eq!(
            process_limits.max_file_size_bytes,
            DEFAULT_MAX_FILE_SIZE_BYTES
        );

        let custom = ResourceLimits::default()
            .with_memory_limit(1024 * 1024 * 1024)
            .with_file_size_limit(100 * 1024 * 1024);

        let converted = custom.to_process_limits();
        assert_eq!(converted.max_memory_bytes, 1024 * 1024 * 1024);
        assert_eq!(converted.max_file_size_bytes, 100 * 1024 * 1024);
    }
}
