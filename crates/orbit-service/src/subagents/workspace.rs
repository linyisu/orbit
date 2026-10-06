use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Strategy {
    #[default]
    Worktree,
    Shared,
}

pub(super) async fn prepare(
    parent: &Path,
    database: &Path,
    agent: &str,
    strategy: Strategy,
) -> Result<PathBuf> {
    if matches!(strategy, Strategy::Shared) {
        return Ok(parent.canonicalize()?);
    }
    ensure!(
        agent.chars().all(|value| value.is_ascii_hexdigit()) && agent.len() == 32,
        "invalid child identity"
    );
    let destination = database.with_extension("worktrees").join(agent);
    ensure!(!destination.exists(), "child worktree already exists");
    let root = git(parent, &["rev-parse", "--show-toplevel"])
        .await
        .context(
            "开发子代理默认使用 Git worktree；非 Git 项目需要显式指定 workspace_strategy=shared",
        )?;
    let root = PathBuf::from(root.trim());
    let base = git(&root, &["rev-parse", "HEAD"]).await?;
    let relative = parent
        .canonicalize()?
        .strip_prefix(root.canonicalize()?)?
        .to_path_buf();
    if !relative.as_os_str().is_empty() {
        let tree = format!(
            "{}:{}",
            base.trim(),
            relative.to_str().context("workspace path is not UTF-8")?
        );
        ensure!(
            git(&root, &["cat-file", "-t", &tree]).await?.trim() == "tree",
            "当前目录在 HEAD 中不存在，请在仓库根目录创建开发子代理"
        );
    }
    std::fs::create_dir_all(destination.parent().unwrap())?;
    let branch = format!("orbit/agent-{agent}");
    git(
        &root,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            "--",
            destination.to_str().context("worktree path is not UTF-8")?,
            base.trim(),
        ],
    )
    .await?;
    let workspace = destination.join(relative);
    ensure!(
        workspace.is_dir(),
        "当前目录在 HEAD 中不存在，请在仓库根目录创建开发子代理"
    );
    Ok(workspace)
}

async fn git(root: &Path, arguments: &[&str]) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(arguments)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    ensure!(
        output.status.success(),
        "git: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(String::from_utf8(output.stdout)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn development_worktrees_keep_changes_separate_and_survive_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let repo = directory.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]).await.unwrap();
        std::fs::write(repo.join("file.txt"), "base").unwrap();
        git(&repo, &["add", "file.txt"]).await.unwrap();
        git(
            &repo,
            &[
                "-c",
                "user.name=Orbit Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "base",
            ],
        )
        .await
        .unwrap();
        let child = prepare(
            &repo,
            &directory.path().join("sessions.db"),
            "0123456789abcdef0123456789abcdef",
            Strategy::Worktree,
        )
        .await
        .unwrap();
        std::fs::write(child.join("file.txt"), "child").unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.join("file.txt")).unwrap(),
            "base"
        );
        assert!(
            git(&child, &["diff", "--", "file.txt"])
                .await
                .unwrap()
                .contains("+child")
        );
        assert!(
            git(&child, &["branch", "--show-current"])
                .await
                .unwrap()
                .starts_with("orbit/agent-")
        );
        let untracked = repo.join("untracked-directory");
        std::fs::create_dir(&untracked).unwrap();
        let rejected = "abcdef0123456789abcdef0123456789";
        assert!(
            prepare(
                &untracked,
                &directory.path().join("sessions.db"),
                rejected,
                Strategy::Worktree,
            )
            .await
            .is_err()
        );
        assert!(
            !directory
                .path()
                .join("sessions.worktrees")
                .join(rejected)
                .exists()
        );
        assert!(
            !git(&repo, &["branch", "--list"])
                .await
                .unwrap()
                .contains(rejected)
        );
    }

    #[tokio::test]
    async fn non_git_development_requires_explicit_shared_strategy() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("sessions.db");
        assert!(
            prepare(
                directory.path(),
                &database,
                "0123456789abcdef0123456789abcdef",
                Strategy::Worktree
            )
            .await
            .is_err()
        );
        assert_eq!(
            prepare(
                directory.path(),
                &database,
                "0123456789abcdef0123456789abcdef",
                Strategy::Shared
            )
            .await
            .unwrap(),
            directory.path()
        );
    }
}
