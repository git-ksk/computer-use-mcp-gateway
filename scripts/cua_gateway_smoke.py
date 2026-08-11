#!/usr/bin/env python3
"""Cross-platform smoke test for a real cua-driver MCP backend.

This test intentionally avoids GUI actions so it can run on GitHub-hosted
Linux, macOS, and Windows runners without desktop/TCC grants. It verifies the
real integration boundary and both supported MCP lifecycles:

  gateway -> cua-driver mcp -> MCP discovery/tools/list/policy

Set MCP_PROTOCOL_VERSION to 2025-11-25 for the legacy initialize lifecycle or
2026-07-28 for the stateless per-request metadata lifecycle.
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
PROTOCOL_VERSION = os.environ.get("MCP_PROTOCOL_VERSION", "2025-11-25")
MODERN_PROTOCOL = PROTOCOL_VERSION == "2026-07-28"
CLIENT_INFO = {"name": "cua-gateway-ci", "version": "0.1.0"}
PINNED_CUA_TOOL_FIXTURE = Path("tests/fixtures/cua-0.19.3-tools.txt")
PINNED_CUA_LINUX_EXTRA_FIXTURE = Path("tests/fixtures/cua-0.19.3-tools-linux-extra.txt")
PINNED_CUA_WINDOWS_EXTRA_FIXTURE = Path("tests/fixtures/cua-0.19.3-tools-windows-extra.txt")


def _fixture_names(path: Path) -> set[str]:
    return {
        line.strip()
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    }


def pinned_cua_tool_names() -> set[str]:
    names = _fixture_names(PINNED_CUA_TOOL_FIXTURE)
    if len(names) != 54:
        raise RuntimeError(
            f"unexpected common Cua tool fixture size: {len(names)} (expected 54)"
        )

    if sys.platform.startswith("linux"):
        extra = _fixture_names(PINNED_CUA_LINUX_EXTRA_FIXTURE)
        if len(extra) != 4:
            raise RuntimeError(
                f"unexpected Linux Cua tool fixture size: {len(extra)} (expected 4)"
            )
        names |= extra
    elif os.name == "nt":
        extra = _fixture_names(PINNED_CUA_WINDOWS_EXTRA_FIXTURE)
        if len(extra) != 1:
            raise RuntimeError(
                f"unexpected Windows Cua tool fixture size: {len(extra)} (expected 1)"
            )
        names |= extra

    return names


def assert_pinned_tool_surface(names: list[str]) -> None:
    expected = pinned_cua_tool_names()
    actual = set(names)
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    if missing or extra:
        raise RuntimeError(
            "pinned Cua tool surface drifted; review semantic classification before "
            f"updating the fixture: missing={missing} extra={extra}"
        )


def gateway_binary() -> Path:
    suffix = ".exe" if os.name == "nt" else ""
    path = Path("target") / "debug" / f"computer-use-mcp-gateway{suffix}"
    if not path.exists():
        raise RuntimeError(f"gateway binary missing: {path}")
    return path.resolve()


def backend_args() -> str:
    return "mcp --direct" if sys.platform == "darwin" else "mcp"


def request_meta() -> dict:
    return {
        "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientInfo": CLIENT_INFO,
        "io.modelcontextprotocol/clientCapabilities": {},
    }


def build_rpc(method: str, params: dict | None, request_id: int) -> tuple[dict, dict]:
    actual_params = dict(params or {})
    if MODERN_PROTOCOL:
        actual_params["_meta"] = request_meta()

    payload = {
        "jsonrpc": "2.0",
        "id": request_id,
        "method": method,
        "params": actual_params,
    }
    headers = {
        "Accept": "application/json, text/event-stream",
        "Content-Type": "application/json",
        "MCP-Protocol-Version": PROTOCOL_VERSION,
    }
    if MODERN_PROTOCOL:
        headers["Mcp-Method"] = method
        if method == "tools/call":
            name = actual_params.get("name")
            if isinstance(name, str):
                headers["Mcp-Name"] = name
    return payload, headers


def decode_http_response(resp) -> dict:
    body = resp.read().decode("utf-8")
    content_type = resp.headers.get("Content-Type", "")
    if "application/json" in content_type:
        return json.loads(body)
    for line in body.splitlines():
        if line.startswith("data:"):
            return json.loads(line.removeprefix("data:").strip())
    raise RuntimeError(f"unexpected HTTP response: {body[:500]}")


def http_json(
    url: str,
    payload: dict | None = None,
    timeout: float = 10.0,
    headers: dict | None = None,
) -> dict:
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    req_headers = dict(headers or {"Accept": "application/json"})
    if data is not None:
        req_headers.setdefault("Content-Type", "application/json")
    req = urllib.request.Request(url, data=data, headers=req_headers)
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return decode_http_response(resp)


def rpc(method: str, params: dict | None = None, request_id: int = 1) -> dict:
    payload, headers = build_rpc(method, params, request_id)
    response = http_json(MCP_URL, payload, headers=headers)
    if "error" in response:
        raise RuntimeError(f"MCP {method} failed: {response['error']}")
    if "result" not in response:
        raise RuntimeError(f"MCP {method} returned no result: {response}")
    return response["result"]


def establish_protocol() -> None:
    if MODERN_PROTOCOL:
        result = rpc("server/discover", {}, request_id=1)
        supported = result.get("supportedVersions", [])
        if PROTOCOL_VERSION not in supported:
            raise RuntimeError(
                f"server/discover did not advertise {PROTOCOL_VERSION}: {supported}"
            )
        return

    result = rpc(
        "initialize",
        {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": CLIENT_INFO,
        },
        request_id=1,
    )
    negotiated = result.get("protocolVersion")
    if negotiated != PROTOCOL_VERSION:
        raise RuntimeError(
            f"unexpected MCP protocol negotiation: wanted {PROTOCOL_VERSION}, got {negotiated}"
        )


def list_tools() -> list[dict]:
    result = rpc("tools/list", {}, request_id=2)
    tools = result.get("tools")
    if not isinstance(tools, list):
        raise RuntimeError(f"tools/list returned invalid tools field: {result}")
    return tools


def assert_transport_guards() -> None:
    method = "server/discover" if MODERN_PROTOCOL else "initialize"
    params = (
        {}
        if MODERN_PROTOCOL
        else {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": CLIENT_INFO,
        }
    )
    payload, headers = build_rpc(method, params, request_id=90)

    for name, bad_headers in (
        ("Origin", {**headers, "Origin": "https://evil.example"}),
        ("Host", {**headers, "Host": "evil.example"}),
    ):
        try:
            http_json(MCP_URL, payload, timeout=5.0, headers=bad_headers)
        except urllib.error.HTTPError as exc:
            if exc.code < 400:
                raise RuntimeError(f"{name} guard returned unexpected status {exc.code}")
        else:
            raise RuntimeError(f"malicious {name} was accepted by the MCP endpoint")


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
            # CI explicitly opts in to the entire real Cua tool surface. The
            # product default is deny-all.
            "CUMG_ALLOW_TOOLS": "*",
            "CUMG_CONNECT_TIMEOUT_SECS": "15",
            "CUMG_TOOL_TIMEOUT_SECS": "30",
            "CUMG_RECONNECT_ATTEMPTS": "3",
            "CUMG_RECONNECT_BACKOFF_MS": "100",
            # Keep gateway audit logs while suppressing verbose rmcp peer dumps.
            "RUST_LOG": "warn,computer_use_mcp_gateway=info",
        }
    )
    if deny_tool:
        env["CUMG_DENY_TOOLS"] = deny_tool
    else:
        env.pop("CUMG_DENY_TOOLS", None)

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
        " ".join(
            [
                f"platform={sys.platform}",
                f"python={sys.version.split()[0]}",
                f"backend_args={backend_args()}",
                f"protocol={PROTOCOL_VERSION}",
            ]
        )
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
        establish_protocol()
        assert_transport_guards()
        tools = list_tools()
        if not tools:
            raise RuntimeError("real Cua backend returned zero MCP tools")
        names = [tool.get("name") for tool in tools if isinstance(tool.get("name"), str)]
        if not names:
            raise RuntimeError("real Cua backend returned no named MCP tools")
        assert_pinned_tool_surface(names)
        denied = names[0]
        print(f"discovered_tools={len(names)} policy_probe={denied}")
    finally:
        stop_gateway(first, first_log)

    second, second_log = start_gateway(deny_tool=denied)
    try:
        establish_protocol()
        filtered_names = [
            tool.get("name")
            for tool in list_tools()
            if isinstance(tool.get("name"), str)
        ]
        if denied in filtered_names:
            raise RuntimeError(f"deny policy leaked tool through tools/list: {denied}")
        assert_blocked(denied)
        print(
            "PASS real Cua MCP smoke: "
            f"protocol={PROTOCOL_VERSION} tools={len(names)} "
            f"filtered={len(filtered_names)} denied={denied}"
        )
    finally:
        stop_gateway(second, second_log)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
