#!/usr/bin/env python3
"""Run the official MCP conformance runner against applicable V1 behavior.

This gateway intentionally advertises only tools. The upstream full requirement
sets also contain fixture-specific prompts/resources/tool-content scenarios, so
this integration does not pretend to be a full everything-server certification.
It does two things in CI:

1. asks the pinned official runner to load both frozen requirement revisions;
2. runs official server scenarios that apply directly to this gateway boundary.
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import tempfile
import time
import urllib.request
from pathlib import Path
from typing import TextIO

HOST = "127.0.0.1"
PORT = 18121
BASE_URL = f"http://{HOST}:{PORT}"
MCP_URL = f"{BASE_URL}/mcp"
HEALTH_URL = f"{BASE_URL}/healthz"
CONFORMANCE_PACKAGE = "@modelcontextprotocol/conformance@0.2.0-alpha.11"
SCENARIOS = ("server-initialize", "tools-list", "dns-rebinding-protection")


def gateway_binary() -> Path:
    path = Path("target/debug/v1_gateway")
    if not path.exists():
        raise RuntimeError(f"gateway binary missing: {path}; run cargo build --locked --bin v1_gateway first")
    return path.resolve()


def read_log(log_file: TextIO) -> str:
    log_file.flush()
    pos = log_file.tell()
    log_file.seek(0)
    value = log_file.read()
    log_file.seek(pos)
    return value


def wait_ready(proc: subprocess.Popen[str], log_file: TextIO) -> None:
    deadline = time.time() + 30.0
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(
                f"gateway exited before readiness ({proc.returncode})\n{read_log(log_file)}"
            )
        try:
            with urllib.request.urlopen(HEALTH_URL, timeout=2) as response:
                health = json.loads(response.read().decode("utf-8"))
            if health.get("status") == "ok" and health.get("backend") == "ready":
                return
        except Exception:
            pass
        time.sleep(0.1)
    raise RuntimeError(f"gateway did not become ready\n{read_log(log_file)}")


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


def conformance(*args: str) -> None:
    command = ["npx", "--yes", CONFORMANCE_PACKAGE, *args]
    print("+", " ".join(command))
    subprocess.run(command, check=True)


def main() -> None:
    node_major = int(
        subprocess.check_output(
            ["node", "-p", "process.versions.node.split('.')[0]"], text=True
        ).strip()
    )
    if node_major < 22:
        raise RuntimeError(f"official conformance runner requires Node 22+; found {node_major}")

    # Exercise the upstream frozen requirement-set parser for both protocol lines
    # tracked by this project. This is deliberately separate from certification.
    conformance("list", "--requirements", "2025-11-25")
    conformance("list", "--requirements", "2026-07-28")

    proc, log_file = start_gateway()
    try:
        for scenario in SCENARIOS:
            conformance("server", "--url", MCP_URL, "--scenario", scenario)
    except Exception:
        print(read_log(log_file))
        raise
    finally:
        stop_gateway(proc)
        log_file.close()


if __name__ == "__main__":
    main()
