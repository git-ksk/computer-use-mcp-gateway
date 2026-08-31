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
TRANSFER_MARKER: Path | None = None
AMBIGUOUS_CLICK_MARKER: Path | None = None
SUCCESSFUL_CLICK_MARKER: Path | None = None
AMBIGUOUS_BROWSER_CLICK_MARKER: Path | None = None
DROP_AFTER_CLICK_MARKER: Path | None = None
FAIL_LIST_APPS = False
SLOW_BROWSER_UPLOAD = False
SLOW_BROWSER_DOWNLOAD = False
FAIL_BROWSER_UPLOAD = False
FAIL_BROWSER_DOWNLOAD = False
DOWNLOAD_PAYLOAD = b"fixture-download"


def args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--drag-marker")
    parser.add_argument("--cancel-marker")
    parser.add_argument("--transfer-marker")
    parser.add_argument("--ambiguous-click-marker")
    parser.add_argument("--successful-click-marker")
    parser.add_argument("--ambiguous-browser-click-marker")
    parser.add_argument("--drop-after-click-marker")
    parser.add_argument("--fail-list-apps", action="store_true")
    parser.add_argument("--slow-browser-upload", action="store_true")
    parser.add_argument("--slow-browser-download", action="store_true")
    parser.add_argument("--fail-browser-upload", action="store_true")
    parser.add_argument("--fail-browser-download", action="store_true")
    parser.add_argument("--download-payload", default="fixture-download")
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
                    {"name": "browser_click", "inputSchema": {"type": "object"}},
                    {"name": "browser_set_input_files", "inputSchema": {"type": "object"}},
                    {"name": "browser_download", "inputSchema": {"type": "object"}},
                ]
            },
        )
        return
    if method != "tools/call":
        emit({"jsonrpc": "2.0", "id": request_id, "error": {"code": -32601, "message": "method not found"}})
        return

    name = params.get("name")
    if name == "list_apps":
        if FAIL_LIST_APPS:
            result(
                request_id,
                {"content": [{"type": "text", "text": "fixture read failure"}], "isError": True},
            )
        else:
            success(request_id, {"apps": [{"name": "Fixture App", "pid": 42}]})
    elif name == "get_screen_size":
        success(request_id, {"width": 1440, "height": 900, "scale_factor": 2.0})
    elif name == "click":
        arguments = params.get("arguments") or {}
        if DROP_AFTER_CLICK_MARKER is not None:
            append(DROP_AFTER_CLICK_MARKER, {"tool": name, "dispatched": True})
            raise SystemExit(0)
        if (
            SUCCESSFUL_CLICK_MARKER is not None
            and arguments.get("x") == 31
            and arguments.get("y") == 41
        ):
            append(SUCCESSFUL_CLICK_MARKER, {"tool": name, "dispatched": True})
            success(request_id)
        elif AMBIGUOUS_CLICK_MARKER is not None:
            append(AMBIGUOUS_CLICK_MARKER, {"tool": name, "dispatched": True})
            result(
                request_id,
                {"content": [{"type": "text", "text": "fixture post-effect failure"}], "isError": True},
            )
        else:
            success(request_id)
    elif name == "browser_click":
        if AMBIGUOUS_BROWSER_CLICK_MARKER is not None:
            append(AMBIGUOUS_BROWSER_CLICK_MARKER, {"tool": name, "dispatched": True})
            result(
                request_id,
                {"content": [{"type": "text", "text": "fixture post-effect browser failure"}], "isError": True},
            )
        else:
            success(
                request_id,
                {
                    "status": "ok",
                    "target_id": (params.get("arguments") or {}).get("target_id"),
                    "tab_id": (params.get("arguments") or {}).get("tab_id"),
                    "effect": "unverifiable",
                },
            )
    elif name == "drag":
        pending[request_id] = name
        append(DRAG_MARKER, request_id)
    elif name == "browser_set_input_files":
        arguments = params.get("arguments") or {}
        files = arguments.get("files") or []
        append(TRANSFER_MARKER, {"tool": name, "args": arguments})
        if FAIL_BROWSER_UPLOAD:
            result(
                request_id,
                {"content": [{"type": "text", "text": "upload failed"}], "isError": True},
            )
        elif SLOW_BROWSER_UPLOAD:
            pending[request_id] = name
        elif not files or not all(Path(value).is_absolute() and Path(value).is_file() for value in files):
            result(
                request_id,
                {"content": [{"type": "text", "text": "invalid staged file"}], "isError": True},
            )
        else:
            success(
                request_id,
                {
                    "status": "ok",
                    "target_id": arguments.get("target_id"),
                    "tab_id": arguments.get("tab_id"),
                    "ref": arguments.get("ref"),
                    "file_count": len(files),
                },
            )
    elif name == "browser_download":
        arguments = params.get("arguments") or {}
        append(TRANSFER_MARKER, {"tool": name, "args": arguments})
        if FAIL_BROWSER_DOWNLOAD:
            result(
                request_id,
                {"content": [{"type": "text", "text": "download failed"}], "isError": True},
            )
        elif arguments.get("_cua_browser_download_mcp_host_approved") is not True:
            success(
                request_id,
                {
                    "status": "refused",
                    "refusal": {"code": "browser_consent_required", "message": "approval required"},
                },
            )
        elif SLOW_BROWSER_DOWNLOAD:
            pending[request_id] = name
        else:
            root = Path(arguments.get("destination_root", ""))
            if not root.is_absolute() or not root.is_dir():
                result(
                    request_id,
                    {"content": [{"type": "text", "text": "invalid destination"}], "isError": True},
                )
            else:
                download_id = "fixture-download-guid"
                (root / download_id).write_bytes(DOWNLOAD_PAYLOAD)
                success(
                    request_id,
                    {"status": "completed", "download_id": download_id, "bytes": len(DOWNLOAD_PAYLOAD)},
                )
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
    global DRAG_MARKER, CANCEL_MARKER, TRANSFER_MARKER
    global AMBIGUOUS_CLICK_MARKER, SUCCESSFUL_CLICK_MARKER
    global AMBIGUOUS_BROWSER_CLICK_MARKER, DROP_AFTER_CLICK_MARKER
    global FAIL_LIST_APPS
    global SLOW_BROWSER_UPLOAD, SLOW_BROWSER_DOWNLOAD
    global FAIL_BROWSER_UPLOAD, FAIL_BROWSER_DOWNLOAD, DOWNLOAD_PAYLOAD
    parsed = args()
    DRAG_MARKER = Path(parsed.drag_marker) if parsed.drag_marker else None
    CANCEL_MARKER = Path(parsed.cancel_marker) if parsed.cancel_marker else None
    TRANSFER_MARKER = Path(parsed.transfer_marker) if parsed.transfer_marker else None
    AMBIGUOUS_CLICK_MARKER = (
        Path(parsed.ambiguous_click_marker) if parsed.ambiguous_click_marker else None
    )
    SUCCESSFUL_CLICK_MARKER = (
        Path(parsed.successful_click_marker) if parsed.successful_click_marker else None
    )
    AMBIGUOUS_BROWSER_CLICK_MARKER = (
        Path(parsed.ambiguous_browser_click_marker)
        if parsed.ambiguous_browser_click_marker
        else None
    )
    DROP_AFTER_CLICK_MARKER = (
        Path(parsed.drop_after_click_marker) if parsed.drop_after_click_marker else None
    )
    FAIL_LIST_APPS = parsed.fail_list_apps
    SLOW_BROWSER_UPLOAD = parsed.slow_browser_upload
    SLOW_BROWSER_DOWNLOAD = parsed.slow_browser_download
    FAIL_BROWSER_UPLOAD = parsed.fail_browser_upload
    FAIL_BROWSER_DOWNLOAD = parsed.fail_browser_download
    DOWNLOAD_PAYLOAD = parsed.download_payload.encode("utf-8")
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
