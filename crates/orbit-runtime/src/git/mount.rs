use std::path::Path;

use super::discovery::GitRepositoryInfo;
use super::policy::GitOperationKind;
use crate::execution::process::MountSpec;

#[derive(Clone, Debug, Default)]
pub struct GitMountPlan {
    pub mounts: Vec<MountSpec>,
    pub env_vars: Vec<(String, String)>,
    /// Whether the sandbox should leave .git unmasked (replacing the blanket empty mask).
    pub unmask_git: bool,
}

impl GitMountPlan {
    pub fn prepare(
        repo: &GitRepositoryInfo,
        workspace_container_root: &Path,
        workspace_is_read_only: bool,
        operation_kind: Option<&GitOperationKind>,
        allow_config_mutation: bool,
    ) -> Self {
        let is_read_only = workspace_is_read_only
            || matches!(operation_kind, Some(GitOperationKind::ReadOnlyQuery { .. }));

        let mut mounts = Vec::new();
        let mut env_vars = vec![
            ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
            ("GIT_PAGER".to_string(), "cat".to_string()),
        ];

        if is_read_only {
            env_vars.push(("GIT_OPTIONAL_LOCKS".to_string(), "0".to_string()));
        }

        if !repo.is_gitfile {
            let container_dot_git = workspace_container_root.join(".git");
            if is_read_only {
                mounts.push(MountSpec::read_only(&repo.git_dir, &container_dot_git));
            } else {
                mounts.push(MountSpec::read_write(&repo.git_dir, &container_dot_git));

                if !allow_config_mutation {
                    let config = repo.config_path();
                    if config.exists() {
                        mounts.push(MountSpec::read_only(
                            &config,
                            container_dot_git.join("config"),
                        ));
                    }

                    let wt_config = repo.worktree_config_path();
                    if wt_config.exists() {
                        mounts.push(MountSpec::read_only(
                            &wt_config,
                            container_dot_git.join("config.worktree"),
                        ));
                    }

                    let hooks = repo.hooks_path();
                    if hooks.exists() {
                        mounts.push(MountSpec::read_only(
                            &hooks,
                            container_dot_git.join("hooks"),
                        ));
                    }
                }
            }
        } else {
            // Gitfiles are visible in /workspace; their metadata may need external mounts.
            if is_read_only {
                mounts.push(MountSpec::read_only(&repo.git_dir, &repo.git_dir));
                if repo.common_dir != repo.git_dir {
                    mounts.push(MountSpec::read_only(&repo.common_dir, &repo.common_dir));
                }
            } else {
                mounts.push(MountSpec::read_write(&repo.git_dir, &repo.git_dir));
                if repo.common_dir != repo.git_dir {
                    mounts.push(MountSpec::read_write(&repo.common_dir, &repo.common_dir));
                }

                if !allow_config_mutation {
                    let config = repo.config_path();
                    if config.exists() {
                        mounts.push(MountSpec::read_only(&config, &config));
                    }
                    let wt_config = repo.worktree_config_path();
                    if wt_config.exists() {
                        mounts.push(MountSpec::read_only(&wt_config, &wt_config));
                    }
                    let hooks = repo.hooks_path();
                    if hooks.exists() {
                        mounts.push(MountSpec::read_only(&hooks, &hooks));
                    }
                }
            }
        }

        Self {
            mounts,
            env_vars,
            unmask_git: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::git::discovery::GitRepositoryInfo;

    #[test]
    fn read_only_mount_sets_optional_locks_zero() {
        let dir = tempdir().unwrap();
        let dot_git = dir.path().join(".git");
        fs::create_dir(&dot_git).unwrap();
        fs::write(dot_git.join("config"), "[core]\n").unwrap();

        let repo = GitRepositoryInfo::discover(dir.path()).unwrap().unwrap();
        let plan = GitMountPlan::prepare(
            &repo,
            Path::new("/workspace"),
            true, // read-only
            Some(&GitOperationKind::ReadOnlyQuery {
                subcommand: "status".into(),
            }),
            false,
        );

        assert!(plan.unmask_git);
        assert!(
            plan.env_vars
                .iter()
                .any(|(k, v)| k == "GIT_OPTIONAL_LOCKS" && v == "0")
        );
        assert_eq!(plan.mounts.len(), 1);
        assert!(plan.mounts[0].access.is_read_only());
        assert_eq!(plan.mounts[0].target, Path::new("/workspace/.git"));
    }

    #[test]
    fn read_write_mount_protects_config_and_hooks() {
        let dir = tempdir().unwrap();
        let dot_git = dir.path().join(".git");
        fs::create_dir_all(dot_git.join("hooks")).unwrap();
        fs::write(dot_git.join("config"), "[core]\n").unwrap();

        let repo = GitRepositoryInfo::discover(dir.path()).unwrap().unwrap();
        let plan = GitMountPlan::prepare(
            &repo,
            Path::new("/workspace"),
            false, // read-write
            Some(&GitOperationKind::LocalWrite {
                subcommand: "commit".into(),
                description: "commit".into(),
            }),
            false,
        );

        assert!(plan.unmask_git);
        assert!(!plan.env_vars.iter().any(|(k, _)| k == "GIT_OPTIONAL_LOCKS"));

        assert!(
            plan.mounts
                .iter()
                .any(|m| m.target == Path::new("/workspace/.git") && m.access.is_read_write())
        );
        assert!(plan.mounts.iter().any(|m| m.target == Path::new("/workspace/.git/config") && m.access.is_read_only()));
        assert!(
            plan.mounts
                .iter()
                .any(|m| m.target == Path::new("/workspace/.git/hooks") && m.access.is_read_only())
        );
    }
}
