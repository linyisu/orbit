#!/usr/bin/env python3
"""Exercise automatic deployment and cache reuse on older Linux through an SSH stand-in."""

import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile


def run(*arguments, **kwargs):
    return subprocess.run(arguments, check=True, capture_output=True, text=True, timeout=120, **kwargs)


def main():
    bundle = Path(sys.argv[1] if len(sys.argv) > 1 else "target/bundle").resolve()
    container = run(
        "docker", "run", "--detach", "--rm", "--network", "none", "--read-only",
        "--tmpfs", "/tmp", "--tmpfs", "/root:exec", "ubuntu:20.04", "sleep", "300",
    ).stdout.strip()
    try:
        with tempfile.TemporaryDirectory(prefix="orbit-bundle-test-") as directory:
            client = Path(directory) / "orbit"
            shutil.copy2(bundle / "orbit", client)
            assert list(Path(directory).iterdir()) == [client]
            wrapper = Path(directory) / "ssh"
            wrapper.write_text(
                f"#!{sys.executable}\n"
                "import os, sys\n"
                "os.execvp('docker', ['docker', 'exec', '-i', os.environ['ORBIT_TEST_CONTAINER'], 'sh', '-c', sys.argv[-1]])\n"
            )
            wrapper.chmod(0o700)
            environment = {
                **os.environ,
                "PATH": directory + os.pathsep + os.environ["PATH"],
                "ORBIT_TEST_CONTAINER": container,
            }
            arguments = (
                str(client), "connect", "--ssh", "isolated-test", "--workspace", "/tmp",
                "--database", "/tmp/sessions.db", "--auth-dir", "/tmp/auth", "--list",
            )
            run(*arguments, env=environment, cwd=directory)
            files = run("docker", "exec", container, "find", "/root/.cache/orbit/servers", "-type", "f").stdout.splitlines()
            assert len(files) == 1, f"Unexpected deployed binaries: {files}"
            executable = files[0]
            deployed = Path(directory) / "deployed-server"
            with deployed.open("wb") as output:
                subprocess.run(
                    ["docker", "exec", container, "cat", executable],
                    stdout=output, check=True, timeout=120,
                )
            digest = hashlib.sha256(deployed.read_bytes()).hexdigest()
            assert executable == f"/root/.cache/orbit/servers/{digest}/orbit"
            headers = run("readelf", "-l", "-d", str(deployed)).stdout
            assert "INTERP" not in headers and "NEEDED" not in headers
            assert digest != hashlib.sha256(client.read_bytes()).hexdigest(), "Uploaded native client"
            deployed.unlink()
            before = run("docker", "exec", container, "stat", "-c", "%i:%Y", executable).stdout
            run(*arguments, env=environment, cwd=directory)
            after = run("docker", "exec", container, "stat", "-c", "%i:%Y", executable).stdout
            assert before == after, "Cached server was replaced instead of reused"
            files = run("docker", "exec", container, "find", "/root/.cache/orbit/servers", "-type", "f").stdout.splitlines()
            assert files == [executable], f"Unexpected deployed binaries: {files}"
            print("PASS: standalone client without companions; static deployment, handshake and cache reuse on Ubuntu 20.04")
    finally:
        run("docker", "rm", "--force", container)


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        print(error.stderr, file=sys.stderr)
        raise
