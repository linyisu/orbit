use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GitRepoKind {
    Standard,
    Worktree { main_repo_dir: PathBuf },
    Submodule { parent_repo_dir: PathBuf },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRepositoryInfo {
    pub worktree_root: PathBuf,
    /// Path to `.git` within the worktree (either a directory or file).
    pub dot_git: PathBuf,
    pub is_gitfile: bool,
    /// The resolved git metadata directory (containing index, HEAD, etc.).
    pub git_dir: PathBuf,
    /// The common git metadata directory (containing objects, refs, config, etc.).
    pub common_dir: PathBuf,
    pub kind: GitRepoKind,
}

impl GitRepositoryInfo {
    pub fn discover(workspace_root: &Path) -> io::Result<Option<Self>> {
        let dot_git = workspace_root.join(".git");
        if !dot_git.exists() {
            return Ok(None);
        }

        let metadata = fs::symlink_metadata(&dot_git)?;
        let worktree_root = workspace_root
            .canonicalize()
            .unwrap_or_else(|_| workspace_root.to_path_buf());

        if metadata.is_dir() {
            let git_dir = dot_git
                .canonicalize()
                .unwrap_or_else(|_| dot_git.clone());
            let common_dir = resolve_commondir(&git_dir).unwrap_or_else(|| git_dir.clone());
            let kind = if common_dir != git_dir {
                GitRepoKind::Worktree {
                    main_repo_dir: common_dir
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_else(|| common_dir.clone()),
                }
            } else {
                GitRepoKind::Standard
            };

            return Ok(Some(Self {
                worktree_root,
                dot_git,
                is_gitfile: false,
                git_dir,
                common_dir,
                kind,
            }));
        }

        if metadata.is_file() {
            let content = fs::read_to_string(&dot_git)?;
            let Some(raw_gitdir) = parse_gitdir_line(&content) else {
                return Ok(None);
            };

            let gitdir_path = Path::new(raw_gitdir);
            let target_gitdir = if gitdir_path.is_absolute() {
                gitdir_path.to_path_buf()
            } else {
                workspace_root.join(gitdir_path)
            };

            let git_dir = target_gitdir
                .canonicalize()
                .unwrap_or_else(|_| target_gitdir.clone());

            let common_dir = resolve_commondir(&git_dir).unwrap_or_else(|| git_dir.clone());

            let kind = if git_dir
                .to_string_lossy()
                .contains("/modules/")
                || git_dir
                    .to_string_lossy()
                    .contains("\\modules\\")
            {
                let parent_repo = common_dir
                    .ancestors()
                    .nth(2)
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| common_dir.clone());
                GitRepoKind::Submodule {
                    parent_repo_dir: parent_repo,
                }
            } else {
                let main_repo = common_dir
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| common_dir.clone());
                GitRepoKind::Worktree {
                    main_repo_dir: main_repo,
                }
            };

            return Ok(Some(Self {
                worktree_root,
                dot_git,
                is_gitfile: true,
                git_dir,
                common_dir,
                kind,
            }));
        }

        Ok(None)
    }

    pub fn is_worktree(&self) -> bool {
        matches!(self.kind, GitRepoKind::Worktree { .. })
    }

    pub fn is_submodule(&self) -> bool {
        matches!(self.kind, GitRepoKind::Submodule { .. })
    }

    pub fn config_path(&self) -> PathBuf {
        self.common_dir.join("config")
    }

    pub fn worktree_config_path(&self) -> PathBuf {
        self.git_dir.join("config.worktree")
    }

    pub fn hooks_path(&self) -> PathBuf {
        self.common_dir.join("hooks")
    }
}

fn parse_gitdir_line(content: &str) -> Option<&str> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("gitdir:") {
            let path = rest.trim();
            if !path.is_empty() {
                return Some(path);
            }
        }
    }

    None
}

fn resolve_commondir(git_dir: &Path) -> Option<PathBuf> {
    let commondir_file = git_dir.join("commondir");
    if commondir_file.is_file()
        && let Ok(content) = fs::read_to_string(&commondir_file)
    {
        let path_str = content.trim();
        if !path_str.is_empty() {
            let p = Path::new(path_str);
            let resolved = if p.is_absolute() {
                p.to_path_buf()
            } else {
                git_dir.join(p)
            };

            return resolved
                .canonicalize()
                .ok()
                .or(Some(resolved));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn discovers_standard_git_repo() {
        let dir = tempdir().unwrap();
        let dot_git = dir.path().join(".git");
        fs::create_dir(&dot_git).unwrap();
        fs::write(dot_git.join("config"), "[core]\n").unwrap();

        let info = GitRepositoryInfo::discover(dir.path())
            .unwrap()
            .unwrap();
        assert!(!info.is_gitfile);
        assert_eq!(info.kind, GitRepoKind::Standard);
        assert_eq!(info.git_dir, dot_git.canonicalize().unwrap());
        assert_eq!(info.common_dir, dot_git.canonicalize().unwrap());
        assert_eq!(
            info.config_path(),
            dot_git
                .canonicalize()
                .unwrap()
                .join("config")
        );
    }

    #[test]
    fn discovers_worktree_repo_with_gitfile() {
        let dir = tempdir().unwrap();
        let main_repo = dir.path().join("main_repo");
        let worktree_dir = dir.path().join("wt_branch");
        fs::create_dir_all(main_repo.join(".git/worktrees/wt_branch")).unwrap();
        fs::create_dir_all(&worktree_dir).unwrap();

        let gitdir_target = main_repo.join(".git/worktrees/wt_branch");
        fs::write(
            worktree_dir.join(".git"),
            format!("gitdir: {}\n", gitdir_target.display()),
        )
        .unwrap();

        let commondir_target = "../..";
        fs::write(gitdir_target.join("commondir"), commondir_target).unwrap();

        let info = GitRepositoryInfo::discover(&worktree_dir)
            .unwrap()
            .unwrap();
        assert!(info.is_gitfile);
        assert!(info.is_worktree());
        assert_eq!(info.git_dir, gitdir_target.canonicalize().unwrap());
        assert_eq!(
            info.common_dir,
            main_repo
                .join(".git")
                .canonicalize()
                .unwrap()
        );
    }

    #[test]
    fn discovers_submodule_repo() {
        let dir = tempdir().unwrap();
        let main_repo = dir.path().join("main_repo");
        let submodule_dir = main_repo.join("submodule_a");
        let module_git_dir = main_repo.join(".git/modules/submodule_a");
        fs::create_dir_all(&module_git_dir).unwrap();
        fs::create_dir_all(&submodule_dir).unwrap();

        fs::write(
            submodule_dir.join(".git"),
            "gitdir: ../.git/modules/submodule_a\n",
        )
        .unwrap();

        let info = GitRepositoryInfo::discover(&submodule_dir)
            .unwrap()
            .unwrap();
        assert!(info.is_gitfile);
        assert!(info.is_submodule());
        assert_eq!(info.git_dir, module_git_dir.canonicalize().unwrap());
    }

    #[test]
    fn returns_none_when_no_git() {
        let dir = tempdir().unwrap();
        assert!(
            GitRepositoryInfo::discover(dir.path())
                .unwrap()
                .is_none()
        );
    }
}
