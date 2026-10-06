use std::{process::Stdio, time::Duration};

pub async fn branch(workspace: &str) -> Option<String> {
    for args in [
        vec!["symbolic-ref", "--quiet", "--short", "HEAD"],
        vec!["rev-parse", "--short", "HEAD"],
    ] {
        let output = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::process::Command::new("git")
                .env_clear()
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .arg("--no-optional-locks")
                .arg("-C")
                .arg(workspace)
                .args(args)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .ok()?
        .ok()?;

        if output.status.success() {
            let value = String::from_utf8(output.stdout).ok()?;
            let value = value.trim();
            return (!value.is_empty()).then(|| value.to_owned());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn branch_detects_repositories_and_updates_after_switching() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(branch(root.path().to_str().unwrap()).await, None);
        let output = std::process::Command::new("git")
            .env_clear()
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(["init", "-b", "test-branch"])
            .arg(root.path())
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let child = root.path().join("src");
        std::fs::create_dir(&child).unwrap();
        assert_eq!(
            branch(child.to_str().unwrap()).await.as_deref(),
            Some("test-branch")
        );
        std::fs::write(root.path().join(".git/HEAD"), "ref: refs/heads/other\n").unwrap();
        assert_eq!(
            branch(child.to_str().unwrap()).await.as_deref(),
            Some("other")
        );
    }
}
