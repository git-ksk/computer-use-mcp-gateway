#!/usr/bin/env python3
"""V1 deterministic quality gate: 100 tool calls plus idle CPU/RSS.

Runs the real gateway binary against scripts/mock_mcp_backend.py so the test
covers the northbound Streamable HTTP server, gateway policy, backend MCP stdio
adapter, serialization, response forwarding, and backend health metrics without
touching a desktop.

Resource measurement is intentionally Linux-only. The idle regression gate
measures the gateway PID, while `/healthz` independently reports the owned
backend child process CPU time and RSS. Thresholds are generous regression
guards rather than marketing performance claims.
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
PORT = 18120
BASE_URL = f"http://{HOST}:{PORT}"
MCP_URL = f"{BASE_URL}/mcp"
HEALTH_URL = f"{BASE_URL}/healthz"
PROTOCOL_VERSION = "2026-07-28"
CLIENT_INFO = {"name": "cumg-v1-quality", "version": "1.0.0"}
SOAK_CALLS = 100
IDLE_SECONDS = 5.0
MAX_IDLE_CPU_PERCENT = 2.0
MAX_IDLE_RSS_MIB = 128.0


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


def rpc(method: str, params: dict | None, request_id: int, timeout: float = 10.0) -> dict:
    actual_params = dict(params or {})
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
        "Mcp-Method": method,
    }
    if method == "tools/call":
        headers["Mcp-Name"] = str(actual_params.get("name", ""))

    request = urllib.request.Request(
        MCP_URL,
        data=json.dumps(payload).encode("utf-8"),
        headers=headers,
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
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


def get_health(timeout: float = 2.0) -> dict:
    with urllib.request.urlopen(HEALTH_URL, timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


def is_ready(health: dict) -> bool:
    return health.get("status") == "ok" and health.get("backend") == "ready"


def assert_backend_metrics(health: dict) -> None:
    resources = health.get("backend_resources")
    if not isinstance(resources, dict):
        raise RuntimeError(f"health response is missing backend_resources: {health}")
    if not isinstance(resources.get("pid"), int) or resources["pid"] <= 0:
        raise RuntimeError(f"invalid backend PID metric: {resources}")
    cpu_seconds = resources.get("cpu_seconds")
    if not isinstance(cpu_seconds, (int, float)) or cpu_seconds < 0:
        raise RuntimeError(f"invalid backend CPU metric: {resources}")
    rss_bytes = resources.get("rss_bytes")
    if not isinstance(rss_bytes, int) or rss_bytes <= 0:
        raise RuntimeError(f"invalid backend RSS metric: {resources}")
    print(
        "backend health metrics PASS: "
        f"pid={resources['pid']} cpu_seconds={cpu_seconds:.3f} rss_bytes={rss_bytes}"
    )


def read_log(log_file: TextIO) -> str:
    log_file.flush()
    pos = log_file.tell()
    log_file.seek(0)
    value = log_file.read()
    log_file.seek(pos)
    return value


def wait_ready(proc: subprocess.Popen[str], log_file: TextIO) -> None:
    deadline = time.time() + 30.0
    last_error: Exception | None = None
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(
                f"gateway exited before readiness ({proc.returncode})\n{read_log(log_file)}"
            )
        try:
            health = get_health()
            if is_ready(health):
                return
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as exc:
            last_error = exc
        time.sleep(0.1)
    raise RuntimeError(f"gateway did not become ready: {last_error}\n{read_log(log_file)}")


def start_gateway() -> tuple[subprocess.Popen[str], TextIO]:
    env = os.environ.copy()
    env.update(
        {
            "CUMG_BIND": f"{HOST}:{PORT}",
            "CUMG_BACKEND_COMMAND": "python3",
            "CUMG_BACKEND_ARGS": "scripts/mock_mcp_backend.py",
            "CUMG_ALLOW_TOOLS": "noop",
            "CUMG_CONNECT_TIMEOUT_SECS": "5",
            "CUMG_TOOL_TIMEOUT_SECS": "10",
            "CUMG_RECONNECT_ATTEMPTS": "1",
            "RUST_LOG": "warn",
        }
    )
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


def stop_gateway(proc: subprocess.Popen[str]) -> None:
    if proc.poll() is not None:
        return
    proc.send_signal(signal.SIGINT)
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


def run_soak() -> None:
    discover = rpc("server/discover", {}, request_id=1)
    if PROTOCOL_VERSION not in discover.get("supportedVersions", []):
        raise RuntimeError(f"server/discover missing {PROTOCOL_VERSION}: {discover}")

    tools = rpc("tools/list", {}, request_id=2).get("tools", [])
    names = [tool.get("name") for tool in tools]
    if names != ["noop"]:
        raise RuntimeError(f"unexpected exposed tools for quality fixture: {names}")

    started = time.monotonic()
    for index in range(SOAK_CALLS):
        result = rpc(
            "tools/call",
            {"name": "noop", "arguments": {}},
            request_id=1000 + index,
        )
        if result.get("isError"):
            raise RuntimeError(f"soak call {index + 1} returned tool error: {result}")
        content = result.get("content", [])
        if not content or content[0].get("text") != "ok":
            raise RuntimeError(f"soak call {index + 1} returned unexpected content: {result}")

    duration = time.monotonic() - started
    health = get_health()
    if not is_ready(health):
        raise RuntimeError(f"gateway unhealthy after soak: {health}")
    assert_backend_metrics(health)
    print(f"100-call soak PASS: calls={SOAK_CALLS} duration_seconds={duration:.3f}")


def proc_ticks(pid: int) -> int:
    fields = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8").split()
    return int(fields[13]) + int(fields[14])


def proc_rss_mib(pid: int) -> float:
    for line in Path(f"/proc/{pid}/status").read_text(encoding="utf-8").splitlines():
        if line.startswith("VmRSS:"):
            kib = int(line.split()[1])
            return kib / 1024.0
    raise RuntimeError("VmRSS missing from /proc status")


def run_idle_resource_gate(pid: int) -> None:
    if not sys.platform.startswith("linux"):
        raise RuntimeError("idle resource gate currently requires Linux /proc")

    time.sleep(1.0)
    ticks_per_second = os.sysconf("SC_CLK_TCK")
    start_ticks = proc_ticks(pid)
    start = time.monotonic()
    time.sleep(IDLE_SECONDS)
    elapsed = time.monotonic() - start
    end_ticks = proc_ticks(pid)
    cpu_percent = ((end_ticks - start_ticks) / ticks_per_second) / elapsed * 100.0
    rss_mib = proc_rss_mib(pid)

    print(
        "idle resource sample: "
        f"window_seconds={elapsed:.3f} cpu_percent={cpu_percent:.3f} rss_mib={rss_mib:.3f}"
    )
    if cpu_percent > MAX_IDLE_CPU_PERCENT:
        raise RuntimeError(
            f"idle gateway CPU {cpu_percent:.3f}% exceeds {MAX_IDLE_CPU_PERCENT:.1f}% gate"
        )
    if rss_mib > MAX_IDLE_RSS_MIB:
        raise RuntimeError(
            f"idle gateway RSS {rss_mib:.3f} MiB exceeds {MAX_IDLE_RSS_MIB:.1f} MiB gate"
        )
    print(
        "idle resource gate PASS: "
        f"cpu<={MAX_IDLE_CPU_PERCENT:.1f}% rss<={MAX_IDLE_RSS_MIB:.1f}MiB"
    )


def main() -> None:
    proc, log_file = start_gateway()
    try:
        run_soak()
        run_idle_resource_gate(proc.pid)
    except Exception:
        print(read_log(log_file), file=sys.stderr)
        raise
    finally:
        stop_gateway(proc)
        log_file.close()


if __name__ == "__main__":
    main()
