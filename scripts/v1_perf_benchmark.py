#!/usr/bin/env python3
"""Reproducible informational V1 Gateway latency/concurrency benchmark.

Uses the real V1 Gateway with the deterministic mock MCP backend. Results are
local processing/transport measurements for regression analysis, not production
capacity claims and not pass/fail latency thresholds.
"""
from __future__ import annotations

import argparse
import json
import math
import os
import signal
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from typing import TextIO

HOST = "127.0.0.1"
DEFAULT_PORT = 18121
PROTOCOL_VERSION = "2026-07-28"
CLIENT_INFO = {"name": "cumg-v1-perf", "version": "1.0.0"}


def percentile(values_ms: list[float], percentile_value: float) -> float:
    if not values_ms:
        raise ValueError("percentile requires at least one sample")
    if not 0.0 <= percentile_value <= 100.0:
        raise ValueError("percentile must be in 0..100")
    ordered = sorted(values_ms)
    rank = max(1, math.ceil((percentile_value / 100.0) * len(ordered)))
    return ordered[rank - 1]


def summarize(latencies_ms: list[float], elapsed_seconds: float, concurrency: int) -> dict:
    if not latencies_ms or elapsed_seconds <= 0 or concurrency <= 0:
        raise ValueError("invalid benchmark sample")
    return {
        "calls": len(latencies_ms),
        "concurrency": concurrency,
        "elapsed_seconds": elapsed_seconds,
        "throughput_rps": len(latencies_ms) / elapsed_seconds,
        "mean_ms": sum(latencies_ms) / len(latencies_ms),
        "p50_ms": percentile(latencies_ms, 50),
        "p95_ms": percentile(latencies_ms, 95),
        "p99_ms": percentile(latencies_ms, 99),
        "max_ms": max(latencies_ms),
    }


def request_meta() -> dict:
    return {
        "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientInfo": CLIENT_INFO,
        "io.modelcontextprotocol/clientCapabilities": {},
    }


def rpc(base_url: str, method: str, params: dict | None, request_id: int, timeout: float = 10.0) -> dict:
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
        f"{base_url}/mcp",
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


def get_health(base_url: str, timeout: float = 2.0) -> dict:
    with urllib.request.urlopen(f"{base_url}/healthz", timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


def read_log(log_file: TextIO) -> str:
    log_file.flush()
    pos = log_file.tell()
    log_file.seek(0)
    value = log_file.read()
    log_file.seek(pos)
    return value


def wait_ready(proc: subprocess.Popen[str], log_file: TextIO, base_url: str) -> None:
    deadline = time.time() + 30.0
    last_error: Exception | None = None
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"gateway exited before readiness ({proc.returncode})\n{read_log(log_file)}")
        try:
            health = get_health(base_url)
            if health.get("status") == "ok" and health.get("backend") == "ready":
                return
        except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as exc:
            last_error = exc
        time.sleep(0.1)
    raise RuntimeError(f"gateway did not become ready: {last_error}\n{read_log(log_file)}")


def start_gateway(binary: Path, port: int) -> tuple[subprocess.Popen[str], TextIO, str]:
    if not binary.is_file():
        raise RuntimeError(f"gateway binary missing: {binary}; run cargo build --locked --bin v1_gateway first")
    base_url = f"http://{HOST}:{port}"
    env = os.environ.copy()
    env.update({
        "CUMG_BIND": f"{HOST}:{port}",
        "CUMG_BACKEND_COMMAND": "python3",
        "CUMG_BACKEND_ARGS": "scripts/mock_mcp_backend.py",
        "CUMG_ALLOW_TOOLS": "noop",
        "CUMG_CONNECT_TIMEOUT_SECS": "5",
        "CUMG_TOOL_TIMEOUT_SECS": "10",
        "CUMG_RECONNECT_ATTEMPTS": "1",
        "CUMG_HEALTH_DETAILS": "true",
        "RUST_LOG": "warn",
    })
    log_file = tempfile.TemporaryFile(mode="w+", encoding="utf-8")
    proc = subprocess.Popen([str(binary.resolve())], env=env, stdout=log_file, stderr=subprocess.STDOUT, text=True)
    wait_ready(proc, log_file, base_url)
    return proc, log_file, base_url


def stop_gateway(proc: subprocess.Popen[str]) -> None:
    if proc.poll() is not None:
        return
    proc.send_signal(signal.SIGINT)
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=5)


def validate_result(result: dict) -> None:
    if result.get("isError"):
        raise RuntimeError(f"noop returned tool error: {result}")
    content = result.get("content", [])
    if not content or content[0].get("text") != "ok":
        raise RuntimeError(f"noop returned unexpected content: {result}")


def timed_call(base_url: str, request_id: int) -> float:
    start = time.perf_counter_ns()
    result = rpc(base_url, "tools/call", {"name": "noop", "arguments": {}}, request_id)
    elapsed_ms = (time.perf_counter_ns() - start) / 1_000_000.0
    validate_result(result)
    return elapsed_ms


def run_calls(base_url: str, calls: int, concurrency: int, request_id_base: int) -> list[float]:
    if calls <= 0 or concurrency <= 0:
        raise ValueError("calls and concurrency must be positive")
    if concurrency == 1:
        return [timed_call(base_url, request_id_base + i) for i in range(calls)]
    with ThreadPoolExecutor(max_workers=concurrency) as pool:
        return list(pool.map(lambda i: timed_call(base_url, request_id_base + i), range(calls)))


def benchmark_profile(base_url: str, calls: int, warmup: int, concurrency: int, request_id_base: int) -> dict:
    if warmup:
        run_calls(base_url, warmup, concurrency, request_id_base)
    started = time.monotonic()
    latencies = run_calls(base_url, calls, concurrency, request_id_base + warmup + 1)
    elapsed = time.monotonic() - started
    return summarize(latencies, elapsed, concurrency)


def linux_process_resources(pid: int, platform: str | None = None) -> dict | None:
    actual_platform = platform or sys.platform
    if not actual_platform.startswith("linux"):
        return None
    stat_fields = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8").split()
    ticks_per_second = os.sysconf("SC_CLK_TCK")
    cpu_seconds = (int(stat_fields[13]) + int(stat_fields[14])) / ticks_per_second
    rss_bytes = None
    for line in Path(f"/proc/{pid}/status").read_text(encoding="utf-8").splitlines():
        if line.startswith("VmRSS:"):
            rss_bytes = int(line.split()[1]) * 1024
            break
    if rss_bytes is None:
        raise RuntimeError("VmRSS missing from /proc status")
    return {"cpu_seconds": cpu_seconds, "rss_bytes": rss_bytes}


def parse_concurrencies(value: str) -> list[int]:
    parsed = [int(part.strip()) for part in value.split(",") if part.strip()]
    if not parsed or any(item <= 0 for item in parsed):
        raise argparse.ArgumentTypeError("concurrency values must be positive integers")
    return parsed


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gateway-binary", type=Path, default=Path("target/debug/v1_gateway"))
    parser.add_argument("--calls", type=int, default=1000)
    parser.add_argument("--warmup", type=int, default=50)
    parser.add_argument("--concurrency", type=parse_concurrencies, default=[1, 4, 16])
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)
    if args.calls <= 0 or args.warmup < 0 or not 1 <= args.port <= 65535:
        parser.error("calls must be >0, warmup >=0, and port in 1..65535")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    proc, log_file, base_url = start_gateway(args.gateway_binary, args.port)
    try:
        discover = rpc(base_url, "server/discover", {}, 1)
        if PROTOCOL_VERSION not in discover.get("supportedVersions", []):
            raise RuntimeError("protocol discovery mismatch")
        tools = rpc(base_url, "tools/list", {}, 2).get("tools", [])
        if [tool.get("name") for tool in tools] != ["noop"]:
            raise RuntimeError(f"unexpected benchmark tool surface: {tools}")

        profiles = []
        request_id_base = 10_000
        for concurrency in args.concurrency:
            profile = benchmark_profile(base_url, args.calls, args.warmup, concurrency, request_id_base)
            profiles.append(profile)
            request_id_base += args.calls + args.warmup + 10_000

        health = get_health(base_url)
        if health.get("status") != "ok" or health.get("backend") != "ready":
            raise RuntimeError(f"gateway unhealthy after benchmark: {health}")
        resources = health.get("backend_resources") if isinstance(health.get("backend_resources"), dict) else None
        report = {
            "schema_version": 1,
            "measurement_boundary": "local_client_http_round_trip_through_gateway_and_deterministic_mock_backend",
            "measurement_components": {
                "client_http_round_trip_included": True,
                "gateway_processing_included": True,
                "mock_backend_round_trip_included": True,
                "isolated_gateway_processing_reported": False,
                "remote_network_transport_included": False,
            },
            "production_capacity_claim": False,
            "latency_threshold_gate": False,
            "warmup_calls_per_profile": args.warmup,
            "profiles": profiles,
            "health_after": {"status": health.get("status"), "backend": health.get("backend")},
            "gateway_resources_after": linux_process_resources(proc.pid),
            "backend_resources_after": resources,
        }
        if args.json:
            print(json.dumps(report, indent=2, sort_keys=True))
        else:
            print("V1 informational performance benchmark")
            print(f"boundary={report['measurement_boundary']} production_capacity_claim=false latency_threshold_gate=false")
            for profile in profiles:
                print(
                    "profile "
                    f"concurrency={profile['concurrency']} calls={profile['calls']} "
                    f"elapsed={profile['elapsed_seconds']:.4f}s throughput={profile['throughput_rps']:.1f}req/s "
                    f"mean={profile['mean_ms']:.3f}ms p50={profile['p50_ms']:.3f}ms "
                    f"p95={profile['p95_ms']:.3f}ms p99={profile['p99_ms']:.3f}ms max={profile['max_ms']:.3f}ms"
                )
            print(f"health_after status={health.get('status')} backend={health.get('backend')}")
            gateway_resources = report["gateway_resources_after"]
            if gateway_resources:
                print(
                    "gateway_resources_after "
                    f"cpu_seconds={gateway_resources.get('cpu_seconds')} rss_bytes={gateway_resources.get('rss_bytes')}"
                )
            if resources:
                print(
                    "backend_resources_after "
                    f"cpu_seconds={resources.get('cpu_seconds')} rss_bytes={resources.get('rss_bytes')}"
                )
        return 0
    except Exception:
        print(read_log(log_file), file=sys.stderr)
        raise
    finally:
        stop_gateway(proc)
        log_file.close()


if __name__ == "__main__":
    raise SystemExit(main())
