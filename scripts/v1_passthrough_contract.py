#!/usr/bin/env python3
"""Regression contract for transparent Gateway <-> backend tool forwarding.

This fixture proves two V1 invariants relevant to Cua session/discovery behavior:
1. tool arguments, including a `session` field, reach the backend unchanged;
2. backend result payloads are returned unchanged, even when application identity
   fields are internally inconsistent.

The gateway must not silently remove session semantics or invent PID/running-state
normalization. Backend-specific semantics belong to the backend/adapter contract.
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

HOST = "127.0.0.1"
PORT = 18121
BASE_URL = f"http://{HOST}:{PORT}"
MCP_URL = f"{BASE_URL}/mcp"
HEALTH_URL = f"{BASE_URL}/healthz"
PROTOCOL_VERSION = "2026-07-28"
CLIENT_INFO = {"name": "cumg-v1-passthrough", "version": "1.0.0"}


def gateway_binary() -> Path:
    path = Path("target/debug/computer-use-mcp-gateway")
    if not path.exists():
        raise RuntimeError(f"gateway binary missing: {path}; run cargo build --locked first")
    return path.resolve()


def request_meta() -> dict:
    return {
        "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientInfo": CLIENT_INFO,
        "io.modelcontextprotocol/clientCapabilities": {},
    }


def rpc(method: str, params: dict | None, request_id: int) -> dict:
    actual_params = dict(params or {})
    actual_params["_meta"] = request_meta()
    payload = {"jsonrpc": "2.0", "id": request_id, "method": method, "params": actual_params}
    headers = {
        "Accept": "application/json, text/event-stream",
        "Content-Type": "application/json",
        "MCP-Protocol-Version": PROTOCOL_VERSION,
        "Mcp-Method": method,
    }
    if method == "tools/call":
        headers["Mcp-Name"] = str(actual_params.get("name", ""))
    request = urllib.request.Request(
        MCP_URL,
        data=json.dumps(payload).encode("utf-8"),
        headers=headers,
    )
    with urllib.request.urlopen(request, timeout=10) as response:
        body = response.read().decode("utf-8")
        content_type = response.headers.get("Content-Type", "")
    if "application/json" in content_type:
        message = json.loads(body)
    else:
        message = None
        for line in body.splitlines():
            if line.startswith("data:"):
                message = json.loads(line.removeprefix("data:").strip())
                break
        if message is None:
            raise RuntimeError(f"unexpected MCP response: {body[:500]}")
    if "error" in message:
        raise RuntimeError(f"MCP {method} failed: {message['error']}")
    return message["result"]


def wait_ready(proc: subprocess.Popen[str]) -> None:
    deadline = time.time() + 20
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"gateway exited before readiness: {proc.returncode}")
        try:
            with urllib.request.urlopen(HEALTH_URL, timeout=1) as response:
                health = json.loads(response.read().decode("utf-8"))
            if health.get("status") == "ok" and health.get("backend") == "ready":
                return
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError):
            pass
        time.sleep(0.1)
    raise RuntimeError("gateway did not become ready")


def main() -> None:
    with tempfile.TemporaryDirectory() as temp_dir:
        args_marker = Path(temp_dir) / "args.json"
        env = os.environ.copy()
        env.update(
            {
                "CUMG_BIND": f"{HOST}:{PORT}",
                "CUMG_BACKEND_COMMAND": "python3",
                "CUMG_BACKEND_ARGS": f"scripts/mock_mcp_backend.py --args-marker {args_marker}",
                "CUMG_ALLOW_TOOLS": "echo_contract",
                "CUMG_CONNECT_TIMEOUT_SECS": "5",
                "CUMG_TOOL_TIMEOUT_SECS": "10",
                "CUMG_RECONNECT_ATTEMPTS": "1",
                "RUST_LOG": "warn",
            }
        )
        proc = subprocess.Popen(
            [str(gateway_binary())],
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.STDOUT,
            text=True,
        )
        try:
            wait_ready(proc)
            expected_arguments = {
                "session": "gateway-smoke-20260811",
                "observe": True,
                "nested": {"value": 7, "label": "unchanged"},
            }
            result = rpc(
                "tools/call",
                {"name": "echo_contract", "arguments": expected_arguments},
                request_id=1,
            )
            recorded = json.loads(args_marker.read_text(encoding="utf-8"))
            if recorded != expected_arguments:
                raise RuntimeError(
                    f"gateway mutated backend arguments: expected={expected_arguments!r} actual={recorded!r}"
                )

            expected_structured = {
                "application": {"name": "Cua Driver", "running": False, "pid": 0},
                "window": {"application": "Cua Driver", "pid": 31438},
            }
            if result.get("structuredContent") != expected_structured:
                raise RuntimeError(
                    "gateway normalized or mutated backend identity payload: "
                    f"{result.get('structuredContent')!r}"
                )
            print("backend passthrough contract PASS")
        finally:
            if proc.poll() is None:
                proc.send_signal(signal.SIGINT)
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)


if __name__ == "__main__":
    main()
