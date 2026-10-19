//! Windows per-user desktop packaging and installed-program acceptance.

use std::{fs, path::Path};

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

use crate::{process, remote};

/// Package the existing Debug GUI with WiX; no compilation or user-data migration is performed.
pub async fn build(root: &Path, bundle: &Path, output: &Path) -> Result<()> {
    ensure!(cfg!(windows), "MSI packaging requires native Windows");
    let bundle = bundle.canonicalize()?;
    ensure!(
        bundle.join("orbit-gui.exe").is_file(),
        "missing desktop payload"
    );
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    process::run(
        Command::new("wix")
            .args(["build", "-arch", "x64", "-pdbtype", "none", "-d"])
            .arg(format!("Version={}", env!("CARGO_PKG_VERSION")))
            .arg("-b")
            .arg(format!("gui={}", bundle.display()))
            .arg("-b")
            .arg(format!(
                "assets={}",
                root.join("crates/orbit-gui/resources")
                    .display()
            ))
            .arg(root.join("crates/orbit-xtask/resources/windows.wxs"))
            .arg("-o")
            .arg(output),
    )
    .await?;
    println!("Created per-user Debug installer: {}", output.display());
    Ok(())
}

/// Install into isolated storage, verify the actual installed window, and always attempt uninstall.
pub async fn verify(installer: &Path, server: &Path) -> Result<()> {
    ensure!(cfg!(windows), "MSI acceptance requires native Windows");
    let installer = installer.canonicalize()?;
    let server = server.canonicalize()?;
    ensure_no_shader_sources()?;
    let directory = tempfile::tempdir()?;
    let install = directory.path().join("Orbit 安装验收");
    let log = directory.path().join("install.log");
    let installed = process::run(
        Command::new("msiexec")
            .arg("/i")
            .arg(&installer)
            .args(["/qn", "/norestart"])
            .arg(format!("INSTALLFOLDER={}", install.display()))
            .arg("/l*v")
            .arg(&log),
    )
    .await;
    if let Err(error) = installed {
        anyhow::bail!("installation failed: {error:#}\n{}", installer_log(&log));
    }
    let retained = install.join("unmanaged-user-data.txt");
    let result = async {
        fs::write(&retained, "synthetic user data")?;
        remote::verify_gui_executable(&install.join("orbit-gui.exe"), &server).await
    }
    .await;
    let uninstall = process::run(
        Command::new("msiexec")
            .arg("/x")
            .arg(&installer)
            .args(["/qn", "/norestart"])
            .arg("/l*v")
            .arg(&log),
    )
    .await;
    uninstall.with_context(|| format!("uninstall failed: {}", installer_log(&log)))?;
    ensure!(
        !install.join("orbit-gui.exe").exists(),
        "uninstall retained executable"
    );
    result?;
    ensure!(
        fs::read_to_string(retained)? == "synthetic user data",
        "uninstall changed user data"
    );
    println!("PASS: MSI installed, rendered without Cargo shader sources, and uninstalled");
    Ok(())
}

fn ensure_no_shader_sources() -> Result<()> {
    let home = std::env::var_os("CARGO_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|path| Path::new(&path).join(".cargo")))
        .context("cannot locate Cargo home for clean-machine acceptance")?;
    check_shader_sources(&home.join("registry/src"))
}

fn check_shader_sources(sources: &Path) -> Result<()> {
    if !sources.exists() {
        return Ok(());
    }
    for registry in fs::read_dir(sources)? {
        for package in fs::read_dir(registry?.path())? {
            let package = package?;
            ensure!(
                !package
                    .file_name()
                    .to_string_lossy()
                    .starts_with("gpui-pre-windows-"),
                "acceptance requires a fresh runner without gpui-pre-windows sources"
            );
        }
    }
    Ok(())
}

fn installer_log(path: &Path) -> String {
    let bytes = fs::read(path).unwrap_or_default();
    if bytes.starts_with(&[0xff, 0xfe]) {
        let units: Vec<_> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_installer_diagnostics_decode_unicode_and_missing_logs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("installer.log");
        assert!(installer_log(&path).is_empty());
        let message = "Installation failed: 目录不存在\r\n";
        let bytes: Vec<_> = std::iter::once(0xfeff_u16)
            .chain(message.encode_utf16())
            .flat_map(u16::to_le_bytes)
            .collect();
        fs::write(&path, bytes).unwrap();
        assert_eq!(installer_log(&path), message);
        fs::write(&path, "UTF-8 diagnostic").unwrap();
        assert_eq!(installer_log(&path), "UTF-8 diagnostic");
    }

    #[test]
    fn clean_machine_acceptance_rejects_renderer_sources() {
        let directory = tempfile::tempdir().unwrap();
        let sources = directory.path().join("registry/src");
        check_shader_sources(&sources).unwrap();
        fs::create_dir_all(sources.join("synthetic-registry/another-package-1.0.0")).unwrap();
        check_shader_sources(&sources).unwrap();
        fs::create_dir(sources.join("synthetic-registry/gpui-pre-windows-0.3.9")).unwrap();
        assert!(
            check_shader_sources(&sources)
                .unwrap_err()
                .to_string()
                .contains("fresh runner")
        );
    }
}
