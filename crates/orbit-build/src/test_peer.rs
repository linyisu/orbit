//! Build offline Rust subprocess peers used by crate-owned regression tests.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

/// Build a crate's named example peer offline and return Cargo's executable receipt.
///
/// Intended for native tests after Cargo has resolved their dependencies. Windows static-CRT
/// invocations retain their explicit host target; the path is never guessed from target layout.
/// Panics with compiler diagnostics if the peer cannot be built or Cargo omits its executable.
pub fn build_test_peer(manifest_directory: &Path, example: &str) -> PathBuf {
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(manifest_directory)
        .args([
            "build",
            "--example",
            example,
            "--locked",
            "--offline",
            "--message-format=json",
        ]);
    if cfg!(windows)
        && std::env::var("CARGO_ENCODED_RUSTFLAGS").is_ok_and(|flags| flags.contains("crt-static"))
    {
        let version = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .arg("-vV")
            .output()
            .expect("query native Rust compiler target");
        assert!(
            version.status.success(),
            "cannot query the native Rust target"
        );
        let version = String::from_utf8(version.stdout).unwrap();
        let host = version
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .expect("Rust compiler must report its native host target");
        command.args(["--target", host]);
    }
    let output = command
        .output()
        .expect("build Rust test peer");
    assert!(
        output.status.success(),
        "cannot build {example}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|message| {
            (message["reason"] == "compiler-artifact" && message["target"]["name"] == example)
                .then(|| {
                    message["executable"]
                        .as_str()
                        .map(PathBuf::from)
                })
                .flatten()
        })
        .expect("Cargo must report the Rust test peer executable")
}
