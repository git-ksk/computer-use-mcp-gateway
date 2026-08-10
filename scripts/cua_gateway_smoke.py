#!/usr/bin/env python3
"""Cross-platform smoke test for a real cua-driver MCP backend.

The test intentionally avoids GUI actions so it can run on GitHub-hosted
Linux, macOS, and Windows runners without desktop/TCC grants. It verifies the
actual integration boundary:

  gateway -> cua-driver mcp -> MCP initialize/tools/list

On GitHub-hosted macOS the smoke test uses `cua-driver mcp --direct` so the
fresh runner does not depend on persistent CuaDriver.app TCC grants. Production
macOS remains free to use the gateway default (`cua-driver mcp`), which proxies
through the signed app identity.

The test then restarts the gateway with a dynamic deny rule and confirms that
the chosen backend tool disappears and is blocked before reaching Cua.
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import TextIO

HOST = "127.0.0.1"
PORT = 18100
BASE_URL = f"http://{HOST}:{PORT}"
MCP_URL = f"{BASE_URL}/mcp"
HEALTH_URL = f"{BASE_URL}/healthz"
PROTOCOL_VERSION = "2025-11-25"


def gateway_binary() -> Path:
    suffix = ".exe" if os.name == "nt" else ""
    path = Path("target") / "debug" / f"computer-use-mcp-gateway{suffix}"
    if not path.exists():
        raise RuntimeError(f"gateway binary missing: {path}")
    return path.resolve()


def backend_args() -> str:
    return "mcp --direct" if sys.platform == "darwin" else "mcp"


def http_json(url: str, payload: dict | None = None, timeout: float = 10.0) -> dict:
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    headers = {
        "Accept": "application/json, text/event-stream",
        "MCP-Protocol-Version": PROTOCOL_VERSION,
    }
    if data is not None:
        headers["Content-Type"] = "application/json"

    req = urllib.request.Request(url, data=data, headers=headers)
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        body = resp.read().decode("utf-8")
        content_type = resp.headers.get("Content-Type", "")

    if "application/json" in content_type:
        return json.loads(body)

    for line in body.splitlines():
        if line.startswith("data:"):
            return json.loads(line.removeprefix("data:").strip())
    raise RuntimeError(f"unexpected HTTP response from {url}: {body[:500]}")


def rpc(method: str, params: dict | None = None, request_id: int = 1) -> dict:
    payload = {"jsonrpc": "2.0", "id": request_id, "method": method}
    if params is not None:
        payload["params"] = params
    response = http_json(MCP_URL, payload)
    if "error" in response:
        raise RuntimeError(f"MCP {method} failed: {response['error']}")
    if "result" not in response:
        raise RuntimeError(f"MCP {method} returned no result: {response}")
    return response["result"]


def initialize() -> dict:
    return rpc(
        "initialize",
        {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "cua-gateway-ci", "version": "0.1.0"},
        },
    )


def list_tools() -> list[dict]:
    result = rpc("tools/list", {}, request_id=2)
    tools = result.get("tools")
    if not isinstance(tools, list):
        raise RuntimeError(f"tools/list returned invalid tools field: {result}")
    return tools


def read_gateway_log(log_file: TextIO) -> str:
    log_file.flush()
    current = log_file.tell()
    log_file.seek(0)
    output = log_file.read()
    log_file.seek(current)
    return output


def wait_ready(
    proc: subprocess.Popen[str], log_file: TextIO, timeout: float = 60.0
) -> None:
    deadline = time.time() + timeout
    last_error: Exception | None = None
    while time.time() < deadline:
        if proc.poll() is not None:
            output = read_gateway_log(log_file)
            raise RuntimeError(
                f"gateway exited before readiness with code {proc.returncode}\n{output}"
            )
        try:
            health = http_json(HEALTH_URL, timeout=2.0)
            if health.get("status") == "ok" and health.get("backend") == "ready":
                return
        except (urllib.error.URLError, TimeoutError, ConnectionError, json.JSONDecodeError) as exc:
            last_error = exc
        time.sleep(0.25)
    raise RuntimeError(f"gateway did not become ready: {last_error}")


def start_gateway(deny_tool: str | None = None) -> tuple[subprocess.Popen[str], TextIO]:
    env = os.environ.copy()
    env.update(
        {
            "CUMG_BIND": f"{HOST}:{PORT}",
            "CUMG_MCP_PATH": "/mcp",
            "CUMG_BACKEND_COMMAND": "cua-driver",
            "CUMG_BACKEND_ARGS": backend_args(),
            # Keep gateway audit logs while suppressing verbose rmcp peer dumps.
            "RUST_LOG": "warn,computer_use_mcp_gateway=info",
        }
    )
    if deny_tool:
        env["CUMG_DENY_TOOLS"] = deny_tool
    else:
        env.pop("CUMG_DENY_TOOLS", None)

    # Do not capture child output with PIPE here. Windows anonymous pipes have a
    # small buffer and verbose protocol logs can block the gateway before the
    # test drains stdout. A temporary file preserves diagnostics without
    # back-pressuring the process.
    log_file = tempfile.TemporaryFile(mode="w+", encoding="utf-8")
    proc = subprocess.Popen(
        [str(gateway_binary())],
        env=env,
        stdout=log_file,
        stderr=subprocess.STDOUT,
        text=True,
    )
    wait_ready(proc, log_file)
    return proc, log_file


def stop_gateway(proc: subprocess.Popen[str], log_file: TextIO) -> None:
    try:
        if proc.poll() is None:
            if os.name == "nt":
                proc.terminate()
            else:
                proc.send_signal(signal.SIGINT)
            try:
                proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=5)
    finally:
        output = read_gateway_log(log_file)
        if output:
            print("--- gateway log ---")
            print(output[-8000:])
        log_file.close()


def assert_blocked(tool_name: str) -> None:
    result = rpc(
        "tools/call",
        {"name": tool_name, "arguments": {}},
        request_id=3,
    )
    if result.get("isError") is not True:
        raise RuntimeError(f"denied tool was not blocked: {tool_name}: {result}")


def main() -> int:
    print(
        f"platform={sys.platform} python={sys.version.split()[0]} backend_args={backend_args()}"
    )
    version = subprocess.run(
        ["cua-driver", "--version"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    print(f"backend={version}")

    first, first_log = start_gateway()
    try:
        init = initialize()
        negotiated = init.get("protocolVersion")
        if negotiated != PROTOCOL_VERSION:
            raise RuntimeError(
                f"unexpected MCP protocol negotiation: wanted {PROTOCOL_VERSION}, got {negotiated}"
            )
        tools = list_tools()
        if not tools:
            raise RuntimeError("real Cua backend returned zero MCP tools")
        names = [tool.get("name") for tool in tools if isinstance(tool.get("name"), str)]
        if not names:
            raise RuntimeError("real Cua backend returned no named MCP tools")
        denied = names[0]
        print(f"discovered_tools={len(names)} policy_probe={denied}")
    finally:
        stop_gateway(first, first_log)

    second, second_log = start_gateway(deny_tool=denied)
    try:
        initialize()
        filtered_names = [
            tool.get("name")
            for tool in list_tools()
            if isinstance(tool.get("name"), str)
        ]
        if denied in filtered_names:
            raise RuntimeError(f"deny policy leaked tool through tools/list: {denied}")
        assert_blocked(denied)
        print(
            f"PASS real Cua MCP smoke: tools={len(names)} filtered={len(filtered_names)} denied={denied}"
        )
    finally:
        stop_gateway(second, second_log)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
