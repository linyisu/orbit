#!/usr/bin/env python3
"""Validate the workspace's dependency direction and execution ownership."""

from pathlib import Path
import re
import sys
import tomllib


ROOT = Path(__file__).resolve().parents[1]
DEPENDENCIES = {
    "orbit-auth": set(),
    "orbit-protocol": set(),
    "orbit-config": {"orbit-agent", "orbit-auth", "orbit-model", "orbit-runtime", "orbit-tools"},
    "orbit-client": {"orbit-agent", "orbit-auth", "orbit-model", "orbit-tools", "orbit-config", "orbit-protocol"},
    "orbit-model": {"orbit-auth"},
    "orbit-runtime": set(),
    "orbit-agent": {"orbit-model", "orbit-runtime"},
    "orbit-tools": {"orbit-agent", "orbit-model", "orbit-runtime"},
    "orbit-store": {"orbit-agent", "orbit-model", "orbit-runtime", "orbit-protocol"},
    "orbit-service": {
        "orbit-agent", "orbit-auth", "orbit-model", "orbit-runtime",
        "orbit-store", "orbit-tools", "orbit-config", "orbit-protocol",
    },
    "orbit-tui": {
        "orbit-service", "orbit-agent", "orbit-auth", "orbit-model",
        "orbit-runtime", "orbit-store", "orbit-tools", "orbit-client", "orbit-protocol",
    },
    "orbit-cli": {"orbit-protocol", "orbit-service", "orbit-client", "orbit-agent", "orbit-auth", "orbit-model", "orbit-store"},
    "orbit-server": {"orbit-service"},
}
PRESENTATION = {"arboard", "clap", "crossterm", "ratatui", "ratatui-image", "syntect"}
EXECUTION = re.compile(r"\b(?:Agent::new|run_with_agent_events|compact_configured|workspace_tools)\b")


def check() -> list[str]:
    errors = []
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())
    packages = set()
    for member in workspace["workspace"]["members"]:
        manifest = tomllib.loads((ROOT / member / "Cargo.toml").read_text())
        name = manifest["package"]["name"]
        packages.add(name)
        dependencies = set(manifest.get("dependencies", {})) | set(manifest.get("build-dependencies", {}))
        allowed = DEPENDENCIES.get(name)
        if allowed is None:
            errors.append(f"{name}: define its dependency boundary")
            continue
        unexpected = {dep for dep in dependencies if dep.startswith("orbit-")} - allowed
        if unexpected:
            errors.append(f"{name}: unexpected application dependencies {sorted(unexpected)}")
        if name == "orbit-service" and dependencies & PRESENTATION:
            errors.append(f"{name}: presentation dependencies {sorted(dependencies & PRESENTATION)}")

    for missing in DEPENDENCIES.keys() - packages:
        errors.append(f"{missing}: workspace member missing")
    for package in ["orbit-cli", "orbit-tui"]:
        for source in (ROOT / "crates" / package / "src").rglob("*.rs"):
            if EXECUTION.search(source.read_text()):
                errors.append(f"{source.relative_to(ROOT)}: execution belongs to orbit-service")
    for source in (ROOT / "crates/orbit-service/src").rglob("*.rs"):
        if re.search(r"\b(?:orbit_tui|crate::tui)::", source.read_text()):
            errors.append(f"{source.relative_to(ROOT)}: application depends on terminal presentation")
    return errors


if __name__ == "__main__":
    failures = check()
    if failures:
        print("\n".join(failures), file=sys.stderr)
        sys.exit(1)
    print("Workspace architecture checks passed.")
