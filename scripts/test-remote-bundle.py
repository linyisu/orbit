#!/usr/bin/env python3
"""Exercise automatic deployment and cache reuse on older Linux through an SSH stand-in."""

import hashlib
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile


def run(*arguments, **kwargs):
    return subprocess.run(arguments, check=True, capture_output=True, text=True, timeout=120, **kwargs)


def main():
    bundle = Path(sys.argv[1] if len(sys.argv) > 1 else "target/bundle").resolve()
    server = bundle / f"orbit-server-{platform.machine()}-linux-musl"
    checksum = hashlib.sha256()
    with server.open("rb") as binary:
        for chunk in iter(lambda: binary.read(65536), b""):
            checksum.update(chunk)
    digest = checksum.hexdigest()
    container = run(
        "docker", "run", "--detach", "--rm", "--network", "none", "--read-only",
        "--tmpfs", "/tmp", "--tmpfs", "/root:exec", "ubuntu:20.04", "sleep", "300",
    ).stdout.strip()
    try:
        with tempfile.TemporaryDirectory(prefix="orbit-bundle-test-") as directory:
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
                str(bundle / "orbit"), "connect", "--ssh", "isolated-test", "--workspace", "/tmp",
                "--database", "/tmp/sessions.db", "--auth-dir", "/tmp/auth", "--list",
            )
            run(*arguments, env=environment)
            executable = f"/root/.cache/orbit/servers/{digest}/orbit"
            before = run("docker", "exec", container, "stat", "-c", "%i:%Y", executable).stdout
            run(*arguments, env=environment)
            after = run("docker", "exec", container, "stat", "-c", "%i:%Y", executable).stdout
            assert before == after, "Cached server was replaced instead of reused"
            files = run("docker", "exec", container, "find", "/root/.cache/orbit/servers", "-type", "f").stdout.splitlines()
            assert files == [executable], f"Unexpected deployed binaries: {files}"
            print("PASS: automatic portable deployment, service handshake and cache reuse on Ubuntu 20.04")
    finally:
        run("docker", "rm", "--force", container)


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        print(error.stderr, file=sys.stderr)
        raise
