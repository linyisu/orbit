//! Windows per-user desktop packaging and installed-program acceptance.

use std::{fs, path::Path, time::Duration};

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

use crate::{process, remote};

/// Package the existing x64 Debug GUI with NSIS, without rebuilding the application.
pub async fn build(root: &Path, bundle: &Path, output: &Path) -> Result<()> {
    ensure!(cfg!(windows), "installer packaging requires native Windows");
    ensure!(
        std::env::consts::ARCH == "x86_64",
        "this installer requires an x64 payload"
    );
    let bundle = std::path::absolute(bundle)?;
    let output = std::path::absolute(output)?;
    ensure!(
        bundle.join("orbit-gui.exe").is_file(),
        "missing desktop payload"
    );
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    let compiler = std::env::var_os("ProgramFiles(x86)")
        .map(|path| Path::new(&path).join("NSIS/makensis.exe"))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| "makensis".into());
    process::run(
        Command::new(compiler)
            .current_dir(root)
            .args(["/NOCD", "/INPUTCHARSET", "UTF8"])
            .arg(format!("/DVERSION={}", env!("CARGO_PKG_VERSION")))
            .arg(format!("/DGUI_DIR={}", bundle.display()))
            .arg(format!("/DOUTPUT={}", output.display()))
            .arg(format!(
                "/DASSETS={}",
                root.join("crates/orbit-gui/resources")
                    .display()
            ))
            .arg(root.join("crates/orbit-xtask/resources/windows.nsi")),
    )
    .await?;
    println!("Created per-user Debug installer: {}", output.display());
    Ok(())
}

/// Install into isolated storage, verify the installed window, and always attempt uninstall.
pub async fn verify(installer: &Path) -> Result<()> {
    ensure!(
        cfg!(windows),
        "installer acceptance requires native Windows"
    );
    let installer = installer.canonicalize()?;
    ensure_no_shader_sources()?;
    let directory = tempfile::tempdir()?;
    let install = directory.path().join("Orbit 安装验收");
    let mut command = Command::new(installer);
    command.arg("/S");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;

        // NSIS requires /D last and unquoted, including when the directory contains spaces.
        command
            .as_std_mut()
            .raw_arg(format!("/D={}", install.display()));
    }
    process::run(&mut command)
        .await
        .context("installation failed")?;
    let retained = install.join("unmanaged-user-data.txt");
    let result = async {
        fs::write(&retained, "synthetic user data")?;
        remote::verify_gui_executable(&install.join("orbit-gui.exe"), None).await
    }
    .await;
    process::run(Command::new(install.join("uninstall.exe")).arg("/S"))
        .await
        .context("uninstall failed")?;
    // NSIS transfers uninstallation to a temporary executable so it can remove itself.
    tokio::time::timeout(Duration::from_secs(30), async {
        while install.join("uninstall.exe").exists() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("uninstaller did not finish")?;
    ensure!(
        !install.join("orbit-gui.exe").exists(),
        "uninstall retained executable"
    );
    result?;
    ensure!(
        fs::read_to_string(retained)? == "synthetic user data",
        "uninstall changed user data"
    );
    println!("PASS: installed, rendered without Cargo shader sources, and uninstalled");
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

#[cfg(test)]
mod tests {
    use super::*;

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
