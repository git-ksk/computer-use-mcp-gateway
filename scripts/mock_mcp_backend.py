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
FAIL_START_SESSION = False
VERIFY_MARKER: str | None = None
ENDED_SESSIONS: set[str] = set()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--call-marker")
    parser.add_argument("--cancel-marker")
    parser.add_argument("--args-marker")
    parser.add_argument("--slow-list-apps", action="store_true")
    parser.add_argument("--slow-type-text", action="store_true")
    parser.add_argument("--fail-start-session", action="store_true")
    parser.add_argument("--verify-marker")
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
                        "name": "list_windows",
                        "description": "Semantic adapter fixture for window listing",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "launch_app",
                        "description": "Semantic adapter fixture for application launch",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "get_window_state",
                        "description": "Semantic adapter fixture for window inspection",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "verify_state",
                        "description": "Semantic adapter fixture for UI verification",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "click",
                        "description": "Desktop semantic fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "drag",
                        "description": "Desktop semantic fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "kill_app",
                        "description": "Desktop semantic fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "bring_to_front",
                        "description": "Desktop semantic fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "set_window_frame",
                        "description": "Desktop semantic fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "invoke_menu",
                        "description": "Desktop semantic fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "press_key",
                        "description": "Desktop semantic fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "scroll",
                        "description": "Desktop semantic fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "clipboard_read",
                        "description": "Desktop semantic fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "clipboard_write",
                        "description": "Desktop semantic fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "get_cursor_position",
                        "description": "Desktop semantic fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "move_cursor",
                        "description": "Desktop semantic fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "set_value",
                        "description": "Desktop semantic fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "zoom",
                        "description": "Desktop semantic fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "start_session",
                        "description": "Interaction lifecycle fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "escalate_session",
                        "description": "Interaction lifecycle fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
                    },
                    {
                        "name": "end_session",
                        "description": "Interaction lifecycle fixture",
                        "inputSchema": {"type": "object", "additionalProperties": True},
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
        if name == "list_windows":
            result(
                request_id,
                {
                    "content": [],
                    "structuredContent": {
                        "current_space_id": 1,
                        "windows": [
                            {
                                "window_id": 77,
                                "pid": 101,
                                "app_name": "Fixture A",
                                "title": "Main",
                                "bounds": {"x": 10.0, "y": 20.0, "width": 800.0, "height": 600.0},
                                "is_on_screen": True,
                                "on_current_space": True,
                            }
                        ],
                    },
                    "isError": False,
                },
            )
            return
        if name == "launch_app":
            arguments = params.get("arguments") or {}
            result(
                request_id,
                {
                    "content": [],
                    "structuredContent": {
                        "bundle_id": arguments.get("bundle_id") or "fixture.app",
                        "name": arguments.get("name") or "Fixture A",
                        "pid": 101,
                        "launch_state": {"process_running": True, "requested": True, "window_ready": True},
                        "windows": [
                            {
                                "window_id": 77,
                                "pid": 101,
                                "app_name": "Fixture A",
                                "title": "Main",
                                "bounds": {"x": 10.0, "y": 20.0, "width": 800.0, "height": 600.0},
                                "is_on_screen": True,
                                "on_current_space": True,
                            }
                        ],
                    },
                    "isError": False,
                },
            )
            return
        if name == "get_window_state":
            arguments = params.get("arguments") or {}
            content = []
            structured = {
                "pid": arguments.get("pid", 101),
                "window_id": arguments.get("window_id", 77),
                "snapshot_id": "sfixture1",
                "elements_complete": True,
                "elements": [
                    {
                        "depth": 0,
                        "element_index": 0,
                        "element_token": "sfixture1:0",
                        "frame": {"x": 10.0, "y": 20.0, "w": 800.0, "h": 600.0},
                        "label": "Main",
                        "role": "AXWindow",
                    },
                    {
                        "depth": 1,
                        "element_index": 1,
                        "element_token": "sfixture1:1",
                        "parent_index": 0,
                        "frame": {"x": 20.0, "y": 40.0, "w": 100.0, "h": 30.0},
                        "label": "Run",
                        "role": "AXButton",
                        "enabled": True,
                        "selected": False,
                    },
                ],
            }
            if arguments.get("include_screenshot", True):
                content.append({"type": "image", "data": "iVBORw0KGgoAAAANSUhEUgAAAOYAAAGY", "mimeType": "image/png"})
                structured.update({"screenshot_width": 230, "screenshot_height": 408, "screenshot_mime_type": "image/png"})
            result(request_id, {"content": content, "structuredContent": structured, "isError": False})
            return
        if name == "verify_state":
            arguments = params.get("arguments") or {}
            touch(VERIFY_MARKER, "verify_state")
            session = arguments.get("session")
            if session in ENDED_SESSIONS:
                result(
                    request_id,
                    {
                        "content": [{"type": "text", "text": "session ended"}],
                        "isError": True,
                    },
                )
                return
            content = []
            if arguments.get("include_screenshot", False):
                content.append({"type": "image", "data": "iVBORw0KGgoAAAANSUhEUgAAAOYAAAGY", "mimeType": "image/png"})
            expect = arguments.get("expect") or []
            result(
                request_id,
                {
                    "content": content,
                    "structuredContent": {
                        "status": "satisfied",
                        "stable": True,
                        "samples": 1,
                        "predicates": [
                            {"index": i, "status": "satisfied", "unknown_reason": None}
                            for i, _ in enumerate(expect)
                        ],
                    },
                    "isError": False,
                },
            )
            return
        if name in {
            "click",
            "drag",
            "kill_app",
            "set_window_frame",
            "invoke_menu",
            "press_key",
            "scroll",
            "clipboard_write",
            "move_cursor",
            "set_value",
            "start_session",
            "escalate_session",
            "end_session",
        }:
            arguments = params.get("arguments") or {}
            session = arguments.get("session")
            if name == "start_session" and FAIL_START_SESSION:
                result(
                    request_id,
                    {
                        "content": [{"type": "text", "text": "start session failed"}],
                        "isError": True,
                    },
                )
                return
            if name == "end_session" and isinstance(session, str):
                ENDED_SESSIONS.add(session)
            elif name == "start_session" and isinstance(session, str):
                ENDED_SESSIONS.discard(session)
            touch(
                ARGS_MARKER,
                json.dumps({"tool": name, "arguments": arguments}, sort_keys=True, separators=(",", ":")),
            )
            structured = None
            if name == "clipboard_write":
                structured = {"types": ["public.utf8-plain-text"]}
            payload = {
                "content": [{"type": "text", "text": "ok"}],
                "isError": False,
            }
            if structured is not None:
                payload["structuredContent"] = structured
            result(request_id, payload)
            return
        if name == "bring_to_front":
            arguments = params.get("arguments") or {}
            result(
                request_id,
                {
                    "content": [],
                    "structuredContent": {
                        "process_activated": True,
                        "exact_window_effect": {
                            "verified": arguments.get("window_id") == 77,
                        },
                    },
                    "isError": False,
                },
            )
            return
        if name == "clipboard_read":
            arguments = params.get("arguments") or {}
            result(
                request_id,
                {
                    "content": [],
                    "structuredContent": {
                        "types": ["public.utf8-plain-text"],
                        "text": "fixture clipboard" if arguments.get("include_text") else None,
                    },
                    "isError": False,
                },
            )
            return
        if name == "get_cursor_position":
            result(
                request_id,
                {
                    "content": [],
                    "structuredContent": {"x": 123.0, "y": 456.0},
                    "isError": False,
                },
            )
            return
        if name == "zoom":
            result(
                request_id,
                {
                    "content": [
                        {"type": "image", "data": "/9j/2Q==", "mimeType": "image/jpeg"}
                    ],
                    "structuredContent": {
                        "format": "jpeg",
                        "mime_type": "image/jpeg",
                        "screenshot_mime_type": "image/jpeg",
                        "width": 144,
                        "height": 96,
                    },
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
    global FAIL_START_SESSION, VERIFY_MARKER
    args = parse_args()
    CALL_MARKER = args.call_marker
    CANCEL_MARKER = args.cancel_marker
    ARGS_MARKER = args.args_marker
    SLOW_LIST_APPS = args.slow_list_apps
    SLOW_TYPE_TEXT = args.slow_type_text
    FAIL_START_SESSION = args.fail_start_session
    VERIFY_MARKER = args.verify_marker

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
