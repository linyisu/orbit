use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use orbit_protocol::{Configuration, Request, Response};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::process::Command;

use crate::process;

const CA_BUNDLES: &[&str] = &[
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/cert.pem",
];

fn find_certificates(
    explicit: Option<PathBuf>,
    candidates: impl IntoIterator<Item = PathBuf>,
) -> Result<PathBuf> {
    let candidates: Vec<_> = explicit.into_iter().chain(candidates).collect();
    for path in &candidates {
        if path
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
        {
            return Ok(path.canonicalize()?);
        }
    }
    anyhow::bail!(
        "No system CA bundle found (checked: {candidates:?}); install ca-certificates or set SSL_CERT_FILE to a PEM bundle"
    )
}

pub async fn transport() -> Result<()> {
    let container = std::env::var_os("ORBIT_TEST_CONTAINER")
        .context("ORBIT_TEST_CONTAINER is required for the SSH stand-in")?;
    let command = std::env::args_os()
        .last()
        .context("SSH command is missing")?;
    let status = Command::new("docker")
        .args([
            OsStr::new("exec"),
            OsStr::new("-i"),
            &container,
            OsStr::new("sh"),
            OsStr::new("-c"),
            &command,
        ])
        .kill_on_drop(true)
        .status()
        .await?;
    std::process::exit(status.code().unwrap_or(1));
}

fn docker(container: &str, args: &[&str]) -> Command {
    let mut command = Command::new("docker");
    command.args(["exec", container]).args(args);
    command
}

async fn text(command: &mut Command) -> Result<String> {
    process::stdout(process::run(command).await?)
}

async fn request(
    child: &mut tokio::process::Child,
    request: Request,
) -> Result<Response<Value, Value>> {
    orbit_protocol::write(
        child.stdin.as_mut().context("missing server stdin")?,
        &request,
    )
    .await?;
    Ok(orbit_protocol::read(child.stdout.as_mut().context("missing server stdout")?).await?)
}

async fn unconfigured_workspace(container: &str, executable: &str) -> Result<()> {
    let mut child = Command::new("docker")
        .args([
            "exec",
            "-i",
            container,
            executable,
            "workspace-service",
            "--workspace",
            "/tmp",
            "--database",
            "/tmp/configuration-test.db",
            "--auth-dir",
            "/tmp/configuration-test-auth",
        ])
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    tokio::time::timeout(Duration::from_secs(45), async {
        orbit_protocol::read_preamble(child.stdout.as_mut().unwrap()).await?;
        ensure!(
            matches!(
                request(
                    &mut child,
                    Request::Hello {
                        version: orbit_protocol::VERSION,
                        workspace: None
                    }
                )
                .await?,
                Response::Hello {
                    version: orbit_protocol::VERSION,
                    ..
                }
            ),
            "server protocol handshake failed"
        );
        let configuration = toml::toml! {
            [providers.test]
            base_url = "http://127.0.0.1:9/v1"
            api_key = "test-only"
            auto_refresh_models = false
            models = ["local-model"]
        }
        .to_string();
        let opened = request(
            &mut child,
            Request::Open {
                resume: None,
                model: Some("test::local-model".into()),
                policy: Default::default(),
                configuration: Some(Configuration {
                    content: configuration,
                    credentials: Default::default(),
                }),
            },
        )
        .await?;
        ensure!(
            matches!(opened, Response::Opened(ref info) if info.model == "test::local-model"),
            "session did not open with local configuration: {opened:?}"
        );
        ensure!(
            matches!(request(&mut child, Request::Shutdown).await?, Response::Ok),
            "server shutdown failed"
        );
        child.stdin.take();
        ensure!(
            child.wait().await?.success(),
            "server exited unsuccessfully"
        );
        process::run(&mut docker(
            container,
            &["test", "!", "-e", "/tmp/config.toml"],
        ))
        .await?;
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("unconfigured workspace verification timed out")??;
    Ok(())
}

async fn deployed_files(container: &str) -> Result<Vec<String>> {
    Ok(text(&mut docker(
        container,
        &["find", "/root/.cache/orbit/servers", "-type", "f"],
    ))
    .await?
    .lines()
    .map(str::to_owned)
    .collect())
}

fn sha256(path: &Path) -> Result<String> {
    Ok(Sha256::digest(fs::read(path)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

async fn in_container(bundle: &Path, container: &str) -> Result<()> {
    let directory = tempfile::Builder::new()
        .prefix("orbit-bundle-test-")
        .tempdir()?;
    let client = directory.path().join("orbit-cli");
    fs::copy(bundle.join("orbit-cli"), &client)?;
    ensure!(
        fs::read_dir(directory.path())?.count() == 1,
        "client has unexpected companion files"
    );
    let wrapper = directory.path().join("ssh");
    // Copy this Rust tool as the transport shim. No interpreter or generated script is required.
    fs::copy(std::env::current_exe()?, &wrapper)?;
    let mut paths = vec![directory.path().to_owned()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let paths = std::env::join_paths(paths)?;
    let connect = || {
        let mut command = Command::new(&client);
        command
            .current_dir(directory.path())
            .env("PATH", &paths)
            .env("ORBIT_TEST_CONTAINER", container)
            .args([
                "connect",
                "--ssh",
                "isolated-test",
                "--workspace",
                "/tmp",
                "--database",
                "/tmp/sessions.db",
                "--auth-dir",
                "/tmp/auth",
                "--list",
            ]);
        command
    };
    process::run(&mut connect()).await?;
    let files = deployed_files(container).await?;
    ensure!(files.len() == 1, "Unexpected deployed binaries: {files:?}");
    let executable = &files[0];
    let deployed = directory.path().join("deployed-server");
    process::run_to_file(&mut docker(container, &["cat", executable]), &deployed).await?;
    let digest = sha256(&deployed)?;
    ensure!(
        *executable == format!("/root/.cache/orbit/servers/{digest}/orbit"),
        "deployed file digest does not match its cache path: {executable}; downloaded SHA-256: {digest}"
    );
    orbit_build::verify_static_server(&deployed).map_err(anyhow::Error::msg)?;
    ensure!(
        digest != sha256(&client)?,
        "Uploaded native client instead of the static server"
    );
    fs::remove_file(deployed)?;
    let before = text(&mut docker(container, &["stat", "-c", "%i:%Y", executable])).await?;
    process::run(&mut connect()).await?;
    let after = text(&mut docker(container, &["stat", "-c", "%i:%Y", executable])).await?;
    ensure!(
        before == after,
        "Cached server was replaced instead of reused"
    );
    ensure!(
        deployed_files(container).await? == files,
        "Unexpected new deployed binaries"
    );
    unconfigured_workspace(container, executable).await?;
    println!(
        "PASS: standalone client, static deployment, local configuration, session opening and cache reuse on Ubuntu 20.04"
    );
    Ok(())
}

pub async fn verify(bundle: &Path) -> Result<()> {
    let bundle = bundle.canonicalize()?;
    ensure!(
        bundle.join("orbit-cli").is_file(),
        "bundle is missing orbit-cli"
    );
    let certificates = find_certificates(
        std::env::var_os("SSL_CERT_FILE").map(PathBuf::from),
        CA_BUNDLES.iter().map(PathBuf::from),
    )?;
    let container = text(
        Command::new("docker")
            .args([
                "run",
                "--detach",
                "--rm",
                "--network",
                "none",
                "--read-only",
                "--tmpfs",
                "/tmp",
                "--tmpfs",
                "/root:exec",
                "--mount",
            ])
            .arg(format!(
                "type=bind,source={},target=/etc/ssl/certs/ca-certificates.crt,readonly",
                certificates.display()
            ))
            .args([
                "--env",
                "SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt",
                "ubuntu:20.04",
                "sleep",
                "300",
            ]),
    )
    .await?;
    ensure!(
        !container.is_empty(),
        "docker did not return a container ID"
    );
    let verification = in_container(&bundle, &container).await;
    let cleanup = process::run(Command::new("docker").args(["rm", "--force", &container])).await;
    verification?;
    cleanup?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn certificate_selection_prefers_explicit_valid_file_and_skips_missing_or_empty_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("system.pem");
        let explicit = dir.path().join("custom.pem");
        let empty = dir.path().join("empty.pem");
        fs::write(&system, "system test certificate").unwrap();
        fs::write(&explicit, "custom test certificate").unwrap();
        fs::write(&empty, "").unwrap();
        assert_eq!(
            find_certificates(Some(explicit.clone()), [system.clone()]).unwrap(),
            explicit
        );
        assert_eq!(
            find_certificates(Some(empty), [dir.path().join("missing"), system.clone()]).unwrap(),
            system
        );
        let error = find_certificates(None, [dir.path().join("missing")])
            .unwrap_err()
            .to_string();
        assert!(error.contains("install ca-certificates or set SSL_CERT_FILE"));
    }

    #[cfg(unix)]
    #[test]
    fn certificate_symlinks_are_resolved_before_the_docker_bind_mount() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("bundle.pem");
        let link = dir.path().join("link.pem");
        fs::write(&bundle, "test certificate").unwrap();
        std::os::unix::fs::symlink(&bundle, &link).unwrap();
        assert_eq!(find_certificates(Some(link), []).unwrap(), bundle);
    }
}
