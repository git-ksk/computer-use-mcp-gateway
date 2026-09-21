#!/usr/bin/env python3
"""Own and clean temporary acceptance fixture HTTP servers safely.

The registry records the exact process identity (PID, process group, start marker,
and command fingerprint). Cleanup never kills by port or executable name.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
from typing import Any

SCHEMA_VERSION = 1
LOOPBACK_BINDS = {"127.0.0.1", "::1", "localhost"}
DEFAULT_REGISTRY = Path(tempfile.gettempdir()) / "cumg-acceptance-fixtures-v1.json"
_CHILDREN: dict[int, subprocess.Popen[bytes]] = {}


def _read_registry(path: Path) -> dict[str, Any]:
    if not path.exists():
        return {"schema_version": SCHEMA_VERSION, "fixtures": []}
    data = json.loads(path.read_text(encoding="utf-8"))
    if data.get("schema_version") != SCHEMA_VERSION or not isinstance(data.get("fixtures"), list):
        raise RuntimeError("unsupported acceptance fixture registry")
    return data


def _write_registry(path: Path, data: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(path.name + f".tmp-{os.getpid()}")
    temp.write_text(json.dumps(data, sort_keys=True, indent=2) + "\n", encoding="utf-8")
    os.chmod(temp, 0o600)
    os.replace(temp, path)


def _ps_field(pid: int, field: str) -> str | None:
    result = subprocess.run(
        ["ps", "-p", str(pid), "-o", f"{field}="],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    value = result.stdout.strip()
    return value or None


def _identity(pid: int) -> dict[str, Any] | None:
    state = _ps_field(pid, "stat")
    if state is None or state.startswith("Z"):
        return None
    command = _ps_field(pid, "command")
    start = _ps_field(pid, "lstart")
    pgid_text = _ps_field(pid, "pgid")
    if command is None or start is None or pgid_text is None:
        return None
    try:
        pgid = int(pgid_text)
    except ValueError:
        return None
    return {
        "pid": pid,
        "pgid": pgid,
        "start": start,
        "command_sha256": hashlib.sha256(command.encode("utf-8")).hexdigest(),
    }


def _matches(entry: dict[str, Any]) -> bool:
    current = _identity(int(entry["pid"]))
    if current is None:
        return False
    return all(current.get(key) == entry.get(key) for key in ("pid", "pgid", "start", "command_sha256"))


def start_http(args: argparse.Namespace) -> int:
    if args.bind not in LOOPBACK_BINDS and not args.allow_lan:
        raise RuntimeError("non-loopback acceptance fixture requires --allow-lan")
    root = Path(args.directory).resolve()
    if not root.is_dir():
        raise RuntimeError("fixture directory does not exist")
    registry = Path(args.registry)
    data = _read_registry(registry)

    command = [
        sys.executable,
        "-m",
        "http.server",
        str(args.port),
        "--bind",
        args.bind,
        "--directory",
        str(root),
    ]
    log_path = Path(args.log).resolve() if args.log else root / "http.log"
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_handle = log_path.open("ab", buffering=0)
    try:
        process = subprocess.Popen(
            command,
            stdin=subprocess.DEVNULL,
            stdout=log_handle,
            stderr=subprocess.STDOUT,
            start_new_session=True,
            close_fds=True,
        )
    finally:
        log_handle.close()

    expected_tokens = (
        "-m http.server",
        str(args.port),
        f"--bind {args.bind}",
        f"--directory {root}",
    )
    identity = None
    for _ in range(40):
        command_line = _ps_field(process.pid, "command")
        if command_line is not None and all(token in command_line for token in expected_tokens):
            first = _identity(process.pid)
            time.sleep(0.05)
            second = _identity(process.pid)
            if first is not None and first == second:
                identity = second
                break
        if process.poll() is not None:
            raise RuntimeError("fixture server exited during startup")
        time.sleep(0.05)
    if identity is None:
        process.terminate()
        raise RuntimeError("could not capture stable fixture process identity")

    _CHILDREN[process.pid] = process

    entry = {
        **identity,
        "kind": "python_http_server",
        "bind": args.bind,
        "port": args.port,
        "lan_exposure": args.bind not in LOOPBACK_BINDS,
    }
    data["fixtures"] = [item for item in data["fixtures"] if item.get("pid") != process.pid]
    data["fixtures"].append(entry)
    _write_registry(registry, data)
    print(process.pid)
    return 0


def check(args: argparse.Namespace) -> int:
    registry = Path(args.registry)
    data = _read_registry(registry)
    retained: list[dict[str, Any]] = []
    stale = 0
    mismatched = 0
    for entry in data["fixtures"]:
        pid = int(entry["pid"])
        current = _identity(pid)
        if current is None:
            continue
        retained.append(entry)
        if _matches(entry):
            stale += 1
            exposure = "lan" if entry.get("lan_exposure") else "loopback"
            print(f"STALE kind={entry.get('kind')} pid={pid} exposure={exposure} port={entry.get('port')}")
        else:
            mismatched += 1
            print(f"IDENTITY_MISMATCH pid={pid}", file=sys.stderr)
    if retained != data["fixtures"]:
        data["fixtures"] = retained
        _write_registry(registry, data)
    if mismatched:
        return 2
    return 1 if stale else 0


def _terminate_exact(entry: dict[str, Any], timeout: float) -> str:
    pid = int(entry["pid"])
    pgid = int(entry["pgid"])
    if _identity(pid) is None:
        return "gone"
    if not _matches(entry):
        return "identity_mismatch"

    try:
        os.killpg(pgid, signal.SIGTERM)
    except ProcessLookupError:
        return "gone"

    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if _identity(pid) is None:
            return "stopped"
        if not _matches(entry):
            return "identity_changed"
        time.sleep(0.05)

    if not _matches(entry):
        return "identity_changed"
    try:
        os.killpg(pgid, signal.SIGKILL)
    except ProcessLookupError:
        return "stopped"
    deadline = time.monotonic() + 1.0
    while time.monotonic() < deadline:
        if _identity(pid) is None:
            return "stopped"
        time.sleep(0.05)
    return "cleanup_failed"


def cleanup(args: argparse.Namespace) -> int:
    registry = Path(args.registry)
    data = _read_registry(registry)
    retained: list[dict[str, Any]] = []
    failed = False
    for entry in data["fixtures"]:
        if args.pid is not None and int(entry["pid"]) != args.pid:
            retained.append(entry)
            continue
        outcome = _terminate_exact(entry, args.timeout)
        child = _CHILDREN.pop(int(entry["pid"]), None)
        if child is not None:
            try:
                child.wait(timeout=0.5)
            except subprocess.TimeoutExpired:
                pass
        print(f"CLEANUP pid={entry['pid']} outcome={outcome}")
        if outcome in {"gone", "stopped"}:
            continue
        retained.append(entry)
        failed = True
    data["fixtures"] = retained
    _write_registry(registry, data)
    return 2 if failed else 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    parser.add_argument("--registry", default=str(DEFAULT_REGISTRY))
    sub = parser.add_subparsers(dest="command", required=True)

    start = sub.add_parser("start-http")
    start.add_argument("--directory", required=True)
    start.add_argument("--bind", default="127.0.0.1")
    start.add_argument("--port", type=int, required=True)
    start.add_argument("--log")
    start.add_argument("--allow-lan", action="store_true")
    start.set_defaults(func=start_http)

    inspect = sub.add_parser("check")
    inspect.set_defaults(func=check)

    stop = sub.add_parser("cleanup")
    stop.add_argument("--pid", type=int)
    stop.add_argument("--timeout", type=float, default=2.0)
    stop.set_defaults(func=cleanup)
    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        return int(args.func(args))
    except (OSError, ValueError, RuntimeError, json.JSONDecodeError) as exc:
        print(f"acceptance fixture guard: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
