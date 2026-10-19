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

fn artifacts(output: &[u8], client: &str) -> Result<BTreeMap<String, PathBuf>> {
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
            && name == client
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
        files.contains_key(client) && files.contains_key("orbit-workspace-server"),
        "Cargo did not report the requested client and its embedded server"
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

fn executable_name(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

async fn build_client(
    root: &Path,
    profile: Profile,
    package: &str,
    executable: &str,
) -> Result<BTreeMap<String, PathBuf>> {
    let mut command = cargo(root);
    command.args([
        "build",
        "--locked",
        "--message-format=json-render-diagnostics",
        "-p",
        package,
    ]);
    #[cfg(windows)]
    {
        let output = process::run(Command::new("rustc").arg("-vV")).await?;
        let version = String::from_utf8(output.stdout)?;
        let host = version
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .context("rustc did not report its native target")?;
        ensure!(
            host.ends_with("-pc-windows-msvc"),
            "Windows bundles require MSVC"
        );
        command.args(["--target", host]);
        let mut flags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_else(|_| {
            std::env::var("RUSTFLAGS")
                .unwrap_or_default()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("\x1f")
        });
        if !flags.is_empty() {
            flags.push('\x1f');
        }
        flags.push_str("-C\x1ftarget-feature=+crt-static");
        command.env("CARGO_ENCODED_RUSTFLAGS", flags);
    }
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
    artifacts(&output.stdout, executable)
}

pub async fn build(root: &Path, profile: Profile) -> Result<()> {
    let files = build_client(root, profile, "orbit-tui", "orbit").await?;
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
        install(&executable, &output.join(executable_name(&name)))?;
    }
    println!("Bundle: {} (workspace server embedded)", output.display());
    Ok(())
}

pub async fn archive(directory: &Path, output: &Path) -> Result<()> {
    let directory = directory.canonicalize()?;
    ensure!(
        directory
            .join(executable_name("orbit"))
            .is_file(),
        "missing bundled orbit"
    );
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    process::run(
        Command::new("tar")
            .arg("-czf")
            .arg(output)
            .arg("-C")
            .arg(directory)
            .arg(executable_name("orbit")),
    )
    .await?;
    println!("Archive: {}", output.display());
    Ok(())
}

/// Install the desktop executable with native launch metadata; the service remains embedded.
pub async fn build_gui(root: &Path, profile: Profile) -> Result<()> {
    let files = build_client(root, profile, "orbit-gui", "orbit-gui").await?;
    let output = target_directory(root)
        .await?
        .join("bundle-gui");
    fs::create_dir_all(&output)?;
    install_gui(root, &output, &files["orbit-gui"])?;
    #[cfg(target_os = "macos")]
    process::run(
        Command::new("codesign")
            .args(["--force", "--sign", "-"])
            .arg(output.join("Orbit.app")),
    )
    .await?;
    install(
        &files["orbit-workspace-server"],
        &output.join(executable_name("orbit-workspace-server")),
    )?;
    println!("Desktop bundle: {}", output.display());
    Ok(())
}

pub fn gui_executable(directory: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        directory.join("Orbit.app/Contents/MacOS/orbit-gui")
    } else {
        directory.join(executable_name("orbit-gui"))
    }
}

pub fn gui_archive_extension() -> &'static str {
    if cfg!(windows) { "zip" } else { "tar.gz" }
}

fn install_gui(root: &Path, output: &Path, executable: &Path) -> Result<()> {
    let resources = root.join("crates/orbit-gui/resources");
    let destination = gui_executable(output);
    fs::create_dir_all(
        destination
            .parent()
            .context("desktop executable directory")?,
    )?;
    install(executable, &destination)?;
    if cfg!(target_os = "macos") {
        let contents = output.join("Orbit.app/Contents");
        fs::create_dir_all(contents.join("Resources"))?;
        fs::copy(
            resources.join("orbit.icns"),
            contents.join("Resources/orbit.icns"),
        )?;
        fs::write(
            contents.join("Info.plist"),
            fs::read_to_string(resources.join("Info.plist"))?
                .replace("ORBIT_PACKAGE_VERSION", env!("CARGO_PKG_VERSION")),
        )?;
    } else if cfg!(target_os = "linux") {
        fs::copy(
            resources.join("orbit.desktop"),
            output.join("orbit.desktop"),
        )?;
        fs::copy(
            root.join("assets/branding/orbit.svg"),
            output.join("orbit.svg"),
        )?;
    }
    Ok(())
}

/// Archive only desktop launch assets; the extracted service is a verification fixture.
pub async fn archive_gui(directory: &Path, output: &Path) -> Result<()> {
    let directory = directory.canonicalize()?;
    ensure!(
        gui_executable(&directory).is_file(),
        "missing desktop executable"
    );
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    if cfg!(windows) {
        ensure!(
            output
                .extension()
                .is_some_and(|extension| extension == "zip"),
            "Windows desktop archives use .zip"
        );
        let mut archive = zip::ZipWriter::new(fs::File::create(output)?);
        archive.start_file(
            executable_name("orbit-gui"),
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored),
        )?;
        std::io::copy(
            &mut fs::File::open(gui_executable(&directory))?,
            &mut archive,
        )?;
        archive.finish()?;
        println!("Desktop archive: {}", output.display());
        return Ok(());
    }
    let mut command = Command::new("tar");
    command
        .arg("-czf")
        .arg(output)
        .arg("-C")
        .arg(directory);
    if cfg!(target_os = "macos") {
        command.arg("Orbit.app");
    } else {
        command.arg(executable_name("orbit-gui"));
        if cfg!(target_os = "linux") {
            command.args(["orbit.desktop", "orbit.svg"]);
        }
    }
    process::run(&mut command).await?;
    println!("Desktop archive: {}", output.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn desktop_archive_preserves_native_launch_assets_and_excludes_service_fixture() {
        let directory = tempfile::tempdir().unwrap();
        let bundle = directory.path().join("desktop");
        let source = directory
            .path()
            .join("synthetic-client");
        fs::create_dir(&bundle).unwrap();
        fs::write(&source, b"generated desktop executable").unwrap();
        install_gui(crate::root(), &bundle, &source).unwrap();
        fs::write(
            bundle.join(executable_name("orbit-workspace-server")),
            b"fixture",
        )
        .unwrap();
        let output = directory
            .path()
            .join(format!("desktop.{}", gui_archive_extension()));
        archive_gui(&bundle, &output)
            .await
            .unwrap();
        let extracted = directory.path().join("unpacked");
        fs::create_dir(&extracted).unwrap();
        if cfg!(windows) {
            zip::ZipArchive::new(fs::File::open(&output).unwrap())
                .unwrap()
                .extract(&extracted)
                .unwrap();
        } else {
            process::run(
                Command::new("tar")
                    .arg("-xzf")
                    .arg(output)
                    .arg("-C")
                    .arg(&extracted),
            )
            .await
            .unwrap();
        }
        assert_eq!(
            fs::read(gui_executable(&extracted)).unwrap(),
            b"generated desktop executable"
        );
        assert!(
            !extracted
                .join(executable_name("orbit-workspace-server"))
                .exists()
        );
        if cfg!(target_os = "macos") {
            let contents = extracted.join("Orbit.app/Contents");
            let plist = fs::read_to_string(contents.join("Info.plist")).unwrap();
            assert!(plist.contains("NSMicrophoneUsageDescription"));
            assert!(plist.contains(env!("CARGO_PKG_VERSION")));
            assert!(!plist.contains("ORBIT_PACKAGE_VERSION"));
            assert!(
                contents
                    .join("Resources/orbit.icns")
                    .is_file()
            );
        } else if cfg!(target_os = "linux") {
            assert!(
                fs::read_to_string(extracted.join("orbit.desktop"))
                    .unwrap()
                    .contains("Exec=orbit-gui")
            );
            assert!(extracted.join("orbit.svg").is_file());
        }
    }

    #[tokio::test]
    async fn archive_contains_only_the_terminal_and_preserves_executable_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let bundle = directory.path().join("bundle");
        let source = directory.path().join("terminal");
        fs::create_dir(&bundle).unwrap();
        fs::write(&source, b"synthetic terminal").unwrap();
        install(&source, &bundle.join(executable_name("orbit"))).unwrap();
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
        assert_eq!(listing, executable_name("orbit"));
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
            fs::read(extracted.join(executable_name("orbit"))).unwrap(),
            b"synthetic terminal"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(extracted.join(executable_name("orbit")))
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
        let files = artifacts(messages.as_bytes(), "orbit").unwrap();
        assert_eq!(
            files["orbit"],
            Path::new("/custom-target/triple/release/orbit")
        );
        assert_eq!(
            files["orbit-workspace-server"],
            Path::new("/static-target/orbit-workspace-server")
        );
        assert!(artifacts(b"{\"reason\":\"build-finished\",\"success\":true}", "orbit").is_err());
    }
}
