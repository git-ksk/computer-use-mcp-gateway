#!/usr/bin/env python3
"""Deterministic Cua-shaped MCP fixture for V2 execution-boundary tests.

This is deliberately not a GUI backend. It exposes the Cua tool names used by
`CuaMcpAdapter`; `drag` remains pending until MCP cancellation arrives so tests
can exercise ambiguous desktop-effect handling without touching a real desktop.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

PROTOCOL_VERSION = "2025-11-25"
pending: dict[object, str] = {}
DRAG_MARKER: Path | None = None
CANCEL_MARKER: Path | None = None


def args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--drag-marker")
    parser.add_argument("--cancel-marker")
    return parser.parse_args()


def emit(message: dict) -> None:
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def append(path: Path | None, value: object) -> None:
    if path is None:
        return
    with path.open("a", encoding="utf-8") as handle:
        handle.write(f"{value}\n")
        handle.flush()


def result(request_id: object, payload: dict) -> None:
    emit({"jsonrpc": "2.0", "id": request_id, "result": payload})


def success(request_id: object, structured: dict | None = None) -> None:
    payload: dict = {"content": [], "isError": False}
    if structured is not None:
        payload["structuredContent"] = structured
    result(request_id, payload)


def handle_request(message: dict) -> None:
    method = message.get("method")
    request_id = message.get("id")
    params = message.get("params") or {}

    if method == "initialize":
        result(
            request_id,
            {
                "protocolVersion": params.get("protocolVersion", PROTOCOL_VERSION),
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": {"name": "cumg-v2-cua-fixture", "version": "1.0.0"},
            },
        )
        return
    if method == "ping":
        result(request_id, {})
        return
    if method == "tools/list":
        result(
            request_id,
            {
                "tools": [
                    {"name": "list_apps", "inputSchema": {"type": "object"}},
                    {"name": "get_screen_size", "inputSchema": {"type": "object"}},
                    {"name": "click", "inputSchema": {"type": "object"}},
                    {"name": "drag", "inputSchema": {"type": "object"}},
                ]
            },
        )
        return
    if method != "tools/call":
        emit({"jsonrpc": "2.0", "id": request_id, "error": {"code": -32601, "message": "method not found"}})
        return

    name = params.get("name")
    if name == "list_apps":
        success(request_id, {"apps": [{"name": "Fixture App", "pid": 42}]})
    elif name == "get_screen_size":
        success(request_id, {"width": 1440, "height": 900, "scale_factor": 2.0})
    elif name == "click":
        success(request_id)
    elif name == "drag":
        pending[request_id] = name
        append(DRAG_MARKER, request_id)
    else:
        result(
            request_id,
            {"content": [{"type": "text", "text": "unknown tool"}], "isError": True},
        )


def handle_notification(message: dict) -> None:
    if message.get("method") != "notifications/cancelled":
        return
    request_id = (message.get("params") or {}).get("requestId")
    if request_id not in pending:
        return
    pending.pop(request_id, None)
    append(CANCEL_MARKER, request_id)
    # A provider acknowledging cancellation still does not prove whether the
    # modeled desktop effect happened before cancellation reached it.
    result(
        request_id,
        {"content": [{"type": "text", "text": "cancelled"}], "isError": True},
    )


def main() -> None:
    global DRAG_MARKER, CANCEL_MARKER
    parsed = args()
    DRAG_MARKER = Path(parsed.drag_marker) if parsed.drag_marker else None
    CANCEL_MARKER = Path(parsed.cancel_marker) if parsed.cancel_marker else None
    for raw in sys.stdin:
        raw = raw.strip()
        if not raw:
            continue
        message = json.loads(raw)
        if "id" in message:
            handle_request(message)
        else:
            handle_notification(message)


if __name__ == "__main__":
    main()
