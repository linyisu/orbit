"""Offline workspace peer for control-channel tests, independent of model discovery."""

import json
import struct
import sys

source = sys.stdin.buffer
target = sys.stdout.buffer
target.write(b"\0ORBIT-WORKSPACE/1\n")
target.flush()
while header := source.read(4):
    request = json.loads(source.read(struct.unpack(">I", header)[0]))
    method = request["method"]
    if method == "hello":
        response = {
            "type": "hello", "value": {
                "version": request["version"],
                "workspace": ".",
                "capabilities": {
                    "system": "Linux", "architecture": "x86_64", "shell": "sh",
                    "native_processes": True, "files": True, "features": [],
                },
            },
        }
    elif method == "all_sessions":
        response = {"type": "sessions", "value": []}
    elif method == "open":
        response = {"type": "opened", "value": {
            "id": "synthetic-session", "title": "Generated observation", "workspace": ".",
            "model": "slow::declared-model", "models": ["slow::declared-model"],
            "supports_vision": False, "reasoning_effort": None, "reasoning_efforts": [],
            "context_tokens": 32000, "input_context_tokens": 28000, "max_output_tokens": 4000,
        }}
    elif method == "poll":
        response = {"type": "page", "value": {
            "next": 0, "more": False, "busy": True, "model": "slow::declared-model",
            "updates": [], "approvals": [],
        }}
    elif method == "configure_models":
        assert request["session"] == "synthetic-session"
        assert "refreshed-model" in request["configuration"]["content"]
        response = {"type": "ok"}
    else:
        response = {"type": "ok"}
    payload = json.dumps(response).encode()
    target.write(struct.pack(">I", len(payload)) + payload)
    target.flush()
