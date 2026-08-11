#!/usr/bin/env python3
"""Deterministic stdio MCP backend used only by V1 gateway quality tests.

It deliberately implements a tiny tools-only surface:
- noop: immediate, side-effect-free success
- slow: remains pending until notifications/cancelled arrives

The fixture never touches the desktop and is not a production backend.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

PROTOCOL_VERSION = "2025-11-25"
CALL_MARKER = os.environ.get("CUMG_MOCK_CALL_MARKER")
CANCEL_MARKER = os.environ.get("CUMG_MOCK_CANCEL_MARKER")
pending: dict[object, str] = {}


def emit(message: dict) -> None:
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def touch(path: str | None, text: str) -> None:
    if path:
        Path(path).write_text(text, encoding="utf-8")


def result(request_id: object, payload: dict) -> None:
    emit({"jsonrpc": "2.0", "id": request_id, "result": payload})


def error(request_id: object, code: int, message: str) -> None:
    emit(
        {
            "jsonrpc": "2.0",
            "id": request_id,
            "error": {"code": code, "message": message},
        }
    )


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
                "serverInfo": {"name": "cumg-quality-fixture", "version": "1.0.0"},
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
                    {
                        "name": "noop",
                        "description": "Side-effect-free V1 soak fixture",
                        "inputSchema": {"type": "object", "additionalProperties": False},
                    },
                    {
                        "name": "slow",
                        "description": "Waits until downstream cancellation is received",
                        "inputSchema": {"type": "object", "additionalProperties": False},
                    },
                ]
            },
        )
        return

    if method == "tools/call":
        name = params.get("name")
        if name == "noop":
            result(
                request_id,
                {
                    "content": [{"type": "text", "text": "ok"}],
                    "isError": False,
                },
            )
            return
        if name == "slow":
            pending[request_id] = name
            touch(CALL_MARKER, str(request_id))
            return
        result(
            request_id,
            {
                "content": [{"type": "text", "text": "unknown fixture tool"}],
                "isError": True,
            },
        )
        return

    error(request_id, -32601, f"method not found: {method}")


def handle_notification(message: dict) -> None:
    method = message.get("method")
    if method != "notifications/cancelled":
        return

    params = message.get("params") or {}
    request_id = params.get("requestId")
    if request_id not in pending:
        return

    pending.pop(request_id, None)
    touch(CANCEL_MARKER, str(request_id))
    result(
        request_id,
        {
            "content": [{"type": "text", "text": "cancelled"}],
            "isError": True,
        },
    )


def main() -> None:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        message = json.loads(line)
        if "id" in message:
            handle_request(message)
        else:
            handle_notification(message)


if __name__ == "__main__":
    main()
