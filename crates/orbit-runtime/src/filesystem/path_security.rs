use std::{
    io,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};

/// Requested spelling paired with its normalized canonical path.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResolvedPath {
    requested: PathBuf,
    canonical: PathBuf,
}

impl ResolvedPath {
    /// Resolve existing links; append missing components to the nearest canonical ancestor.
    pub fn resolve(requested: impl AsRef<Path>, base: Option<&Path>) -> io::Result<Self> {
        let requested_ref = requested.as_ref();
        #[cfg(windows)]
        validate_windows_path(requested_ref)?;

        let absolute_requested = if requested_ref.is_absolute() {
            normalize_lexical(requested_ref)
        } else {
            let base_dir = match base {
                Some(b) => b.to_path_buf(),
                None => std::env::current_dir()?,
            };

            normalize_lexical(&base_dir.join(requested_ref))
        };

        let canonical = resolve_canonical_best_effort(&absolute_requested)?;

        Ok(Self {
            requested: requested_ref.to_path_buf(),
            canonical,
        })
    }

    /// The caller must supply an already canonical path.
    pub fn from_canonical(path: impl Into<PathBuf>) -> Self {
        let canonical = path.into();
        Self {
            requested: canonical.clone(),
            canonical,
        }
    }

    pub fn requested(&self) -> &Path {
        &self.requested
    }

    pub fn canonical(&self) -> &Path {
        &self.canonical
    }

    pub fn is_subpath_of(&self, parent: &ResolvedPath) -> bool {
        self.canonical
            .starts_with(&parent.canonical)
    }

    pub fn is_subpath_of_canonical(&self, parent_canonical: &Path) -> bool {
        self.canonical
            .starts_with(parent_canonical)
    }
}

/// Reject Win32 aliases whose filesystem meaning differs from a normal path component.
#[cfg(windows)]
pub fn validate_windows_path(path: &Path) -> io::Result<()> {
    use std::path::Prefix;
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "ambiguous Windows filesystem path",
        )
    };
    if path.has_root() && !path.is_absolute() {
        return Err(invalid());
    }
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                if !path.is_absolute()
                    || !matches!(
                        prefix.kind(),
                        Prefix::Disk(_)
                            | Prefix::VerbatimDisk(_)
                            | Prefix::UNC(_, _)
                            | Prefix::VerbatimUNC(_, _)
                    )
                {
                    return Err(invalid());
                }
            }
            Component::Normal(name) => {
                let name = name.to_string_lossy();
                if name.ends_with(['.', ' '])
                    || name
                        .chars()
                        .any(|c| c < ' ' || matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*'))
                {
                    return Err(invalid());
                }
                let stem = name
                    .split('.')
                    .next()
                    .unwrap_or_default()
                    .trim_end()
                    .to_ascii_uppercase();
                if matches!(
                    stem.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
                ) || ["COM", "LPT"].iter().any(|prefix| {
                    stem.strip_prefix(prefix)
                        .is_some_and(|suffix| {
                            matches!(
                                suffix,
                                "1" | "2"
                                    | "3"
                                    | "4"
                                    | "5"
                                    | "6"
                                    | "7"
                                    | "8"
                                    | "9"
                                    | "¹"
                                    | "²"
                                    | "³"
                            )
                        })
                }) {
                    return Err(invalid());
                }
            }
            _ => (),
        }
    }
    Ok(())
}

/// Canonicalize the existing prefix and append the missing suffix.
fn resolve_canonical_best_effort(path: &Path) -> io::Result<PathBuf> {
    if path.exists() {
        return std::fs::canonicalize(path);
    }

    let mut existing_ancestor = path.to_path_buf();
    let mut trailing_components = Vec::new();

    while !existing_ancestor.exists() {
        if let Some(file_name) = existing_ancestor.file_name() {
            trailing_components.push(file_name.to_os_string());
        } else {
            break;
        }

        if !existing_ancestor.pop() {
            break;
        }
    }

    trailing_components.reverse();

    let mut canonical_base = if existing_ancestor.exists() {
        std::fs::canonicalize(&existing_ancestor)?
    } else {
        existing_ancestor
    };

    for component in trailing_components {
        canonical_base.push(component);
    }

    Ok(canonical_base)
}

/// Normalize `.` and `..` lexically, without accessing the filesystem.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut components = Vec::new();

    for component in path.components() {
        match component {
            Component::Prefix(prefix) => components.push(Component::Prefix(prefix)),
            Component::RootDir => components.push(Component::RootDir),
            Component::CurDir => {}
            Component::ParentDir => {
                if let Some(last) = components.last()
                    && matches!(last, Component::Normal(_))
                {
                    components.pop();
                    continue;
                }

                components.push(Component::ParentDir);
            }

            Component::Normal(name) => components.push(Component::Normal(name)),
        }
    }

    components.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_existing_file_and_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("test.txt");
        std::fs::write(&file_path, "content").unwrap();

        let resolved_file = ResolvedPath::resolve(&file_path, None).unwrap();

        assert_eq!(
            resolved_file.canonical(),
            std::fs::canonicalize(&file_path).unwrap()
        );
        assert_eq!(resolved_file.requested(), &file_path);

        let resolved_dir = ResolvedPath::resolve(tmp.path(), None).unwrap();

        assert!(resolved_file.is_subpath_of(&resolved_dir));
    }

    #[test]
    fn resolves_non_existent_future_child() {
        let tmp = tempfile::tempdir().unwrap();
        let future_path = tmp
            .path()
            .join("sub1")
            .join("sub2")
            .join("new_file.txt");

        let resolved = ResolvedPath::resolve(&future_path, None).unwrap();
        let canonical_tmp = std::fs::canonicalize(tmp.path()).unwrap();

        assert!(resolved.is_subpath_of_canonical(&canonical_tmp));
        assert_eq!(
            resolved.canonical(),
            canonical_tmp
                .join("sub1")
                .join("sub2")
                .join("new_file.txt")
        );
    }

    #[test]
    fn resolves_symlink_to_real_target() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real_target");
        let link_dir = tmp.path().join("symlink_dir");

        std::fs::create_dir(&real_dir).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real_dir, &link_dir).unwrap();

        let file_in_link = link_dir.join("data.txt");
        std::fs::write(&file_in_link, "data").unwrap();

        let resolved = ResolvedPath::resolve(&file_in_link, None).unwrap();
        let canonical_real = std::fs::canonicalize(&real_dir).unwrap();

        assert!(resolved.is_subpath_of_canonical(&canonical_real));
    }

    #[test]
    fn prevents_sibling_prefix_spoofing() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("models");
        let sibling = tmp.path().join("models_other");

        std::fs::create_dir(&parent).unwrap();
        std::fs::create_dir(&sibling).unwrap();

        let resolved_parent = ResolvedPath::resolve(&parent, None).unwrap();
        let resolved_sibling = ResolvedPath::resolve(&sibling, None).unwrap();

        assert!(!resolved_sibling.is_subpath_of(&resolved_parent));
    }

    #[test]
    fn normalizes_parent_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();

        let traversal = nested
            .join("..")
            .join("b")
            .join("file.txt");
        let resolved = ResolvedPath::resolve(&traversal, None).unwrap();

        let expected = std::fs::canonicalize(&nested)
            .unwrap()
            .join("file.txt");

        assert_eq!(resolved.canonical(), expected);
    }
}
