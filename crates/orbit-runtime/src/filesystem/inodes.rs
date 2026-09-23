use std::{
    collections::{HashSet, VecDeque},
    path::Path,
};

use std::os::unix::fs::MetadataExt;

use crate::error::{RuntimeError, failed};

/// Collect descendants of a protected directory. Callers only use this for
/// directories whose inode identities are needed; oversized scans fail closed.
pub(crate) fn collect_dir_inodes(
    dir: &Path,
    protected_inodes: &mut HashSet<(u64, u64)>,
) -> Result<(), RuntimeError> {
    let mut queue = VecDeque::from([(dir.to_path_buf(), 0usize)]);
    let mut visited = 0usize;
    const MAX_PROTECTED_DIR_ENTRIES: usize = 20_000;
    const MAX_PROTECTED_DIR_DEPTH: usize = 32;

    while let Some((curr, depth)) = queue.pop_front() {
        let entries = match std::fs::read_dir(&curr) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(failed(format!(
                    "failed to read protected directory '{}': {e}",
                    curr.display()
                )));
            }
        };
        for entry in entries {
            let entry = entry.map_err(|e| {
                failed(format!(
                    "failed to read entry in protected directory '{}': {e}",
                    curr.display()
                ))
            })?;
            visited += 1;
            if visited > MAX_PROTECTED_DIR_ENTRIES {
                return Err(failed(format!(
                    "protected directory '{}' exceeds maximum scan limit of {MAX_PROTECTED_DIR_ENTRIES} entries",
                    dir.display()
                )));
            }
            let meta = entry.path().symlink_metadata().map_err(|e| {
                failed(format!(
                    "failed to inspect protected entry '{}': {e}",
                    entry.path().display()
                ))
            })?;
            protected_inodes.insert((meta.dev(), meta.ino()));
            if meta.is_dir() && !meta.file_type().is_symlink() {
                if depth >= MAX_PROTECTED_DIR_DEPTH {
                    return Err(failed(format!(
                        "protected directory '{}' depth exceeds maximum limit of {MAX_PROTECTED_DIR_DEPTH} at '{}'",
                        dir.display(),
                        entry.path().display()
                    )));
                }
                queue.push_back((entry.path(), depth + 1));
            }
        }
    }
    Ok(())
}

/// Scan opaque roots only when a multiply-linked inode could point into one.
/// A root on another device cannot share a hardlink with the candidate.
pub(crate) fn collect_opaque_inodes_for_dev(
    device: u64,
    roots: &[std::path::PathBuf],
    protected_inodes: &mut HashSet<(u64, u64)>,
    scanned: &mut HashSet<std::path::PathBuf>,
) -> Result<(), RuntimeError> {
    for root in roots {
        let metadata = match std::fs::symlink_metadata(root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(failed(error)),
        };
        if metadata.dev() == device && metadata.is_dir() && scanned.insert(root.clone()) {
            collect_dir_inodes(root, protected_inodes)?;
        }
    }
    Ok(())
}

/// File tools reject inode aliases of reserved workspace names as well as
/// literal paths. The command sandbox performs this same check while building
/// its mount masks; ordinary single-link reads need no extra traversal.
pub(crate) fn is_reserved_workspace_inode(
    root: &Path,
    candidate: (u64, u64),
) -> Result<bool, RuntimeError> {
    let mut queue = VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut visited = 0usize;
    const MAX_SCAN_ENTRIES: usize = 100_000;
    const MAX_SCAN_DEPTH: usize = 32;

    while let Some((directory, depth)) = queue.pop_front() {
        let entries = std::fs::read_dir(&directory).map_err(failed)?;
        for entry in entries {
            let entry = entry.map_err(failed)?;
            visited += 1;
            if visited > MAX_SCAN_ENTRIES {
                return Err(failed("reserved path scan exceeded 100000 entries"));
            }
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).map_err(failed)?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let reserved = name == ".git"
                || name == ".env"
                || name.starts_with(".env.")
                || name.starts_with(".orbit-write-");
            if reserved {
                if (meta.dev(), meta.ino()) == candidate {
                    return Ok(true);
                }
                if meta.is_dir() {
                    let mut descendants = HashSet::new();
                    collect_dir_inodes(&path, &mut descendants)?;
                    if descendants.contains(&candidate) {
                        return Ok(true);
                    }
                }
                continue;
            }
            if meta.is_dir() && !meta.file_type().is_symlink() {
                if depth >= MAX_SCAN_DEPTH {
                    return Err(failed("reserved path scan exceeded 32 directory levels"));
                }
                queue.push_back((path, depth + 1));
            }
        }
    }
    Ok(false)
}
