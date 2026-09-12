#!/usr/bin/env python3
"""Fail-closed Windows upgrade for one reviewed CUMG V2 runtime set."""
from __future__ import annotations

import argparse
import ctypes
from dataclasses import dataclass
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import sys
import time
import uuid
from typing import Protocol

SCRIPT_DIR = Path(__file__).resolve().parent
RELEASE_SCRIPT = SCRIPT_DIR / "v2_release_candidate.py"
SPEC = importlib.util.spec_from_file_location("v2_release_candidate_for_windows_upgrade", RELEASE_SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("release candidate verifier is unavailable")
release_candidate = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release_candidate)

STATUS_SCHEMA_VERSION = 1
MAX_CONFIG_BYTES = 128 * 1024
MAX_STATUS_BYTES = 128 * 1024
WINDOWS_BINARIES = tuple(f"{name}.exe" for name in release_candidate.PLATFORM_BINARIES["windows"])
SAFE_TASK_PREFIX_RE = re.compile(r"^[A-Za-z0-9._-]{1,80}$")
SAFE_ENV_RE = re.compile(r"%([A-Za-z_][A-Za-z0-9_]*)%")
FLAG_RE = re.compile(r"--[A-Za-z0-9][A-Za-z0-9-]*")
USAGE_RE = re.compile(r"(?ms)^Usage:\s*(.*?)(?:\r?\n\r?\n|\Z)")
LOOPBACK_RE = re.compile(r"^127\.0\.0\.1:([0-9]{1,5})$")


class UpgradeError(RuntimeError):
    def __init__(self, code: str):
        if not re.fullmatch(r"[a-z0-9_:-]{1,120}", code):
            code = "unexpected_upgrade_failure"
        super().__init__(code)
        self.code = code


@dataclass(frozen=True)
class CandidateIdentity:
    package_version: str
    source_commit: str
    hub_agent_schema_version: int
    files: dict[str, str]

    def as_dict(self) -> dict[str, object]:
        return {
            "package_version": self.package_version,
            "source_commit": self.source_commit,
            "hub_agent_schema_version": self.hub_agent_schema_version,
            "files": dict(sorted(self.files.items())),
        }


@dataclass(frozen=True)
class ReviewedConfig:
    path: Path
    component: str
    executable: Path
    working_directory: Path
    log_directory: Path
    arguments: tuple[str, ...]
    raw: dict[str, object]

    def pid_file(self) -> Path:
        configured = self.raw.get("pidFile")
        if isinstance(configured, str) and configured.strip():
            return expand_path(configured)
        return self.log_directory / f"{self.component}.pid"


class RuntimeController(Protocol):
    def validate(self) -> None: ...
    def stop_pair(self, hub: ReviewedConfig, agent: ReviewedConfig) -> None: ...
    def start_hub(self) -> None: ...
    def start_agent(self) -> None: ...
    def wait_hub_healthy(self, hub: ReviewedConfig, timeout_seconds: int) -> None: ...
    def wait_agent_stable(self, agent: ReviewedConfig, stable_seconds: int, timeout_seconds: int) -> None: ...
    def external_smoke(self) -> None: ...


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def is_relative_to(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def expand_environment(value: str) -> str:
    def replace(match: re.Match[str]) -> str:
        name = match.group(1)
        if name not in os.environ:
            raise UpgradeError("config_environment_variable_unavailable")
        return os.environ[name]
    return SAFE_ENV_RE.sub(replace, value)


def expand_path(value: str) -> Path:
    path = Path(expand_environment(value))
    if not path.is_absolute():
        raise UpgradeError("config_path_not_absolute")
    return path.resolve(strict=False)


def require_regular(path: Path, code: str) -> Path:
    try:
        info = path.lstat()
    except FileNotFoundError as exc:
        raise UpgradeError(code) from exc
    if path.is_symlink() or not path.is_file() or info.st_size <= 0:
        raise UpgradeError(code)
    return path


def load_json_file(path: Path, *, max_bytes: int, code: str) -> dict[str, object]:
    require_regular(path, code)
    if path.stat().st_size > max_bytes:
        raise UpgradeError(code)
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (UnicodeError, json.JSONDecodeError, OSError) as exc:
        raise UpgradeError(code) from exc
    if not isinstance(value, dict):
        raise UpgradeError(code)
    return value


def verify_candidate(bundle_dir: Path) -> CandidateIdentity:
    try:
        manifest = release_candidate.verify_bundle_dir(bundle_dir.resolve(strict=True))
    except Exception as exc:
        raise UpgradeError("candidate_verification_failed") from exc
    if manifest.get("platform") != "windows":
        raise UpgradeError("candidate_platform_mismatch")
    records = manifest.get("files")
    if not isinstance(records, list):
        raise UpgradeError("candidate_manifest_invalid")
    by_path: dict[str, str] = {}
    for record in records:
        if isinstance(record, dict) and isinstance(record.get("path"), str) and isinstance(record.get("sha256"), str):
            by_path[str(record["path"])] = str(record["sha256"])
    files: dict[str, str] = {}
    for name in WINDOWS_BINARIES:
        digest = by_path.get(f"bin/{name}")
        if digest is None:
            raise UpgradeError("candidate_runtime_set_incomplete")
        files[name] = digest
    return CandidateIdentity(
        package_version=str(manifest["package_version"]),
        source_commit=str(manifest["source_commit"]),
        hub_agent_schema_version=int(manifest["hub_agent_schema_version"]),
        files=files,
    )


def config_arguments(value: dict[str, object]) -> tuple[str, ...]:
    arguments = value.get("arguments")
    if not isinstance(arguments, list) or any(not isinstance(item, str) for item in arguments):
        raise UpgradeError("candidate_config_arguments_invalid")
    return tuple(expand_environment(str(item)) for item in arguments)


def load_reviewed_config(path: Path, component: str, data_root: Path) -> ReviewedConfig:
    value = load_json_file(path, max_bytes=MAX_CONFIG_BYTES, code="candidate_config_invalid")
    if value.get("component") != component:
        raise UpgradeError(f"candidate_{component}_component_mismatch")
    for field in ("executable", "workingDirectory", "logDirectory"):
        if not isinstance(value.get(field), str) or not str(value[field]).strip():
            raise UpgradeError(f"candidate_{component}_config_missing_{field.lower()}")
    executable = expand_path(str(value["executable"]))
    expected = (data_root / "bin" / f"v2_{component}.exe").resolve(strict=False)
    if os.path.normcase(str(executable)) != os.path.normcase(str(expected)):
        raise UpgradeError(f"candidate_{component}_executable_mismatch")
    working = expand_path(str(value["workingDirectory"]))
    logs = expand_path(str(value["logDirectory"]))
    root = data_root.resolve(strict=True)
    if not is_relative_to(working, root) or not is_relative_to(logs, root):
        raise UpgradeError(f"candidate_{component}_path_outside_data_root")
    return ReviewedConfig(path.resolve(strict=True), component, executable, working, logs, config_arguments(value), value)


def config_flags(config: ReviewedConfig) -> set[str]:
    return {item for item in config.arguments if FLAG_RE.fullmatch(item)}


def required_flags_from_help(binary: Path) -> set[str]:
    require_regular(binary, "candidate_binary_missing")
    try:
        result = subprocess.run([str(binary), "--help"], stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=15, check=False)
    except (OSError, subprocess.SubprocessError) as exc:
        raise UpgradeError("candidate_help_probe_failed") from exc
    if result.returncode != 0:
        raise UpgradeError("candidate_help_probe_failed")
    match = USAGE_RE.search(result.stdout + "\n" + result.stderr)
    if match is None:
        raise UpgradeError("candidate_help_usage_missing")
    return set(FLAG_RE.findall(match.group(1)))


def validate_required_flags(config: ReviewedConfig, required: set[str]) -> None:
    missing = sorted(required - config_flags(config))
    if "--allowed-file-root" in missing:
        raise UpgradeError("candidate_agent_missing_allowed_file_root")
    if missing:
        raise UpgradeError(f"candidate_{config.component}_missing_required_flag")


def preflight_configs(bundle_dir: Path, data_root: Path, hub_path: Path, agent_path: Path) -> tuple[ReviewedConfig, ReviewedConfig]:
    hub = load_reviewed_config(hub_path, "hub", data_root)
    agent = load_reviewed_config(agent_path, "agent", data_root)
    validate_required_flags(hub, required_flags_from_help(bundle_dir / "bin" / "v2_hub.exe"))
    validate_required_flags(agent, required_flags_from_help(bundle_dir / "bin" / "v2_agent.exe"))
    return hub, agent


def atomic_copy(source: Path, destination: Path) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_name(f".{destination.name}.new.{os.getpid()}.{uuid.uuid4().hex[:8]}")
    try:
        shutil.copy2(source, temporary)
        os.replace(temporary, destination)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def atomic_write_json(path: Path, value: dict[str, object]) -> None:
    payload = (json.dumps(value, indent=2, sort_keys=True) + "\n").encode("utf-8")
    if len(payload) > MAX_STATUS_BYTES:
        raise UpgradeError("upgrade_status_too_large")
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.new.{os.getpid()}.{uuid.uuid4().hex[:8]}")
    try:
        with temporary.open("xb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def status_path(data_root: Path) -> Path:
    return data_root / "v2-windows-shell" / "state" / "upgrade" / "windows-upgrade-status.json"


def ensure_no_incomplete_status(data_root: Path) -> None:
    path = status_path(data_root)
    if path.exists():
        record = load_json_file(path, max_bytes=MAX_STATUS_BYTES, code="upgrade_status_invalid")
        if record.get("status") in {"activating", "rolling_back", "operator_action_required"}:
            raise UpgradeError("prior_upgrade_requires_operator_action")


def snapshot_previous(data_root: Path, hub_config: Path, agent_config: Path, rollback: Path) -> dict[str, object]:
    binaries: dict[str, dict[str, object]] = {}
    for name in WINDOWS_BINARIES:
        source = data_root / "bin" / name
        present = source.is_file() and not source.is_symlink()
        item: dict[str, object] = {"present": present, "sha256": None}
        if present:
            digest = sha256_file(source)
            item["sha256"] = digest
            destination = rollback / "bin" / name
            atomic_copy(source, destination)
            if sha256_file(destination) != digest:
                raise UpgradeError("rollback_binary_copy_mismatch")
        binaries[name] = item
    for source, target in ((hub_config, rollback / "config" / "hub.json"), (agent_config, rollback / "config" / "agent.json")):
        require_regular(source, "active_config_missing")
        atomic_copy(source, target)
    return {"binaries": binaries, "hub_config_sha256": sha256_file(hub_config), "agent_config_sha256": sha256_file(agent_config)}


def stage_candidate(bundle_dir: Path, identity: CandidateIdentity, hub_config: Path, agent_config: Path, stage: Path) -> None:
    for name, expected in identity.files.items():
        destination = stage / "bin" / name
        atomic_copy(bundle_dir / "bin" / name, destination)
        if sha256_file(destination) != expected:
            raise UpgradeError("staged_binary_hash_mismatch")
    atomic_copy(hub_config, stage / "config" / "hub.json")
    atomic_copy(agent_config, stage / "config" / "agent.json")


def activate_staged(data_root: Path, stage: Path, identity: CandidateIdentity, active_hub_config: Path, active_agent_config: Path) -> None:
    for name, expected in identity.files.items():
        if sha256_file(stage / "bin" / name) != expected:
            raise UpgradeError("staged_binary_hash_mismatch")
    atomic_copy(stage / "config" / "hub.json", active_hub_config)
    atomic_copy(stage / "config" / "agent.json", active_agent_config)
    for name, expected in identity.files.items():
        destination = data_root / "bin" / name
        atomic_copy(stage / "bin" / name, destination)
        if sha256_file(destination) != expected:
            raise UpgradeError("activated_binary_hash_mismatch")


def restore_previous(data_root: Path, rollback: Path, previous: dict[str, object], active_hub_config: Path, active_agent_config: Path) -> None:
    binary_state = previous.get("binaries")
    if not isinstance(binary_state, dict):
        raise UpgradeError("rollback_evidence_invalid")
    for name in WINDOWS_BINARIES:
        item = binary_state.get(name)
        if not isinstance(item, dict) or not isinstance(item.get("present"), bool):
            raise UpgradeError("rollback_evidence_invalid")
        destination = data_root / "bin" / name
        if item["present"]:
            source = rollback / "bin" / name
            expected = item.get("sha256")
            if not isinstance(expected, str) or sha256_file(source) != expected:
                raise UpgradeError("rollback_binary_hash_mismatch")
            atomic_copy(source, destination)
            if sha256_file(destination) != expected:
                raise UpgradeError("rollback_binary_hash_mismatch")
        else:
            try:
                destination.unlink()
            except FileNotFoundError:
                pass
    atomic_copy(rollback / "config" / "hub.json", active_hub_config)
    atomic_copy(rollback / "config" / "agent.json", active_agent_config)
    if sha256_file(active_hub_config) != previous.get("hub_config_sha256") or sha256_file(active_agent_config) != previous.get("agent_config_sha256"):
        raise UpgradeError("rollback_config_hash_mismatch")


def new_status(transaction_id: str, identity: CandidateIdentity, previous: dict[str, object], rollback_name: str) -> dict[str, object]:
    now = time.time_ns() // 1_000_000
    return {
        "schema_version": STATUS_SCHEMA_VERSION,
        "transaction_id": transaction_id,
        "status": "activating",
        "phase": "staged",
        "started_at_ms": now,
        "updated_at_ms": now,
        "candidate": identity.as_dict(),
        "previous": previous,
        "rollback_asset": rollback_name,
        "result": None,
        "external_smoke_configured": False,
    }


def update_status(path: Path, record: dict[str, object], *, status: str | None = None, phase: str | None = None, result: str | None = None) -> None:
    if status is not None:
        record["status"] = status
    if phase is not None:
        record["phase"] = phase
    if result is not None:
        record["result"] = result
    record["updated_at_ms"] = max(time.time_ns() // 1_000_000, int(record["updated_at_ms"]))
    atomic_write_json(path, record)


def perform_upgrade(
    *,
    bundle_dir: Path,
    identity: CandidateIdentity,
    data_root: Path,
    active_hub_config: Path,
    active_agent_config: Path,
    candidate_hub_config: ReviewedConfig,
    candidate_agent_config: ReviewedConfig,
    controller: RuntimeController,
    stable_seconds: int,
    timeout_seconds: int,
    preflight_only: bool,
) -> dict[str, object]:
    ensure_no_incomplete_status(data_root)
    controller.validate()
    transaction_id = f"windows-upgrade-{time.time_ns() // 1_000_000}-{os.getpid()}-{uuid.uuid4().hex[:8]}"
    stage = data_root / "staging" / transaction_id
    rollback = data_root / "backup" / transaction_id
    stage_candidate(bundle_dir, identity, candidate_hub_config.path, candidate_agent_config.path, stage)
    previous = snapshot_previous(data_root, active_hub_config, active_agent_config, rollback)
    record = new_status(transaction_id, identity, previous, rollback.name)
    record["external_smoke_configured"] = bool(getattr(controller, "external_smoke_configured", False))
    evidence = status_path(data_root)
    if preflight_only:
        update_status(evidence, record, status="preflight_passed", phase="preflight", result="no_activation")
        return record

    runtime_touched = False
    try:
        update_status(evidence, record, phase="service_drain")
        runtime_touched = True
        controller.stop_pair(
            load_reviewed_config(active_hub_config, "hub", data_root),
            load_reviewed_config(active_agent_config, "agent", data_root),
        )
        update_status(evidence, record, phase="activate_pair")
        activate_staged(data_root, stage, identity, active_hub_config, active_agent_config)
        active_hub = load_reviewed_config(active_hub_config, "hub", data_root)
        active_agent = load_reviewed_config(active_agent_config, "agent", data_root)
        update_status(evidence, record, phase="hub_health")
        controller.start_hub()
        controller.wait_hub_healthy(active_hub, timeout_seconds)
        update_status(evidence, record, phase="agent_health")
        controller.start_agent()
        controller.wait_agent_stable(active_agent, stable_seconds, timeout_seconds)
        if bool(getattr(controller, "external_smoke_configured", False)):
            update_status(evidence, record, phase="external_smoke")
            controller.external_smoke()
        update_status(evidence, record, status="completed", phase="completed", result="upgraded")
        shutil.rmtree(stage, ignore_errors=True)
        return record
    except Exception as exc:
        failure = exc.code if isinstance(exc, UpgradeError) else "unexpected_upgrade_failure"
        if not runtime_touched:
            update_status(evidence, record, status="failed_preflight", phase="preflight", result=failure)
            raise UpgradeError(failure) from exc
        try:
            update_status(evidence, record, status="rolling_back", phase="rollback", result=failure)
            try:
                controller.stop_pair(
                    load_reviewed_config(active_hub_config, "hub", data_root),
                    load_reviewed_config(active_agent_config, "agent", data_root),
                )
            except Exception:
                pass
            restore_previous(data_root, rollback, previous, active_hub_config, active_agent_config)
            previous_hub = load_reviewed_config(active_hub_config, "hub", data_root)
            previous_agent = load_reviewed_config(active_agent_config, "agent", data_root)
            controller.start_hub()
            controller.wait_hub_healthy(previous_hub, timeout_seconds)
            controller.start_agent()
            controller.wait_agent_stable(previous_agent, min(stable_seconds, 5), timeout_seconds)
            update_status(evidence, record, status="rolled_back", phase="rollback_complete", result=failure)
            shutil.rmtree(stage, ignore_errors=True)
        except Exception as rollback_exc:
            rollback_failure = rollback_exc.code if isinstance(rollback_exc, UpgradeError) else "rollback_failed"
            update_status(evidence, record, status="operator_action_required", phase="rollback_failed", result=rollback_failure)
            raise UpgradeError("upgrade_and_rollback_failed") from rollback_exc
        raise UpgradeError(failure) from exc


def argument_value(arguments: tuple[str, ...], flag: str) -> str | None:
    for index, item in enumerate(arguments[:-1]):
        if item == flag:
            return arguments[index + 1]
    return None


def loopback_ports(config: ReviewedConfig) -> tuple[int, ...]:
    ports: list[int] = []
    for flag in ("--bind", "--mcp-bind"):
        endpoint = argument_value(config.arguments, flag)
        if endpoint is None:
            continue
        match = LOOPBACK_RE.fullmatch(endpoint)
        if match is None:
            raise UpgradeError("hub_health_endpoint_not_loopback")
        port = int(match.group(1))
        if not 1 <= port <= 65535:
            raise UpgradeError("hub_health_endpoint_invalid")
        ports.append(port)
    if not ports:
        raise UpgradeError("hub_health_endpoint_missing")
    return tuple(ports)


def windows_process_image(pid: int) -> Path | None:
    if os.name != "nt":
        return None
    PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.OpenProcess.restype = ctypes.c_void_p
    kernel32.QueryFullProcessImageNameW.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_wchar_p, ctypes.POINTER(ctypes.c_ulong)]
    handle = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        return None
    try:
        size = ctypes.c_ulong(32768)
        buffer = ctypes.create_unicode_buffer(size.value)
        if not kernel32.QueryFullProcessImageNameW(handle, 0, buffer, ctypes.byref(size)):
            return None
        return Path(buffer.value).resolve(strict=False)
    finally:
        kernel32.CloseHandle(handle)


class WindowsTaskController:
    def __init__(self, task_prefix: str, external_smoke_script: Path | None = None) -> None:
        if not SAFE_TASK_PREFIX_RE.fullmatch(task_prefix):
            raise UpgradeError("invalid_task_prefix")
        self.task_prefix = task_prefix
        self.external_smoke_script = external_smoke_script.resolve(strict=True) if external_smoke_script else None
        self.external_smoke_configured = self.external_smoke_script is not None

    def task(self, component: str) -> str:
        return f"{self.task_prefix}-{component}"

    @staticmethod
    def run(args: list[str], code: str, *, allow_failure: bool = False) -> subprocess.CompletedProcess[str]:
        try:
            result = subprocess.run(args, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=20, check=False)
        except (OSError, subprocess.SubprocessError) as exc:
            raise UpgradeError(code) from exc
        if result.returncode != 0 and not allow_failure:
            raise UpgradeError(code)
        return result

    def validate(self) -> None:
        if os.name != "nt":
            raise UpgradeError("windows_upgrade_requires_windows")
        for component in ("hub", "agent"):
            self.run(["schtasks.exe", "/Query", "/TN", self.task(component)], "scheduled_task_missing")
        if self.external_smoke_script is not None:
            require_regular(self.external_smoke_script, "external_smoke_script_invalid")

    def _end_task(self, component: str) -> None:
        self.run(["schtasks.exe", "/End", "/TN", self.task(component)], "scheduled_task_stop_failed", allow_failure=True)

    @staticmethod
    def _read_pid(config: ReviewedConfig) -> int | None:
        path = config.pid_file()
        if not path.exists():
            return None
        try:
            pid = int(path.read_text(encoding="ascii").strip())
        except (OSError, ValueError) as exc:
            raise UpgradeError("component_pid_file_invalid") from exc
        return pid if pid > 0 else None

    def _stop_child(self, config: ReviewedConfig) -> None:
        pid_file = config.pid_file()
        pid = self._read_pid(config)
        if pid is not None:
            image = windows_process_image(pid)
            if image is not None:
                if os.path.normcase(str(image)) != os.path.normcase(str(config.executable)):
                    raise UpgradeError("component_pid_identity_mismatch")
                self.run(["taskkill.exe", "/PID", str(pid), "/F"], "component_stop_failed", allow_failure=True)
                deadline = time.monotonic() + 5
                while windows_process_image(pid) is not None and time.monotonic() < deadline:
                    time.sleep(0.1)
                if windows_process_image(pid) is not None:
                    raise UpgradeError("component_stop_timeout")
        try:
            pid_file.unlink()
        except FileNotFoundError:
            pass
        time.sleep(0.5)
        replacement = self._read_pid(config)
        if replacement is not None and windows_process_image(replacement) is not None:
            raise UpgradeError("supervisor_restart_race")

    def stop_pair(self, hub: ReviewedConfig, agent: ReviewedConfig) -> None:
        self._end_task("agent")
        self._stop_child(agent)
        self._end_task("hub")
        self._stop_child(hub)

    def _start(self, component: str) -> None:
        self.run(["schtasks.exe", "/Run", "/TN", self.task(component)], "scheduled_task_start_failed")

    def start_hub(self) -> None:
        self._start("hub")

    def start_agent(self) -> None:
        self._start("agent")

    @staticmethod
    def _wait_stable(config: ReviewedConfig, stable_seconds: int, timeout_seconds: int) -> None:
        deadline = time.monotonic() + timeout_seconds
        first_pid: int | None = None
        stable_since: float | None = None
        while time.monotonic() < deadline:
            pid = WindowsTaskController._read_pid(config)
            if pid is not None:
                image = windows_process_image(pid)
                if image is not None and os.path.normcase(str(image)) == os.path.normcase(str(config.executable)):
                    if first_pid is None:
                        first_pid = pid
                        stable_since = time.monotonic()
                    elif pid != first_pid:
                        raise UpgradeError(f"{config.component}_restarted_during_health_gate")
                    elif stable_since is not None and time.monotonic() - stable_since >= stable_seconds:
                        return
            time.sleep(0.25)
        raise UpgradeError(f"{config.component}_health_timeout")

    def wait_hub_healthy(self, hub: ReviewedConfig, timeout_seconds: int) -> None:
        self._wait_stable(hub, min(2, timeout_seconds), timeout_seconds)
        for port in loopback_ports(hub):
            deadline = time.monotonic() + timeout_seconds
            while time.monotonic() < deadline:
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=0.5):
                        break
                except OSError:
                    time.sleep(0.25)
            else:
                raise UpgradeError("hub_listener_health_timeout")

    def wait_agent_stable(self, agent: ReviewedConfig, stable_seconds: int, timeout_seconds: int) -> None:
        self._wait_stable(agent, stable_seconds, timeout_seconds)

    def external_smoke(self) -> None:
        if self.external_smoke_script is None:
            return
        powershell = os.path.join(os.environ.get("SystemRoot", r"C:\Windows"), "System32", "WindowsPowerShell", "v1.0", "powershell.exe")
        self.run([powershell, "-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", str(self.external_smoke_script)], "external_route_smoke_failed")


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--bundle-dir", type=Path, required=True)
    p.add_argument("--data-root", type=Path, required=True)
    p.add_argument("--active-hub-config", type=Path, required=True)
    p.add_argument("--active-agent-config", type=Path, required=True)
    p.add_argument("--candidate-hub-config", type=Path, required=True)
    p.add_argument("--candidate-agent-config", type=Path, required=True)
    p.add_argument("--task-prefix", default="cumg-v2-windows")
    p.add_argument("--stable-seconds", type=int, default=10)
    p.add_argument("--health-timeout-seconds", type=int, default=45)
    p.add_argument("--external-smoke-script", type=Path)
    p.add_argument("--preflight-only", action="store_true")
    return p


def main(argv: list[str] | None = None) -> int:
    args = parser().parse_args(argv)
    try:
        if os.name != "nt":
            raise UpgradeError("windows_upgrade_requires_windows")
        if not 1 <= args.stable_seconds <= 120 or not 5 <= args.health_timeout_seconds <= 300:
            raise UpgradeError("invalid_health_window")
        data_root = args.data_root.resolve(strict=True)
        bundle_dir = args.bundle_dir.resolve(strict=True)
        identity = verify_candidate(bundle_dir)
        hub, agent = preflight_configs(bundle_dir, data_root, args.candidate_hub_config, args.candidate_agent_config)
        controller = WindowsTaskController(args.task_prefix, args.external_smoke_script)
        record = perform_upgrade(
            bundle_dir=bundle_dir,
            identity=identity,
            data_root=data_root,
            active_hub_config=args.active_hub_config.resolve(strict=True),
            active_agent_config=args.active_agent_config.resolve(strict=True),
            candidate_hub_config=hub,
            candidate_agent_config=agent,
            controller=controller,
            stable_seconds=args.stable_seconds,
            timeout_seconds=args.health_timeout_seconds,
            preflight_only=args.preflight_only,
        )
    except UpgradeError as exc:
        print(f"REFUSED reason={exc.code}", file=sys.stderr)
        return 2
    except Exception:
        print("REFUSED reason=unexpected_upgrade_failure", file=sys.stderr)
        return 2
    print(f"WINDOWS_UPGRADE status={record['status']} package={identity.package_version} source_commit={identity.source_commit}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
