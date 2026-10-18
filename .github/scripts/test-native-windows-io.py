"""Exercise production Windows IPC and cache modules before the full service links."""

import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib

root = Path(__file__).resolve().parents[2]
lock = tomllib.loads((root / "Cargo.lock").read_text())


def version(name, required=None):
    matches = [package["version"] for package in lock["package"] if package["name"] == name]
    if required:
        assert required in matches
        return required
    assert len(matches) == 1, (name, matches)
    return matches[0]


with tempfile.TemporaryDirectory(prefix="orbit-native-io-") as temporary:
    directory = Path(temporary)
    transport = root / "crates/orbit-service/src/workspace/server/transport/windows.rs"
    cache = root / "crates/orbit-client/src/local_server.rs"
    runtime = root / "crates/orbit-runtime"
    (directory / "lib.rs").write_text(f"""
// Production call sites are outside this component harness.
#![allow(dead_code)]
#[path = {json.dumps(transport.as_posix())}] mod transport;
#[path = {json.dumps(cache.as_posix())}] mod cache;
""")
    (directory / "Cargo.toml").write_text(f"""
[package]
name = "orbit-native-windows-io-contracts"
version = "0.0.0"
edition = "2024"
[lib]
path = "lib.rs"
[dependencies]
orbit-runtime = {{ path = {json.dumps(runtime.as_posix())} }}
anyhow = "={version('anyhow')}"
sha2 = "={version('sha2', '0.11.0')}"
tempfile = "={version('tempfile')}"
tokio = {{ version = "={version('tokio')}", features = ["full"] }}
windows-sys = {{ version = "=0.61.2", features = [
    "Win32_Foundation", "Win32_Security", "Win32_Security_Authorization",
    "Win32_System_Pipes", "Win32_System_Threading",
] }}
""")
    shutil.copyfile(root / "Cargo.lock", directory / "Cargo.lock")
    common = ["--manifest-path", str(directory / "Cargo.toml"),
              "--target-dir", str(root / "target/native-windows-io-contracts")]
    subprocess.run(["cargo", "metadata", *common[:2], "--format-version", "1"],
                   check=True, stdout=subprocess.DEVNULL)
    subprocess.run(["cargo", "test", *common, "--locked"], check=True)
