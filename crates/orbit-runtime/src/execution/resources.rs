use serde::{Deserialize, Serialize};

/// Number of bytes in one mebibyte.
pub const MIB: usize = 1_024 * 1_024;
/// Sentinel disabling an optional OS resource limit.
pub const UNLIMITED: u64 = 0;
/// Default address-space budget; unlimited unless the host configures it.
pub const DEFAULT_MAX_MEMORY_BYTES: u64 = UNLIMITED;
/// Default output-file size budget; unlimited unless the host configures it.
pub const DEFAULT_MAX_FILE_SIZE_BYTES: u64 = UNLIMITED;
/// Default per-process file-descriptor ceiling on platforms with rlimits.
#[cfg(unix)]
pub const DEFAULT_MAX_OPEN_FILES: u64 = 1_024;
/// Windows has no equivalent per-process file-descriptor ceiling.
#[cfg(windows)]
pub const DEFAULT_MAX_OPEN_FILES: u64 = UNLIMITED;

/// OS process limits. Zero disables the corresponding limit.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ProcessLimits {
    pub max_memory_bytes: u64,
    pub max_file_size_bytes: u64,
    pub max_open_files: u64,
}

impl ProcessLimits {
    /// Reject limits that the native host cannot enforce before starting a process.
    pub fn validate_native(&self) -> std::io::Result<()> {
        self.validate()
            .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidInput, message))?;
        #[cfg(windows)]
        for (name, value) in [
            ("max_memory_bytes", self.max_memory_bytes),
            ("max_file_size_bytes", self.max_file_size_bytes),
            ("max_open_files", self.max_open_files),
        ] {
            if value != UNLIMITED && value != u64::MAX {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    format!("Windows cannot enforce the configured POSIX limit {name}"),
                ));
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_memory_bytes != UNLIMITED
            && self.max_memory_bytes != u64::MAX
            && self.max_memory_bytes < 16 * 1_024 * 1_024
        {
            return Err("process max_memory_bytes must be at least 16 MiB or 0 (unlimited)");
        }

        if self.max_file_size_bytes != UNLIMITED
            && self.max_file_size_bytes != u64::MAX
            && self.max_file_size_bytes < 1_024
        {
            return Err("process max_file_size_bytes must be at least 1 KiB or 0 (unlimited)");
        }

        if self.max_open_files != UNLIMITED
            && self.max_open_files != u64::MAX
            && self.max_open_files < 16
        {
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
            max_open_files: DEFAULT_MAX_OPEN_FILES,
        }
    }
}

/// Optional overrides of the default process limits.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ResourceLimits {
    pub max_memory_bytes: Option<u64>,
    pub max_file_size_bytes: Option<u64>,
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
            max_memory_bytes: self
                .max_memory_bytes
                .unwrap_or(DEFAULT_MAX_MEMORY_BYTES),
            max_file_size_bytes: self
                .max_file_size_bytes
                .unwrap_or(DEFAULT_MAX_FILE_SIZE_BYTES),
            max_open_files: self
                .max_open_files
                .unwrap_or(DEFAULT_MAX_OPEN_FILES),
        }
    }
}

impl From<ProcessLimits> for ResourceLimits {
    fn from(limits: ProcessLimits) -> Self {
        Self {
            max_memory_bytes: Some(limits.max_memory_bytes),
            max_file_size_bytes: Some(limits.max_file_size_bytes),
            max_open_files: Some(limits.max_open_files),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_default_limits_are_supported() {
        ProcessLimits::default()
            .validate_native()
            .unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn native_windows_rejects_each_unsupported_finite_limit() {
        for limits in [
            ProcessLimits {
                max_memory_bytes: 16 * MIB as u64,
                ..Default::default()
            },
            ProcessLimits {
                max_file_size_bytes: 1_024,
                ..Default::default()
            },
            ProcessLimits {
                max_open_files: 16,
                ..Default::default()
            },
        ] {
            assert_eq!(
                limits
                    .validate_native()
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::Unsupported
            );
        }
        ProcessLimits {
            max_memory_bytes: u64::MAX,
            max_file_size_bytes: u64::MAX,
            max_open_files: u64::MAX,
        }
        .validate_native()
        .unwrap();
    }

    #[test]
    fn process_limits_validation() {
        let valid = ProcessLimits::default();
        assert!(valid.validate().is_ok());

        let mut invalid = valid.clone();
        // Less than 16 MiB
        invalid.max_memory_bytes = 1_024;
        assert!(invalid.validate().is_err());
    }

    #[test]
    fn resource_limits_conversion() {
        let unconstrained = ResourceLimits::unconstrained();
        assert_eq!(unconstrained.max_memory_bytes, None);
        assert_eq!(unconstrained.max_file_size_bytes, None);

        let process_limits = unconstrained.to_process_limits();
        assert_eq!(process_limits.max_memory_bytes, UNLIMITED);
        assert_eq!(process_limits.max_file_size_bytes, UNLIMITED);

        let custom = ResourceLimits::default()
            .with_memory_limit(1_024 * 1_024 * 1_024)
            .with_file_size_limit(100 * 1_024 * 1_024);

        let converted = custom.to_process_limits();
        assert_eq!(converted.max_memory_bytes, 1_024 * 1_024 * 1_024);
        assert_eq!(converted.max_file_size_bytes, 100 * 1_024 * 1_024);
    }

    #[test]
    fn resource_limits_preserves_unlimited_round_trip() {
        let original = ProcessLimits {
            max_memory_bytes: 0,
            max_file_size_bytes: 0,
            max_open_files: 0,
        };

        let resource_limits = ResourceLimits::from(original.clone());
        let round_trip = resource_limits.to_process_limits();
        assert_eq!(original, round_trip);
    }
}
