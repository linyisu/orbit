"""Run the production process owner in isolation while filesystem ports are incomplete."""

import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import tomllib

root = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser()
parser.add_argument("--check-target")
parser.add_argument("--offline", action="store_true")
args = parser.parse_args()
lock = tomllib.loads((root / "Cargo.lock").read_text())


def version(name, required=None):
    matches = [p["version"] for p in lock["package"] if p["name"] == name]
    if required:
        assert required in matches
        return required
    assert len(matches) == 1, (name, matches)
    return matches[0]


with tempfile.TemporaryDirectory(prefix="orbit-native-process-") as temporary:
    directory = Path(temporary)
    # The harness imports actual production files and their tests, not copied implementations.
    process = root / "crates/orbit-runtime/src/execution/process"
    paths = {name: json.dumps((process / f"{name}.rs").as_posix())
             for name in ["unix", "windows", "tests"]}
    (directory / "lib.rs").write_text(f"""
pub mod execution {{ pub mod process {{
    #[cfg(unix)] #[path = {paths['unix']}] mod platform;
    #[cfg(windows)] #[path = {paths['windows']}] mod platform;
    pub use platform::ProcessGroup;
    #[cfg(test)] #[path = {paths['tests']}] mod tests;
}} }}
""")
    (directory / "Cargo.toml").write_text(f"""
[package]
name = "orbit-native-process-contracts"
version = "0.0.0"
edition = "2024"
[lib]
path = "lib.rs"
[dependencies]
tokio = {{ version = "={version('tokio')}", features = ["process", "io-util", "macros", "rt-multi-thread", "time"] }}
tempfile = "={version('tempfile')}"
[target.'cfg(unix)'.dependencies]
libc = "={version('libc')}"
[target.'cfg(windows)'.dependencies]
windows-sys = {{ version = "={version('windows-sys', '0.61.2')}", features = [
    "Win32_Foundation", "Win32_Security", "Win32_System_JobObjects",
    "Win32_System_Threading", "Win32_System_Diagnostics_ToolHelp", "Win32_System_Console",
] }}
""")
    shutil.copyfile(root / "Cargo.lock", directory / "Cargo.lock")
    common = ["--manifest-path", str(directory / "Cargo.toml"),
              "--target-dir", str(root / "target/native-process-contracts")]
    if args.offline:
        common.append("--offline")
    # Cargo prunes unrelated workspace packages from this temporary lock; pinned direct
    # dependencies and the copied lock retain the repository's dependency selection.
    subprocess.run(["cargo", "metadata", *common[:2], "--format-version", "1",
                    *(["--offline"] if args.offline else [])], check=True,
                   stdout=subprocess.DEVNULL)
    if args.check_target:
        subprocess.run(["cargo", "clippy", *common, "--locked", "--tests", "--target",
                        args.check_target, "--", "-D", "warnings"], check=True)
    else:
        subprocess.run(["cargo", "clippy", *common, "--locked", "--tests", "--",
                        "-D", "warnings"], check=True)
        subprocess.run(["cargo", "test", *common, "--locked"], check=True)
