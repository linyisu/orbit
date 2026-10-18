use super::resources::MIB;
use crate::{
    error::{RuntimeError, failed},
    filesystem::{Workspace, WorkspaceError},
    verification::{FileFingerprint, WorkspaceFingerprint},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkspaceScanLimits {
    #[serde(default = "default_max_scan_mib")]
    pub max_scan_mib: usize,
    #[serde(default = "default_max_scan_entries")]
    pub max_scan_entries: usize,
}

fn default_max_scan_mib() -> usize {
    512
}

fn default_max_scan_entries() -> usize {
    10_000
}

impl Default for WorkspaceScanLimits {
    fn default() -> Self {
        Self {
            max_scan_mib: default_max_scan_mib(),
            max_scan_entries: default_max_scan_entries(),
        }
    }
}

impl WorkspaceScanLimits {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=1_024).contains(&self.max_scan_mib)
            || !(1..=100_000).contains(&self.max_scan_entries)
        {
            return Err("max_scan_mib must be 1..1024 and max_scan_entries 1..100000");
        }

        Ok(())
    }
}

struct WorkspaceScan {
    pub fingerprint: WorkspaceFingerprint,
    pub limits: WorkspaceScanLimits,
    pub files: usize,
    pub skipped: usize,
    pub bytes: usize,
    pub visited: usize,
}

impl WorkspaceScan {
    pub fn scan(
        workspace: Workspace,
        cancellation: CancellationToken,
    ) -> Result<Self, RuntimeError> {
        Self::scan_configured(workspace, cancellation, WorkspaceScanLimits::default())
    }

    pub fn scan_configured(
        workspace: Workspace,
        cancellation: CancellationToken,
        limits: WorkspaceScanLimits,
    ) -> Result<Self, RuntimeError> {
        limits.validate().map_err(failed)?;
        let mut scan = Self {
            fingerprint: WorkspaceFingerprint::default(),
            limits,
            files: 0,
            skipped: 0,
            bytes: 0,
            visited: 0,
        };

        scan.scan_directory(&workspace, Path::new(""), &cancellation, 0)?;
        scan.fingerprint.directories.sort();
        scan.fingerprint.skipped_entries = scan.skipped;
        scan.fingerprint.digest = content_hash(
            &serde_json::to_vec(&(
                &scan.fingerprint.files,
                &scan.fingerprint.directories,
                scan.fingerprint.skipped_entries,
            ))
            .map_err(failed)?,
        );

        Ok(scan)
    }

    fn scan_directory(
        &mut self,
        workspace: &Workspace,
        relative: &Path,
        token: &CancellationToken,
        depth: usize,
    ) -> Result<(), RuntimeError> {
        if token.is_cancelled() {
            return Err(RuntimeError::Cancelled);
        }
        if depth > 64 {
            return Err(failed("workspace scan exceeds 64 directory levels"));
        }

        for name in workspace
            .directory_entries(relative)
            .map_err(failed)?
        {
            self.visited += 1;

            if self.visited > self.limits.max_scan_entries {
                return Err(failed(format!(
                    "workspace scan exceeds {} entries",
                    self.limits.max_scan_entries
                )));
            }

            if token.is_cancelled() {
                return Err(RuntimeError::Cancelled);
            }
            if ["target", "node_modules", ".venv", "__pycache__"]
                .iter()
                .any(|excluded| name == *excluded)
            {
                self.skipped += 1;
                continue;
            }

            let path = relative.join(name);

            match workspace.open_read(&path) {
                Ok(mut file) => {
                    use std::os::unix::fs::PermissionsExt;

                    let executable = file
                        .metadata()
                        .map_err(failed)?
                        .permissions()
                        .mode()
                        & 0o111
                        != 0;
                    let available = (self.limits.max_scan_mib * MIB).saturating_sub(self.bytes);
                    if file.metadata().map_err(failed)?.len() > available as u64 {
                        return Err(failed(format!(
                            "workspace scan exceeds {} MiB",
                            self.limits.max_scan_mib
                        )));
                    }
                    let mut reader = file.by_ref().take(available as u64 + 1);
                    let mut digest = Sha256::new();
                    let mut buffer = vec![0; 64 * 1_024];
                    let mut length = 0;
                    loop {
                        if token.is_cancelled() {
                            return Err(RuntimeError::Cancelled);
                        }
                        let count = reader
                            .read(&mut buffer)
                            .map_err(failed)?;
                        if count == 0 {
                            break;
                        }
                        length += count;
                        if length > available {
                            return Err(failed(format!(
                                "workspace scan exceeds {} MiB",
                                self.limits.max_scan_mib
                            )));
                        }
                        digest.update(&buffer[..count]);
                    }
                    self.bytes += length;
                    self.fingerprint.files.insert(
                        path.to_str()
                            .ok_or_else(|| failed("workspace scan paths must be valid UTF-8"))?
                            .to_owned(),
                        FileFingerprint {
                            sha256: digest
                                .finalize()
                                .iter()
                                .map(|byte| format!("{byte:02x}"))
                                .collect(),
                            executable,
                        },
                    );

                    self.files += 1;
                }

                Err(WorkspaceError::NotFile) => {
                    if workspace
                        .directory_entries(&path)
                        .is_ok()
                    {
                        self.fingerprint.directories.push(
                            path.to_str()
                                .ok_or_else(|| failed("workspace scan paths must be valid UTF-8"))?
                                .to_owned(),
                        );

                        self.scan_directory(workspace, &path, token, depth + 1)?;
                    } else {
                        self.skipped += 1;
                    }
                }

                Err(WorkspaceError::Io(_)) => self.skipped += 1,
                Err(error) => return Err(failed(error)),
            }
        }

        Ok(())
    }
}

pub fn content_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub async fn fingerprint(
    workspace: Workspace,
    cancellation: CancellationToken,
) -> Result<WorkspaceFingerprint, RuntimeError> {
    tokio::task::spawn_blocking(move || {
        WorkspaceScan::scan(workspace, cancellation).map(|s| s.fingerprint)
    })
    .await
    .map_err(failed)?
}

pub async fn fingerprint_configured(
    workspace: Workspace,
    cancellation: CancellationToken,
    limits: WorkspaceScanLimits,
) -> Result<WorkspaceFingerprint, RuntimeError> {
    tokio::task::spawn_blocking(move || {
        WorkspaceScan::scan_configured(workspace, cancellation, limits).map(|s| s.fingerprint)
    })
    .await
    .map_err(failed)?
}

pub async fn scan_workspace_summary(
    workspace: Workspace,
    cancellation: CancellationToken,
    limits: WorkspaceScanLimits,
) -> Result<(WorkspaceFingerprint, usize, usize), RuntimeError> {
    tokio::task::spawn_blocking(move || {
        WorkspaceScan::scan_configured(workspace, cancellation, limits)
            .map(|s| (s.fingerprint, s.files, s.skipped))
    })
    .await
    .map_err(failed)?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    #[tokio::test]
    async fn default_scan_accepts_large_assets_and_hashes_their_tail_without_buffering_them() {
        let directory = tempfile::tempdir().unwrap();
        let mut asset = std::fs::File::create(directory.path().join("generated.bin")).unwrap();
        asset.set_len(65 * MIB as u64).unwrap();
        let workspace = Workspace::new(directory.path()).unwrap();
        let before = fingerprint(workspace.clone(), CancellationToken::new())
            .await
            .unwrap();
        asset.seek(SeekFrom::End(-1)).unwrap();
        asset.write_all(&[1]).unwrap();
        let after = fingerprint(workspace.clone(), CancellationToken::new())
            .await
            .unwrap();
        assert_ne!(before.files["generated.bin"], after.files["generated.bin"]);
        let small = WorkspaceScanLimits {
            max_scan_mib: 1,
            ..Default::default()
        };
        assert!(
            fingerprint_configured(workspace.clone(), CancellationToken::new(), small)
                .await
                .is_err()
        );
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(matches!(
            fingerprint(workspace, cancelled).await,
            Err(RuntimeError::Cancelled)
        ));
    }
}
