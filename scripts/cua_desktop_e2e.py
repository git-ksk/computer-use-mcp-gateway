#!/usr/bin/env python3
"""Manual macOS desktop E2E through the real gateway and Cua Driver.

This intentionally performs real GUI actions. It is guarded by
CUMG_DESKTOP_E2E_ACK=1 and is intended only for a dedicated, logged-in,
TCC-granted self-hosted macOS runner. The workflow that invokes it is
workflow_dispatch-only and checks out trusted main.
"""

from __future__ import annotations

import json
import os
import signal
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any, TextIO

HOST = "127.0.0.1"
PORT = 18101
BASE_URL = f"http://{HOST}:{PORT}"
MCP_URL = f"{BASE_URL}/mcp"
HEALTH_URL = f"{BASE_URL}/healthz"
PROTOCOL_VERSION = "2025-11-25"


def gateway_binary() -> Path:
    path = Path("target/debug/v1_gateway")
    if not path.exists():
        raise RuntimeError(f"gateway binary missing: {path}")
    return path.resolve()


def decode_response(resp) -> dict[str, Any]:
    body = resp.read().decode("utf-8")
    content_type = resp.headers.get("Content-Type", "")
    if "application/json" in content_type:
        return json.loads(body)
    for line in body.splitlines():
        if line.startswith("data:"):
            return json.loads(line.removeprefix("data:").strip())
    raise RuntimeError(f"unexpected MCP HTTP response: {body[:500]}")


def http_json(url: str, payload: dict[str, Any] | None = None, timeout: float = 15.0):
    data = None if payload is None else json.dumps(payload).encode("utf-8")
    headers = {"Accept": "application/json, text/event-stream"}
    if data is not None:
        headers["Content-Type"] = "application/json"
        headers["MCP-Protocol-Version"] = PROTOCOL_VERSION
    request = urllib.request.Request(url, data=data, headers=headers)
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return decode_response(response)


def rpc(method: str, params: dict[str, Any] | None = None, request_id: int = 1):
    payload: dict[str, Any] = {"jsonrpc": "2.0", "id": request_id, "method": method}
    if params is not None:
        payload["params"] = params
    response = http_json(MCP_URL, payload)
    if "error" in response:
        raise RuntimeError(f"MCP {method} failed: {response['error']}")
    return response.get("result", {})


def initialize() -> None:
    result = rpc(
        "initialize",
        {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "cua-desktop-e2e", "version": "0.1.0"},
        },
    )
    if result.get("protocolVersion") != PROTOCOL_VERSION:
        raise RuntimeError(f"unexpected MCP negotiation: {result}")


def call_tool(name: str, arguments: dict[str, Any], request_id: int):
    result = rpc(
        "tools/call",
        {"name": name, "arguments": arguments},
        request_id=request_id,
    )
    if result.get("isError") is True:
        raise RuntimeError(f"Cua tool {name} returned an error: {result}")
    return result


def structured(result: dict[str, Any]) -> dict[str, Any]:
    value = result.get("structuredContent")
    if isinstance(value, dict):
        return value
    return {}


def recursively_find_int(value: Any, key: str) -> int | None:
    if isinstance(value, dict):
        candidate = value.get(key)
        if isinstance(candidate, int):
            return candidate
        for child in value.values():
            found = recursively_find_int(child, key)
            if found is not None:
                return found
    elif isinstance(value, list):
        for child in value:
            found = recursively_find_int(child, key)
            if found is not None:
                return found
    return None


def has_screenshot(result: dict[str, Any]) -> bool:
    for block in result.get("content", []):
        if isinstance(block, dict) and (
            block.get("type") == "image"
            or str(block.get("mimeType", "")).startswith("image/")
        ):
            return True
    state = structured(result)
    return any(
        key in state
        for key in ("screenshot", "screenshot_base64", "screenshot_file_path")
    )


def choose_text_element(state: dict[str, Any]) -> dict[str, Any]:
    elements = state.get("elements")
    if not isinstance(elements, list):
        raise RuntimeError("get_window_state returned no structured elements")

    candidates: list[tuple[float, dict[str, Any]]] = []
    for element in elements:
        if not isinstance(element, dict):
            continue
        role = str(element.get("role", "")).lower()
        label = str(element.get("label", "")).lower()
        if not any(token in f"{role} {label}" for token in ("textarea", "text area", "textview", "document", "editor")):
            continue
        frame = element.get("frame") or {}
        width = float(frame.get("w", frame.get("width", 0)) or 0)
        height = float(frame.get("h", frame.get("height", 0)) or 0)
        if width <= 10 or height <= 10:
            continue
        candidates.append((width * height, element))

    if not candidates:
        roles = sorted(
            {
                str(element.get("role"))
                for element in elements
                if isinstance(element, dict) and element.get("role")
            }
        )
        raise RuntimeError(f"could not locate TextEdit editor element; roles={roles[:40]}")
    candidates.sort(key=lambda item: item[0], reverse=True)
    return candidates[0][1]


def wait_for_window(pid: int, preferred_title: str, timeout: float = 10.0) -> dict[str, Any]:
    deadline = time.time() + timeout
    attempt = 0
    while time.time() < deadline:
        state = structured(call_tool("list_windows", {"pid": pid}, 110 + attempt))
        windows = state.get("windows")
        if isinstance(windows, list):
            eligible = [window for window in windows if isinstance(window, dict)]
            titled = [
                window
                for window in eligible
                if preferred_title and preferred_title in str(window.get("title", ""))
            ]
            visible = [window for window in eligible if window.get("is_on_screen") is True]
            for pool in (titled, visible, eligible):
                if pool:
                    window_id = pool[0].get("window_id")
                    if isinstance(window_id, int):
                        return pool[0]
        attempt += 1
        time.sleep(0.25)
    raise RuntimeError(f"TextEdit launched but no window became available within {timeout:.1f}s")


def read_log(log_file: TextIO) -> str:
    log_file.flush()
    pos = log_file.tell()
    log_file.seek(0)
    text = log_file.read()
    log_file.seek(pos)
    return text


def start_gateway() -> tuple[subprocess.Popen[str], TextIO]:
    env = os.environ.copy()
    env.update(
        {
            "CUMG_BIND": f"{HOST}:{PORT}",
            "CUMG_BACKEND_COMMAND": "cua-driver",
            "CUMG_BACKEND_ARGS": "mcp",
            "CUMG_ALLOW_TOOLS": "launch_app,kill_app,list_windows,get_window_state,click,type_text",
            "CUMG_CONNECT_TIMEOUT_SECS": "15",
            "CUMG_TOOL_TIMEOUT_SECS": "60",
            "CUMG_RECONNECT_ATTEMPTS": "3",
            "CUMG_RECONNECT_BACKOFF_MS": "250",
            "RUST_LOG": "warn,computer_use_mcp_gateway=info",
        }
    )
    log_file = tempfile.TemporaryFile(mode="w+", encoding="utf-8")
    process = subprocess.Popen(
        [str(gateway_binary())],
        env=env,
        stdout=log_file,
        stderr=subprocess.STDOUT,
        text=True,
    )

    deadline = time.time() + 60
    while time.time() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"gateway exited early:\n{read_log(log_file)}")
        try:
            health = http_json(HEALTH_URL, timeout=2)
            if health.get("status") == "ok":
                return process, log_file
        except (urllib.error.URLError, TimeoutError, ConnectionError, json.JSONDecodeError):
            pass
        time.sleep(0.25)
    raise RuntimeError(f"gateway did not become ready:\n{read_log(log_file)}")


def stop_gateway(process: subprocess.Popen[str], log_file: TextIO) -> None:
    try:
        if process.poll() is None:
            process.send_signal(signal.SIGINT)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
    finally:
        text = read_log(log_file)
        if text:
            print("--- gateway log ---")
            print(text[-8000:])
        log_file.close()


def main() -> int:
    if os.environ.get("CUMG_DESKTOP_E2E_ACK") != "1":
        raise SystemExit(
            "refusing real desktop automation without CUMG_DESKTOP_E2E_ACK=1"
        )
    if os.uname().sysname != "Darwin":
        raise SystemExit("this desktop fixture currently targets the macOS TextEdit runner")

    process, log_file = start_gateway()
    pid: int | None = None
    fixture_path = Path(tempfile.gettempdir()) / f"cumg-desktop-e2e-{os.getpid()}.txt"
    fixture_path.write_text("", encoding="utf-8")
    try:
        initialize()
        launch = call_tool(
            "launch_app",
            {
                "bundle_id": "com.apple.TextEdit",
                "creates_new_application_instance": True,
                "urls": [str(fixture_path)],
            },
            10,
        )
        launch_state = structured(launch)
        pid = recursively_find_int(launch_state, "pid")
        if pid is None:
            raise RuntimeError(f"launch_app returned no pid: {launch_state}")

        window = wait_for_window(pid, fixture_path.name)
        window_id = window["window_id"]

        state_result = call_tool(
            "get_window_state",
            {"pid": pid, "window_id": window_id},
            13,
        )
        if not has_screenshot(state_result):
            raise RuntimeError("get_window_state did not return screenshot evidence")
        state = structured(state_result)
        element = choose_text_element(state)
        element_index = element.get("element_index")
        if not isinstance(element_index, int):
            raise RuntimeError(f"editor element had no element_index: {element}")

        frame = element.get("frame") or {}
        bounds = window.get("bounds") or {}
        try:
            local_x = float(frame["x"]) - float(bounds["x"]) + min(20.0, float(frame["w"]) / 2.0)
            local_y = float(frame["y"]) - float(bounds["y"]) + min(20.0, float(frame["h"]) / 2.0)
        except (KeyError, TypeError, ValueError) as exc:
            raise RuntimeError(f"could not derive editor pixel target: element={element} window={window}") from exc

        call_tool(
            "click",
            {
                "pid": pid,
                "window_id": window_id,
                "x": local_x,
                "y": local_y,
            },
            14,
        )

        marker = f"CUMG_DESKTOP_E2E_{int(time.time())}"
        type_args: dict[str, Any] = {
            "pid": pid,
            "window_id": window_id,
            "text": marker,
        }
        element_token = element.get("element_token")
        snapshot_id = state.get("snapshot_id")
        if isinstance(element_token, str):
            type_args["element_token"] = element_token
        elif isinstance(snapshot_id, str):
            type_args["element_index"] = element_index
            type_args["snapshot_id"] = snapshot_id
        else:
            raise RuntimeError("editor element had neither element_token nor snapshot_id")
        call_tool("type_text", type_args, 15)

        verified_result = call_tool(
            "get_window_state",
            {"pid": pid, "window_id": window_id, "include_screenshot": False},
            16,
        )
        verified = structured(verified_result)
        evidence = json.dumps(verified, ensure_ascii=False)
        if marker not in evidence:
            raise RuntimeError("typed marker was not present in independent AX readback")

        print(
            "PASS desktop E2E: gateway -> Cua -> TextEdit -> screenshot -> click -> type -> AX verify"
        )
        return 0
    finally:
        if pid is not None:
            try:
                call_tool("kill_app", {"pid": pid}, 99)
            except Exception as exc:  # cleanup only
                print(f"cleanup warning: {exc}")
        try:
            fixture_path.unlink(missing_ok=True)
        except OSError as exc:
            print(f"fixture cleanup warning: {exc}")
        stop_gateway(process, log_file)


if __name__ == "__main__":
    raise SystemExit(main())
