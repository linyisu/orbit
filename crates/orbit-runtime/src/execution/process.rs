pub use super::output::{
    MAX_OUTPUT, MAX_RETAINED_OUTPUT_PER_RUN, MAX_RETAINED_OUTPUT_PER_STREAM, OutputLog,
};
pub use super::resources::{
    DEFAULT_MAX_FILE_SIZE_BYTES, DEFAULT_MAX_MEMORY_BYTES, DEFAULT_MAX_OPEN_FILES, MIB,
    ProcessLimits,
};
use crate::verification::VerificationEvidence;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::ProcessGroup;
#[cfg(windows)]
pub use windows::ProcessGroup;

#[cfg(test)]
mod tests;

/// Find a system program in trusted locations without consulting inherited PATH.
#[cfg(unix)]
pub fn system_executable(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    ["/usr/bin", "/bin"]
        .into_iter()
        .map(|directory| Path::new(directory).join(name))
        .filter_map(|path| path.canonicalize().ok())
        .find(|path| {
            (path.starts_with("/usr/bin") || path.starts_with("/bin"))
                && path.metadata().is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o022 == 0
                })
        })
}

/// Find a program in the Windows system directory reported by the operating system.
#[cfg(windows)]
pub fn system_executable(name: &str) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    if Path::new(name).components().count() != 1 || name.contains(['/', '\\', ':']) {
        return None;
    }
    let mut buffer = vec![0u16; 32_768];
    // SAFETY: the writable buffer contains the supplied number of UTF-16 elements.
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if length == 0 || length as usize >= buffer.len() {
        return None;
    }
    let directory = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length as usize]));
    let path = directory.join(name);
    path.is_file().then_some(path)
}

#[derive(Debug, Serialize)]
pub struct CommandResult {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub core_dumped: bool,
    pub elapsed_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
    pub output_truncated: bool,
    pub stdout_total_bytes: u64,
    pub stderr_total_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_id: Option<String>,
    pub retained_output_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_output_error: Option<String>,
    pub sandbox: &'static str,
    pub workspace_files: usize,
    pub skipped_entries: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_error: Option<String>,
    #[serde(skip)]
    pub verification: Option<VerificationEvidence>,
}
