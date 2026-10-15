use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use tokio::process::Command;

use crate::{Profile, process};

fn cargo(root: &Path) -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.current_dir(root);
    command
}

pub async fn target_directory(root: &Path) -> Result<PathBuf> {
    let output = process::run(cargo(root).args([
        "metadata",
        "--offline",
        "--no-deps",
        "--format-version",
        "1",
    ]))
    .await?;
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    Ok(PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .context("Cargo target directory missing")?,
    ))
}

fn artifacts(output: &[u8]) -> Result<BTreeMap<String, PathBuf>> {
    let mut files = BTreeMap::new();
    for line in output
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let message: Value = serde_json::from_slice(line)?;
        if message["reason"] == "compiler-artifact"
            && let (Some(name), Some(executable)) = (
                message["target"]["name"].as_str(),
                message["executable"].as_str(),
            )
            && name == "orbit"
        {
            files.insert(name.to_owned(), executable.into());
        }
        if message["reason"] == "build-script-executed"
            && let Some(environment) = message["env"].as_array()
        {
            for variable in environment {
                if variable[0] == "ORBIT_EMBEDDED_SERVER"
                    && let Some(path) = variable[1].as_str()
                {
                    files.insert("orbit-workspace-server".into(), path.into());
                }
            }
        }
    }
    ensure!(
        files.contains_key("orbit") && files.contains_key("orbit-workspace-server"),
        "Cargo did not report the terminal client and its embedded server"
    );
    Ok(files)
}

fn install(source: &Path, destination: &Path) -> Result<()> {
    fs::copy(source, destination).with_context(|| format!("install {}", source.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

pub async fn build(root: &Path, profile: Profile) -> Result<()> {
    let mut command = cargo(root);
    command.args([
        "build",
        "--locked",
        "--message-format=json-render-diagnostics",
        "-p",
        "orbit-tui",
    ]);
    if matches!(profile, Profile::Release) {
        command.arg("--release");
    }
    // Compilation can take substantially longer than the bounded verification commands.
    let output = command
        .kill_on_drop(true)
        .stderr(Stdio::inherit())
        .output()
        .await?;
    ensure!(
        output.status.success(),
        "client build failed: {}",
        output.status
    );
    let files = artifacts(&output.stdout)?;
    let output = target_directory(root)
        .await?
        .join("bundle");
    fs::create_dir_all(&output)?;
    for obsolete in [
        "orbit-server-x86_64-linux-musl",
        "orbit-server-aarch64-linux-musl",
        "orbit-cli",
    ] {
        match fs::remove_file(output.join(obsolete)) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    for (name, executable) in files {
        install(&executable, &output.join(name))?;
    }
    println!("Bundle: {} (workspace server embedded)", output.display());
    Ok(())
}

pub async fn archive(directory: &Path, output: &Path) -> Result<()> {
    let directory = directory.canonicalize()?;
    ensure!(directory.join("orbit").is_file(), "missing bundled orbit");
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    process::run(
        Command::new("tar")
            .arg("-czf")
            .arg(output)
            .arg("-C")
            .arg(directory)
            .arg("orbit"),
    )
    .await?;
    println!("Archive: {}", output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn archive_contains_only_the_terminal_and_preserves_executable_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let bundle = directory.path().join("bundle");
        let source = directory.path().join("terminal");
        fs::create_dir(&bundle).unwrap();
        fs::write(&source, b"synthetic terminal").unwrap();
        install(&source, &bundle.join("orbit")).unwrap();
        fs::write(bundle.join("orbit-workspace-server"), b"verification asset").unwrap();
        let output = directory.path().join("release.tar.gz");
        archive(&bundle, &output).await.unwrap();
        let listing = process::stdout(
            process::run(
                Command::new("tar")
                    .arg("-tzf")
                    .arg(&output),
            )
            .await
            .unwrap(),
        )
        .unwrap();
        assert_eq!(listing, "orbit");
        let extracted = directory.path().join("extracted");
        fs::create_dir(&extracted).unwrap();
        process::run(
            Command::new("tar")
                .arg("-xzf")
                .arg(&output)
                .arg("-C")
                .arg(&extracted),
        )
        .await
        .unwrap();
        assert_eq!(
            fs::read(extracted.join("orbit")).unwrap(),
            b"synthetic terminal"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(extracted.join("orbit"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
    }

    #[test]
    fn uses_cargo_artifacts_instead_of_assuming_a_target_layout() {
        let messages = [
            serde_json::json!({
                "reason": "compiler-artifact",
                "target": {
                    "name": "orbit-build"
                },
                "executable": null
            }),
            serde_json::json!({
                "reason": "compiler-artifact",
                "target": {
                    "name": "orbit"
                },
                "executable": "/custom-target/triple/release/orbit"
            }),
            serde_json::json!({
                "reason": "build-script-executed",
                "env": [["ORBIT_EMBEDDED_SERVER", "/static-target/orbit-workspace-server"]]
            }),
            serde_json::json!({
                "reason": "build-finished",
                "success": true
            }),
        ]
        .map(|message| message.to_string())
        .join("\n");
        let files = artifacts(messages.as_bytes()).unwrap();
        assert_eq!(
            files["orbit"],
            Path::new("/custom-target/triple/release/orbit")
        );
        assert_eq!(
            files["orbit-workspace-server"],
            Path::new("/static-target/orbit-workspace-server")
        );
        assert!(artifacts(b"{\"reason\":\"build-finished\",\"success\":true}").is_err());
    }
}
