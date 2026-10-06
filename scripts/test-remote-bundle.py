#!/usr/bin/env python3
"""Exercise automatic deployment and cache reuse on older Linux through an SSH stand-in."""

import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import ssl
import struct
import subprocess
import sys
import tempfile
import threading


SYSTEM_CA_BUNDLES = (
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/ssl/cert.pem",
)


def system_ca_bundle():
    candidates = (
        os.environ.get("SSL_CERT_FILE"),
        ssl.get_default_verify_paths().cafile,
        *SYSTEM_CA_BUNDLES,
    )
    for candidate in candidates:
        if candidate and Path(candidate).is_file() and Path(candidate).stat().st_size:
            return Path(candidate).resolve()
    checked = ", ".join(str(path) for path in candidates if path)
    raise RuntimeError(
        f"No system CA bundle found (checked: {checked}); "
        "install ca-certificates or set SSL_CERT_FILE to a PEM bundle"
    )


def run(*arguments, **kwargs):
    return subprocess.run(arguments, check=True, capture_output=True, text=True, timeout=120, **kwargs)


def open_unconfigured_workspace(container, executable):
    process = subprocess.Popen(
        ["docker", "exec", "-i", container, executable, "workspace-service", "--workspace", "/tmp",
         "--database", "/tmp/configuration-test.db", "--auth-dir", "/tmp/configuration-test-auth"],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    deadline = threading.Timer(45, process.kill)
    deadline.start()
    try:
        assert process.stdout.read(len(b"\0ORBIT-WORKSPACE/1\n")) == b"\0ORBIT-WORKSPACE/1\n"

        def request(value):
            payload = json.dumps(value).encode()
            process.stdin.write(struct.pack(">I", len(payload)) + payload)
            process.stdin.flush()
            size = struct.unpack(">I", process.stdout.read(4))[0]
            assert size <= 32 * 1024 * 1024
            return json.loads(process.stdout.read(size))

        protocol = Path(__file__).resolve().parents[1] / "crates/orbit-protocol/src/lib.rs"
        version = int(re.search(r"pub const VERSION: u32 = (\d+);", protocol.read_text()).group(1))
        assert request({"method": "hello", "version": version})["type"] == "hello"
        config = """[providers.test]
base_url = "http://127.0.0.1:9/v1"
api_key = "test-only"
auto_refresh_models = false
models = [{ id = "local-model" }]
"""
        response = request({"method": "open", "resume": None, "model": "test::local-model", "configuration": {"content": config, "credentials": {}}})
        assert response["type"] == "opened", response
        assert response["value"]["model"] == "test::local-model"
        assert request({"method": "shutdown"})["type"] == "ok"
        run("docker", "exec", container, "test", "!", "-e", "/tmp/config.toml")
    finally:
        deadline.cancel()
        process.stdin.close()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=10)


def main():
    bundle = Path(sys.argv[1] if len(sys.argv) > 1 else "target/bundle").resolve()
    certificates = system_ca_bundle()
    container = run(
        "docker", "run", "--detach", "--rm", "--network", "none", "--read-only",
        "--tmpfs", "/tmp", "--tmpfs", "/root:exec",
        "--mount", f"type=bind,source={certificates},target=/etc/ssl/certs/ca-certificates.crt,readonly",
        "--env", "SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt",
        "ubuntu:20.04", "sleep", "300",
    ).stdout.strip()
    try:
        with tempfile.TemporaryDirectory(prefix="orbit-bundle-test-") as directory:
            client = Path(directory) / "orbit-cli"
            shutil.copy2(bundle / "orbit-cli", client)
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
            open_unconfigured_workspace(container, executable)
            print("PASS: standalone client without companions; static deployment, local configuration, session opening and cache reuse on Ubuntu 20.04")
    finally:
        run("docker", "rm", "--force", container)


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        print(error.stderr, file=sys.stderr)
        raise
