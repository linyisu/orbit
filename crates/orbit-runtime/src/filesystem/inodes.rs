use std::os::unix::fs::MetadataExt;
use std::{
    collections::{HashSet, VecDeque},
    path::Path,
};

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
                    "protected directory '{}' exceeds maximum scan limit of \
                        {MAX_PROTECTED_DIR_ENTRIES} entries",
                    dir.display()
                )));
            }

            let meta = entry
                .path()
                .symlink_metadata()
                .map_err(|e| {
                    failed(format!(
                        "failed to inspect protected entry '{}': {e}",
                        entry.path().display()
                    ))
                })?;

            protected_inodes.insert((meta.dev(), meta.ino()));

            if meta.is_dir() && !meta.file_type().is_symlink() {
                if depth >= MAX_PROTECTED_DIR_DEPTH {
                    return Err(failed(format!(
                        "protected directory '{}' depth exceeds maximum limit of \
                            {MAX_PROTECTED_DIR_DEPTH} at '{}'",
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

/// File tools reject inode aliases of reserved workspace names as well as
/// literal paths; ordinary single-link reads need no extra traversal.
pub(crate) fn is_reserved_workspace_inode_with_templates(
    root: &Path,
    candidate: (u64, u64),
    link_count: u64,
    read: &[std::path::PathBuf],
    write: &[std::path::PathBuf],
) -> Result<bool, RuntimeError> {
    type Aliases =
        std::collections::HashMap<(std::path::PathBuf, (u64, u64)), Vec<std::path::PathBuf>>;
    static COMPLETE_ALIASES: std::sync::OnceLock<std::sync::Mutex<Aliases>> =
        std::sync::OnceLock::new();
    let cache = COMPLETE_ALIASES.get_or_init(Default::default);
    let key = (root.to_path_buf(), candidate);
    let known = cache
        .lock()
        .map_err(failed)?
        .get(&key)
        .cloned();
    if let Some(aliases) = known
        && aliases.len() as u64 == link_count
        && aliases.iter().all(|path| {
            if !std::fs::symlink_metadata(path).is_ok_and(|meta| {
                meta.is_file()
                    && (meta.dev(), meta.ino()) == candidate
                    && meta.nlink() == link_count
            }) {
                return false;
            }

            for ancestor in path
                .ancestors()
                .take_while(|path| *path != root)
            {
                if ancestor != path
                    && !std::fs::symlink_metadata(ancestor)
                        .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink())
                {
                    return false;
                }

                if ancestor
                    .file_name()
                    .is_some_and(|name| {
                        super::path_security::is_protected_workspace_name(&name.to_string_lossy())
                    })
                    && !read
                        .iter()
                        .any(|allowed| ancestor.starts_with(allowed))
                    && !write
                        .iter()
                        .any(|allowed| ancestor.starts_with(allowed))
                {
                    return false;
                }
            }

            true
        })
    {
        return Ok(false);
    }
    // Cache only a complete set of hardlinks, never an incomplete scan of the tree.
    let mut aliases = Vec::new();
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
            let reserved = super::path_security::is_protected_workspace_name(&name)
                && !read.contains(&path)
                && !write.contains(&path);

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

            if meta.is_file() && (meta.dev(), meta.ino()) == candidate {
                aliases.push(path.clone());
                if aliases.len() as u64 == link_count {
                    let mut cache = cache.lock().map_err(failed)?;
                    if cache.len() >= 128 {
                        cache.clear();
                    }
                    cache.insert(key, aliases);
                    return Ok(false);
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_alias_cache_rechecks_links_and_ancestors_before_allowing_access() {
        for change in ["rename-file", "rename-directory", "new-link"] {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path();
            let source = root.join("orbit.rs");
            std::fs::write(&source, "Orbit").unwrap();
            std::fs::create_dir(root.join("normal")).unwrap();
            std::fs::hard_link(&source, root.join("normal/alias.rs")).unwrap();
            let check = || {
                let meta = std::fs::metadata(&source).unwrap();
                is_reserved_workspace_inode_with_templates(
                    root,
                    (meta.dev(), meta.ino()),
                    meta.nlink(),
                    &[],
                    &[],
                )
                .unwrap()
            };

            assert!(!check());
            assert!(!check());
            match change {
                "rename-file" => {
                    std::fs::rename(root.join("normal/alias.rs"), root.join(".env")).unwrap()
                }
                "rename-directory" => {
                    std::fs::rename(root.join("normal"), root.join(".git")).unwrap()
                }
                _ => std::fs::hard_link(&source, root.join(".env")).unwrap(),
            }

            assert!(check(), "{change} must invalidate the safe alias set");
        }
    }
}
