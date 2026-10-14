"""Synthetic stdio peer for transport lifecycle tests; no external services."""

import json
import pathlib
import subprocess
import sys
import time

mode = sys.argv[1]
root = pathlib.Path.cwd()
starts = root / "starts"
generation = int(starts.read_text()) + 1 if starts.exists() else 1
starts.write_text(str(generation))
calls = 0


def send(value):
    print(json.dumps({"jsonrpc": "2.0", **value}), flush=True)


for line in sys.stdin:
    message = json.loads(line)
    with (root / "requests.jsonl").open("a") as log:
        log.write(json.dumps(message) + "\n")
    method = message.get("method")
    request_id = message.get("id")
    if method == "initialize":
        if mode != "init_timeout":
            send({"id": request_id, "result": {"capabilities": {"tools": {}}}})
    elif method == "tools/list":
        schema = {"type": "object"}
        if mode == "changed" and generation > 1:
            schema["required"] = ["new_field"]
        send({"id": request_id, "result": {"tools": [
            {"name": "work", "inputSchema": schema}
        ]}})
        if mode == "blocked_write":
            time.sleep(60)
    elif method == "tools/call":
        calls += 1
        if mode == "rpc_error" and calls == 1:
            send({"id": request_id, "error": {"code": -32000, "message": "busy"}})
        elif mode == "tool_error" and calls == 1:
            send({"id": request_id, "result": {"isError": True, "content": []}})
        elif mode in ("timeout", "partial", "changed", "malformed", "exit") and generation == 1:
            child = subprocess.Popen(["/bin/sleep", "60"], stdout=subprocess.DEVNULL)
            (root / "child.pid").write_text(str(child.pid))
            if mode == "partial":
                sys.stdout.write('{"jsonrpc":"2.0","id":')
                sys.stdout.flush()
            elif mode == "malformed":
                print("invalid-json", flush=True)
            elif mode == "exit":
                sys.exit(0)
        elif mode == "collision":
            send({"id": request_id, "method": "roots/list"})
            roots = json.loads(next(sys.stdin))
            assert "roots" in roots["result"]
            send({"id": request_id, "result": {"content": []}})
        else:
            send({"id": request_id, "result": {"content": []}})
if (root / "child.pid").exists() and generation == 1:
    # Simulate a server whose shutdown handler cannot finish.
    time.sleep(60)
