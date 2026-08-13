#!/usr/bin/env python3
"""Deterministic stdio MCP backend used only by V1 gateway quality tests.

It deliberately implements a tiny tools-only surface:
- noop: immediate, side-effect-free success
- slow: remains pending until notifications/cancelled arrives
- echo_contract: records exact arguments and returns a deliberately inconsistent
  application/window identity payload so passthrough behavior can be regression-tested

The fixture never touches the desktop and is not a production backend.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

PROTOCOL_VERSION = "2025-11-25"
pending: dict[object, str] = {}
CALL_MARKER: str | None = None
CANCEL_MARKER: str | None = None
ARGS_MARKER: str | None = None
SLOW_LIST_APPS = False
SLOW_TYPE_TEXT = False


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--call-marker")
    parser.add_argument("--cancel-marker")
    parser.add_argument("--args-marker")
    parser.add_argument("--slow-list-apps", action="store_true")
    parser.add_argument("--slow-type-text", action="store_true")
    return parser.parse_args()


def emit(message: dict) -> None:
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def touch(path: str | None, text: str) -> None:
    if not path:
        return
    marker = Path(path)
    temporary = marker.with_name(f"{marker.name}.{os.getpid()}.tmp")
    temporary.write_text(text, encoding="utf-8")
    temporary.replace(marker)


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
                    {
                        "name": "list_apps",
                        "description": "Semantic adapter fixture for application listing",
                        "inputSchema": {"type": "object", "additionalProperties": False},
                    },
                    {
                        "name": "get_screen_size",
                        "description": "Semantic adapter fixture for screen geometry",
                        "inputSchema": {"type": "object", "additionalProperties": False},
                    },
                    {
                        "name": "get_desktop_state",
                        "description": "Semantic adapter fixture for desktop screenshot",
                        "inputSchema": {"type": "object", "additionalProperties": False},
                    },
                    {
                        "name": "type_text",
                        "description": "Semantic adapter fixture for typed text",
                        "inputSchema": {"type": "object", "additionalProperties": False},
                    },
                    {
                        "name": "echo_contract",
                        "description": "Records exact arguments and returns backend identity data unchanged",
                        "inputSchema": {"type": "object", "additionalProperties": True},
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
        if name == "list_apps":
            if SLOW_LIST_APPS:
                pending[request_id] = name
                touch(CALL_MARKER, str(request_id))
                return
            result(
                request_id,
                {
                    "content": [],
                    "structuredContent": {
                        "apps": [
                            {"name": "Fixture A", "pid": 101},
                            {"name": "Fixture B", "pid": 202},
                        ]
                    },
                    "isError": False,
                },
            )
            return
        if name == "get_screen_size":
            result(
                request_id,
                {
                    "content": [],
                    "structuredContent": {
                        "width": 1920,
                        "height": 1080,
                        "scale_factor": 2.0,
                    },
                    "isError": False,
                },
            )
            return
        if name == "get_desktop_state":
            result(
                request_id,
                {
                    "content": [
                        {
                            "type": "image",
                            "data": "iVBORw0KGgo=",
                            "mimeType": "image/png",
                        },
                        {"type": "text", "text": "fixture screenshot"},
                    ],
                    "structuredContent": {
                        "screenshot_width": 2,
                        "screenshot_height": 1,
                        "screenshot_mime_type": "image/png",
                    },
                    "isError": False,
                },
            )
            return
        if name == "type_text":
            arguments = params.get("arguments") or {}
            touch(
                ARGS_MARKER,
                json.dumps(arguments, sort_keys=True, separators=(",", ":")),
            )
            if SLOW_TYPE_TEXT:
                pending[request_id] = name
                touch(CALL_MARKER, str(request_id))
                return
            result(
                request_id,
                {
                    "content": [{"type": "text", "text": "typed"}],
                    "isError": False,
                },
            )
            return
        if name == "echo_contract":
            arguments = params.get("arguments") or {}
            touch(
                ARGS_MARKER,
                json.dumps(arguments, sort_keys=True, separators=(",", ":")),
            )
            result(
                request_id,
                {
                    "content": [{"type": "text", "text": "backend-identity-fixture"}],
                    "structuredContent": {
                        "application": {
                            "name": "Cua Driver",
                            "running": False,
                            "pid": 0,
                        },
                        "window": {
                            "application": "Cua Driver",
                            "pid": 31438,
                        },
                    },
                    "isError": False,
                },
            )
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
    global CALL_MARKER, CANCEL_MARKER, ARGS_MARKER, SLOW_LIST_APPS, SLOW_TYPE_TEXT
    args = parse_args()
    CALL_MARKER = args.call_marker
    CANCEL_MARKER = args.cancel_marker
    ARGS_MARKER = args.args_marker
    SLOW_LIST_APPS = args.slow_list_apps
    SLOW_TYPE_TEXT = args.slow_type_text

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
