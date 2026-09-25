#!/usr/bin/env python3
"""Verified backup/restore for the reviewed CUMG V2 single-Mac profile."""
from __future__ import annotations

import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from typing import Iterator, Sequence

BACKUP_MANIFEST = "backup-manifest.json"
BACKUP_SCHEMA_VERSION = 1
STAGE_MARKER = "restore-stage.json"
STAGE_SCHEMA_VERSION = 1
BACKUP_PROFILE = "single-mac-v2-verified-v1"
RUNTIME_MANIFEST_SCHEMA_VERSION = 4
HANDOFF_MANIFEST_SCHEMA_VERSION = 1
MUTATION_AUTHORITY_SCHEMA_VERSION = 1
SUPPORTED_HUB_M1_STATE_SCHEMAS = {5, 6, 7}
SUPPORTED_AGENT_M1_STATE_SCHEMAS = {5, 6}
HUB_PERSISTENCE_FENCE_SCHEMA_VERSION = 1
MAX_MANIFEST_BYTES = 2 * 1024 * 1024
MAX_STATE_BYTES = 1024 * 1024
MAX_FILE_BYTES = 256 * 1024 * 1024
MAX_FILES = 20000
MAX_TOTAL_BYTES = 2 * 1024 * 1024 * 1024
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")
CHECKPOINT_RE = {
    "hub": re.compile(r"^hub-([0-9]{20})\.json$"),
    "agent": re.compile(r"^agent-([0-9]{20})\.json$"),
}
LABELS = (
    "com.github.git-ksk.cumg-v2-grant-signer",
    "com.github.git-ksk.cumg-v2-hub",
    "com.github.git-ksk.cumg-v2-agent",
)
PLISTS = {
    "com.github.git-ksk.cumg-v2-grant-signer": "com.github.git-ksk.cumg-v2-grant-signer.plist",
    "com.github.git-ksk.cumg-v2-hub": "com.github.git-ksk.cumg-v2-hub.plist",
    "com.github.git-ksk.cumg-v2-agent": "com.github.git-ksk.cumg-v2-agent.plist",
}
CONFLICTING_LABELS = (
    *LABELS,
    "com.sawadakousuke.cumg-v2-hub",
    "com.sawadakousuke.cumg-v2-agent",
    "com.sawadakousuke.computer-use-mcp-gateway",
)
HUB_DURABLE_FILES = {"recovery-public-key.p256", "recovery-webauthn-verifier.json"}
AGENT_DURABLE_FILES = {
    "recovery-challenge.json",
    "recovery-authorization.json",
    "recovery-resolved.json",
}
STATE_EXCLUDED_FILES = {".cumg-v2-state.lock", ".DS_Store"}
AGENT_EXCLUDED_DIRS = {
    "browser-upload-staging",
    "browser-download-staging",
    "playwright-artifacts",
}

class BackupRestoreError(RuntimeError):
    def __init__(self, code: str) -> None:
        super().__init__(code)
        self.code = code

def _next_action(code: str) -> str:
    if code == "effectful_service_loaded":
        return "stop_reviewed_services"
    if code == "mutation_authority_busy":
        return "stop_effectful_writer_and_retry"
    if code in {"backup_manifest_digest_mismatch", "expected_manifest_digest_invalid"}:
        return "use_external_manifest_digest"
    if code in {
        "checkpoint_schema_unsupported",
        "runtime_manifest_schema_mismatch",
        "backup_runtime_identity_mismatch",
        "backup_handoff_identity_mismatch",
        "handoff_manifest_schema_mismatch",
        "handoff_cumg_identity_mismatch",
    }:
        return "use_version_paired_reviewed_artifact"
    if code in {
        "restore_target_not_clean",
        "restore_launchagent_target_not_clean",
        "restore_profile_path_mismatch",
        "restore_stage_exists",
    }:
        return "use_clean_exact_profile"
    if code in {
        "private_file_permissions",
        "private_directory_permissions",
        "group_or_world_writable_file",
        "group_or_world_writable_directory",
        "symlinked_authority_reference",
        "external_authority_reference",
        "external_handoff_durable_reference",
    }:
        return "repair_reviewed_profile_or_reprovision"
    if code == "unknown_state_entry":
        return "review_new_durable_state_before_backup"
    if code.startswith("post_restore_"):
        return "keep_non_effectful_and_inspect_status_doctor"
    return "inspect_backup_restore_runbook"

def _fail(code: str) -> None:
    raise BackupRestoreError(code)

def _canonical_json(value: object) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n").encode("utf-8")

def _sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()

def _sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()

def _safe_rel(value: str) -> PurePosixPath:
    if not value or "\\" in value or ":" in value:
        _fail("unsafe_relative_path")
    path = PurePosixPath(value)
    if path.is_absolute() or path.as_posix() != value or any(p in {"", ".", ".."} for p in path.parts):
        _fail("unsafe_relative_path")
    return path

def _lstat_regular(
    path: Path,
    *,
    private: bool = False,
    max_bytes: int = MAX_FILE_BYTES,
    allow_empty: bool = False,
) -> os.stat_result:
    try:
        info = path.lstat()
    except FileNotFoundError:
        _fail("required_file_missing")
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        _fail("unsafe_regular_file")
    if info.st_size > max_bytes or (info.st_size == 0 and not allow_empty):
        _fail("invalid_file_size")
    if info.st_mode & 0o022:
        _fail("group_or_world_writable_file")
    if private and info.st_mode & 0o077:
        _fail("private_file_permissions")
    return info

def _lstat_directory(path: Path, *, private: bool = False) -> os.stat_result:
    try:
        info = path.lstat()
    except FileNotFoundError:
        _fail("required_directory_missing")
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
        _fail("unsafe_directory")
    if info.st_mode & 0o022:
        _fail("group_or_world_writable_directory")
    if private and info.st_mode & 0o077:
        _fail("private_directory_permissions")
    return info

def _read_json(path: Path, *, max_bytes: int = MAX_MANIFEST_BYTES) -> dict:
    info = _lstat_regular(path, max_bytes=max_bytes)
    try:
        raw = path.read_bytes()
        if len(raw) != info.st_size:
            _fail("file_changed_while_reading")
        value = json.loads(raw)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError):
        _fail("invalid_json")
    if not isinstance(value, dict):
        _fail("invalid_json_root")
    return value

def _is_within(path: Path, root: Path) -> bool:
    try:
        path.resolve(strict=False).relative_to(root.resolve(strict=False))
        return True
    except ValueError:
        return False

def _exact_root_path(path: Path, root: Path) -> Path:
    if not path.is_absolute() or not _is_within(path, root):
        _fail("external_authority_reference")
    try:
        relative = path.relative_to(root)
    except ValueError:
        _fail("external_authority_reference")
    current = root
    try:
        for component in relative.parts:
            current = current / component
            info = current.lstat()
            if stat.S_ISLNK(info.st_mode):
                _fail("symlinked_authority_reference")
        resolved = path.resolve(strict=True)
        resolved_root = root.resolve(strict=True)
    except FileNotFoundError:
        _fail("required_file_missing")
    if not _is_within(resolved, resolved_root):
        _fail("symlinked_authority_reference")
    return resolved

def _file_record(path: Path, bundle_path: str) -> dict[str, object]:
    _safe_rel(bundle_path)
    info = _lstat_regular(path)
    return {
        "path": bundle_path,
        "size": info.st_size,
        "mode": stat.S_IMODE(info.st_mode),
        "sha256": _sha256_file(path),
    }

def _copy_exact(source: Path, destination: Path, mode: int) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    if destination.exists() or destination.is_symlink():
        _fail("destination_not_clean")
    shutil.copyfile(source, destination)
    os.chmod(destination, mode)

def _harden_private_tree(root: Path) -> None:
    _lstat_directory(root, private=False)
    os.chmod(root, 0o700)
    for path in root.rglob("*"):
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode):
            _fail("private_tree_contains_symlink")
        if stat.S_ISDIR(info.st_mode):
            os.chmod(path, 0o700)

def _write_private(path: Path, data: bytes, *, allow_empty: bool = False) -> None:
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    fd = os.open(path, flags, 0o600)
    try:
        with os.fdopen(fd, "wb", closefd=False) as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
    finally:
        os.close(fd)
    if not allow_empty and not data:
        _fail("unexpected_empty_file")

def _ensure_macos() -> None:
    if sys.platform != "darwin":
        _fail("macos_required")

def _default_domain() -> str:
    return f"gui/{os.getuid()}"

def _service_loaded(label: str, domain: str, runner=subprocess.run) -> bool:
    result = runner(
        ["launchctl", "print", f"{domain}/{label}"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return result.returncode == 0

def require_services_stopped(domain: str, runner=subprocess.run) -> None:
    for label in CONFLICTING_LABELS:
        if _service_loaded(label, domain, runner=runner):
            _fail("effectful_service_loaded")

def _read_authority(directory: Path) -> dict[str, object]:
    _lstat_directory(directory, private=True)
    data = _read_json(directory / "mutation-authority.json", max_bytes=1024)
    if set(data) != {"schema_version", "owner", "epoch"}:
        _fail("mutation_authority_schema_mismatch")
    if data["schema_version"] != MUTATION_AUTHORITY_SCHEMA_VERSION:
        _fail("mutation_authority_schema_mismatch")
    if data["owner"] not in {"v1", "v2"}:
        _fail("mutation_authority_owner_invalid")
    if not isinstance(data["epoch"], int) or isinstance(data["epoch"], bool) or data["epoch"] <= 0:
        _fail("mutation_authority_epoch_invalid")
    return data

@contextmanager
def hold_mutation_authority(directory: Path) -> Iterator[dict[str, object]]:
    _lstat_directory(directory, private=True)
    lock_path = directory / "mutation-authority.lock"
    info = _lstat_regular(lock_path, private=True, max_bytes=1024, allow_empty=True)
    flags = os.O_RDWR | (getattr(os, "O_NOFOLLOW", 0))
    fd = os.open(lock_path, flags)
    try:
        current = os.fstat(fd)
        if not stat.S_ISREG(current.st_mode) or current.st_ino != info.st_ino:
            _fail("mutation_authority_lock_replaced")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            _fail("mutation_authority_busy")
        yield _read_authority(directory)
    finally:
        try:
            fcntl.flock(fd, fcntl.LOCK_UN)
        except OSError:
            pass
        os.close(fd)

def _codesign_verify(path: Path, runner=subprocess.run) -> None:
    result = runner(
        ["codesign", "--verify", "--strict", str(path)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if result.returncode != 0:
        _fail("codesign_verification_failed")

def validate_runtime_manifest(root: Path, *, verify_codesign: bool = True) -> dict[str, object]:
    manifest_path = root / "runtime-manifest.json"
    data = _read_json(manifest_path, max_bytes=256 * 1024)
    expected = {
        "schema_version", "hub_agent_schema_version", "control_schema_version",
        "capability_schema_version", "source_commit", "package_version", "binaries",
    }
    if set(data) != expected or data["schema_version"] != RUNTIME_MANIFEST_SCHEMA_VERSION:
        _fail("runtime_manifest_schema_mismatch")
    source_commit = data["source_commit"]
    if not isinstance(source_commit, str) or not COMMIT_RE.fullmatch(source_commit):
        _fail("runtime_source_commit_invalid")
    for key in ("hub_agent_schema_version", "control_schema_version", "capability_schema_version"):
        if not isinstance(data[key], int) or isinstance(data[key], bool) or data[key] <= 0:
            _fail("runtime_schema_identity_invalid")
    if not isinstance(data["package_version"], str) or not data["package_version"]:
        _fail("runtime_package_version_invalid")
    records = data["binaries"]
    if not isinstance(records, list) or not records:
        _fail("runtime_binary_records_invalid")
    seen: set[str] = set()
    files: list[Path] = [manifest_path]
    for record in records:
        if not isinstance(record, dict) or set(record) != {"name", "sha256"}:
            _fail("runtime_binary_record_invalid")
        name, digest = record["name"], record["sha256"]
        if (
            not isinstance(name, str) or not name or Path(name).name != name or name in seen
            or not isinstance(digest, str) or not SHA256_RE.fullmatch(digest)
        ):
            _fail("runtime_binary_record_invalid")
        seen.add(name)
        path = root / "bin" / name
        info = _lstat_regular(path)
        if info.st_mode & 0o111 == 0:
            _fail("runtime_binary_not_executable")
        if _sha256_file(path) != digest:
            _fail("runtime_binary_digest_mismatch")
        if verify_codesign:
            _codesign_verify(path)
        files.append(path)
    return {
        "schema_version": data["schema_version"],
        "hub_agent_schema_version": data["hub_agent_schema_version"],
        "control_schema_version": data["control_schema_version"],
        "capability_schema_version": data["capability_schema_version"],
        "source_commit": source_commit,
        "package_version": data["package_version"],
        "binary_names": sorted(seen),
        "files": files,
    }


def _parse_managed_env(path: Path) -> dict[str, str]:
    _lstat_regular(path, private=True, max_bytes=256 * 1024)
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        _fail("managed_runtime_env_invalid")
    result: dict[str, str] = {}
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if "=" not in line:
            _fail("managed_runtime_env_invalid")
        key, value = line.split("=", 1)
        if not re.fullmatch(r"[A-Z0-9_]{1,128}", key) or key in result:
            _fail("managed_runtime_env_invalid")
        result[key] = value
    return result

def validate_handoff_generation(
    generation: Path,
    expected_cumg_commit: str,
) -> tuple[dict[str, object], list[Path]]:
    _lstat_directory(generation, private=True)
    manifest_path = generation / "runtime-generation-manifest.json"
    data = _read_json(manifest_path, max_bytes=1024 * 1024)
    if set(data) != {"schema_version", "cumg_source_commit", "handoff_source_commit", "files"}:
        _fail("handoff_manifest_schema_mismatch")
    if data["schema_version"] != HANDOFF_MANIFEST_SCHEMA_VERSION:
        _fail("handoff_manifest_schema_mismatch")
    if data["cumg_source_commit"] != expected_cumg_commit:
        _fail("handoff_cumg_identity_mismatch")
    handoff_commit = data["handoff_source_commit"]
    if not isinstance(handoff_commit, str) or not COMMIT_RE.fullmatch(handoff_commit):
        _fail("handoff_source_commit_invalid")
    expected_name = f"runtime-{expected_cumg_commit[:12]}-{handoff_commit[:12]}"
    if generation.name != expected_name:
        _fail("handoff_generation_name_mismatch")
    records = data["files"]
    if not isinstance(records, list) or not records:
        _fail("handoff_file_records_invalid")
    expected: dict[str, str] = {}
    for record in records:
        if not isinstance(record, dict) or set(record) != {"path", "sha256"}:
            _fail("handoff_file_record_invalid")
        rel, digest = record["path"], record["sha256"]
        if not isinstance(rel, str) or not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
            _fail("handoff_file_record_invalid")
        _safe_rel(rel)
        if rel == "runtime-generation-manifest.json" or rel in expected:
            _fail("handoff_file_record_invalid")
        expected[rel] = digest
    actual: dict[str, Path] = {}
    count = total = 0
    for base, directories, files in os.walk(generation, topdown=True, followlinks=False):
        base_path = Path(base)
        for name in directories:
            candidate = base_path / name
            info = candidate.lstat()
            if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
                _fail("handoff_unsafe_directory")
            if info.st_mode & 0o022:
                _fail("handoff_weak_directory")
        for name in files:
            candidate = base_path / name
            if candidate == manifest_path:
                continue
            info = _lstat_regular(candidate)
            count += 1
            total += info.st_size
            if count > MAX_FILES or total > MAX_TOTAL_BYTES:
                _fail("handoff_tree_too_large")
            actual[candidate.relative_to(generation).as_posix()] = candidate
    if set(actual) != set(expected):
        _fail("handoff_file_set_mismatch")
    for rel, candidate in actual.items():
        if _sha256_file(candidate) != expected[rel]:
            _fail("handoff_file_digest_mismatch")
    required = {
        "v2_handoff_runtime.mjs",
        "handoff-root/dist/index.js",
        "handoff-root/package.json",
        "handoff-root/package-lock.json",
    }
    if not required.issubset(actual):
        _fail("handoff_required_file_missing")
    return (
        {
            "schema_version": data["schema_version"],
            "cumg_source_commit": data["cumg_source_commit"],
            "handoff_source_commit": handoff_commit,
            "generation_name": generation.name,
        },
        [manifest_path, *actual.values()],
    )

def collect_handoff(root: Path, runtime: dict[str, object]) -> tuple[dict[str, object], list[Path]]:
    env_path = root / "v2/handoff/managed-runtime.env"
    env = _parse_managed_env(env_path)
    raw_root = env.get("CUMG_V2_HANDOFF_ROOT")
    if raw_root is None:
        _fail("handoff_root_missing")
    handoff_root = Path(raw_root)
    if handoff_root.name != "handoff-root":
        _fail("handoff_root_invalid")
    generation = handoff_root.parent
    _exact_root_path(handoff_root, root)
    if generation.parent != root / "v2/handoff":
        _fail("handoff_generation_outside_reviewed_root")
    identity, generation_files = validate_handoff_generation(
        generation, str(runtime["source_commit"])
    )
    extra_files: list[Path] = []
    for key in ("CUMG_V2_HANDOFF_CHECKPOINT_KEY_FILE", "CUMG_V2_HANDOFF_CHECKPOINT_FILE"):
        raw = env.get(key)
        if raw is None:
            continue
        candidate = Path(raw)
        if not candidate.is_absolute() or not _is_within(candidate, root):
            _fail("external_handoff_durable_reference")
        if candidate.exists():
            resolved = _exact_root_path(candidate, root)
            _lstat_regular(resolved, private=True)
            extra_files.append(resolved)
        elif key.endswith("_KEY_FILE"):
            _fail("handoff_checkpoint_key_missing")
    identity["active_generation_rel"] = generation.relative_to(root).as_posix()
    identity["managed_env_rel"] = env_path.relative_to(root).as_posix()
    identity["checkpoint_key_configured"] = "CUMG_V2_HANDOFF_CHECKPOINT_KEY_FILE" in env
    identity["checkpoint_configured"] = "CUMG_V2_HANDOFF_CHECKPOINT_FILE" in env
    return identity, [env_path, *generation_files, *extra_files]

def _load_plist(path: Path) -> dict:
    _lstat_regular(path, private=True, max_bytes=256 * 1024)
    try:
        with path.open("rb") as handle:
            value = plistlib.load(handle)
    except (OSError, plistlib.InvalidFileException):
        _fail("launchagent_plist_invalid")
    if not isinstance(value, dict):
        _fail("launchagent_plist_invalid")
    return value

def collect_launchd_profile(
    root: Path, launch_agent_dir: Path
) -> tuple[dict[str, object], list[Path], list[Path]]:
    _lstat_directory(launch_agent_dir)
    plists: list[Path] = []
    referenced: list[Path] = []
    by_label: dict[str, dict] = {}
    for label in LABELS:
        path = launch_agent_dir / PLISTS[label]
        data = _load_plist(path)
        if data.get("Label") != label:
            _fail("launchagent_label_mismatch")
        args = data.get("ProgramArguments")
        env = data.get("EnvironmentVariables")
        if not isinstance(args, list) or not args or not all(isinstance(x, str) for x in args):
            _fail("launchagent_program_arguments_invalid")
        if not isinstance(env, dict) or not all(isinstance(k, str) and isinstance(v, str) for k, v in env.items()):
            _fail("launchagent_environment_invalid")
        program = Path(args[0])
        if not program.is_absolute() or not _is_within(program, root / "bin"):
            _fail("launchagent_program_outside_runtime")
        _exact_root_path(program, root)
        for key, value in env.items():
            if not key.endswith("_FILE"):
                continue
            candidate = Path(value)
            if not candidate.is_absolute() or not _is_within(candidate, root):
                _fail("external_authority_reference")
            resolved = _exact_root_path(candidate, root)
            _lstat_regular(resolved, private=True)
            referenced.append(resolved)
        plists.append(path)
        by_label[label] = data

    hub_env = by_label["com.github.git-ksk.cumg-v2-hub"]["EnvironmentVariables"]
    agent_env = by_label["com.github.git-ksk.cumg-v2-agent"]["EnvironmentVariables"]
    signer_env = by_label["com.github.git-ksk.cumg-v2-grant-signer"]["EnvironmentVariables"]
    expected_exact = {
        agent_env.get("CUMG_V2_STATE_DIR"): root / "v2/state/agent",
        agent_env.get("CUMG_MUTATION_AUTHORITY_DIR"): root / "mutation-authority",
        agent_env.get("CUMG_V2_HANDOFF_RUNTIME_ENV_FILE"): root / "v2/handoff/managed-runtime.env",
        hub_env.get("CUMG_V2_HUB_STATE_DIR"): root / "v2/state/hub",
        hub_env.get("CUMG_V2_STATUS_INSTALL_ROOT"): root,
    }
    for raw, expected in expected_exact.items():
        if raw is None or Path(raw) != expected:
            _fail("launchagent_reviewed_path_mismatch")
    run_root_raw = hub_env.get("CUMG_V2_STATUS_RUN_ROOT")
    if not isinstance(run_root_raw, str) or not Path(run_root_raw).is_absolute():
        _fail("launchagent_run_root_invalid")
    run_root = Path(run_root_raw)
    if _is_within(run_root, root):
        _fail("run_root_must_be_ephemeral")
    signer_socket = signer_env.get("CUMG_V2_GRANT_SIGNER_SOCKET")
    if not isinstance(signer_socket, str) or Path(signer_socket).parent != run_root:
        _fail("launchagent_run_root_mismatch")
    cua_command = agent_env.get("CUMG_V2_CUA_COMMAND")
    cua_version = agent_env.get("CUMG_V2_CUA_BACKEND_VERSION")
    handoff_socket = hub_env.get("CUMG_V2_HANDOFF_CONTROL_SOCKET")
    if not isinstance(cua_command, str) or not Path(cua_command).is_absolute():
        _fail("cua_command_invalid")
    if not isinstance(cua_version, str) or not cua_version:
        _fail("cua_version_invalid")
    if not isinstance(handoff_socket, str) or not Path(handoff_socket).is_absolute():
        _fail("handoff_control_socket_invalid")
    optional_recovery = root / "v2/secrets/recovery.sealed"
    if optional_recovery.exists():
        resolved_recovery = _exact_root_path(optional_recovery, root)
        _lstat_regular(resolved_recovery, private=True)
        referenced.append(resolved_recovery)
    return (
        {
            "run_root": str(run_root),
            "cua_command": cua_command,
            "cua_version": cua_version,
            "handoff_control_socket": handoff_socket,
            "labels": list(LABELS),
            "plist_files": [PLISTS[label] for label in LABELS],
        },
        plists,
        referenced,
    )

def _state_entry_excluded(role: str, name: str, is_dir: bool) -> bool:
    if name in STATE_EXCLUDED_FILES:
        return True
    if name.startswith(f".{role}.pending-") and name.endswith(".tmp"):
        return True
    if role == "hub" and name.startswith("recovery-public-key.p256.pre-rotate-"):
        return True
    if role == "agent" and is_dir and name in AGENT_EXCLUDED_DIRS:
        return True
    return False

def _checkpoint_summary(path: Path, role: str, sequence: int) -> dict[str, object]:
    data = _read_json(path, max_bytes=MAX_STATE_BYTES)
    schema = data.get("schema_version")
    supported = (
        SUPPORTED_HUB_M1_STATE_SCHEMAS
        if role == "hub"
        else SUPPORTED_AGENT_M1_STATE_SCHEMAS
    )
    if schema not in supported:
        _fail("checkpoint_schema_unsupported")
    summary: dict[str, object] = {
        "latest_sequence": sequence,
        "schema_version": schema,
        "latest_sha256": _sha256_file(path),
    }
    if role == "hub":
        fence = data.get("durable_fence")
        if schema == 7:
            if (
                not isinstance(fence, dict)
                or set(fence) != {"schema_version", "revision", "writer_epoch"}
                or fence.get("schema_version") != HUB_PERSISTENCE_FENCE_SCHEMA_VERSION
                or not isinstance(fence.get("revision"), int)
                or isinstance(fence.get("revision"), bool)
                or fence["revision"] <= 0
                or not isinstance(fence.get("writer_epoch"), int)
                or isinstance(fence.get("writer_epoch"), bool)
                or fence["writer_epoch"] <= 0
            ):
                _fail("hub_writer_fence_invalid")
            summary["state_revision"] = fence["revision"]
            summary["writer_epoch"] = fence["writer_epoch"]
        elif fence is not None:
            _fail("hub_writer_fence_invalid")
        execution = data.get("execution")
        if not isinstance(execution, dict):
            _fail("hub_execution_state_missing")
        quarantines = execution.get("quarantines")
        if not isinstance(quarantines, list):
            _fail("hub_quarantine_state_invalid")
        summary["quarantine_count"] = len(quarantines)
        summary["execution_sha256"] = _sha256_bytes(_canonical_json(execution))
        for key in (
            "operations", "retirements", "resolutions", "auto_resolutions",
            "mutation_resume_barriers", "mutation_resumes",
        ):
            value = execution.get(key)
            if not isinstance(value, list):
                _fail("hub_execution_state_invalid")
            summary[f"{key}_count"] = len(value)
    return summary

def collect_state(root: Path, role: str) -> tuple[dict[str, object], list[Path]]:
    directory = root / f"v2/state/{role}"
    _lstat_directory(directory, private=True)
    allowed_named = HUB_DURABLE_FILES if role == "hub" else AGENT_DURABLE_FILES
    checkpoints: list[tuple[int, Path]] = []
    durable: list[Path] = []
    for entry in directory.iterdir():
        info = entry.lstat()
        is_dir = stat.S_ISDIR(info.st_mode) and not stat.S_ISLNK(info.st_mode)
        if _state_entry_excluded(role, entry.name, is_dir):
            continue
        match = CHECKPOINT_RE[role].fullmatch(entry.name)
        if match is not None:
            _lstat_regular(entry, private=True, max_bytes=MAX_STATE_BYTES)
            checkpoints.append((int(match.group(1)), entry))
            durable.append(entry)
            continue
        if entry.name in allowed_named:
            _lstat_regular(entry, private=True, max_bytes=MAX_STATE_BYTES)
            durable.append(entry)
            continue
        _fail("unknown_state_entry")
    if not checkpoints:
        _fail("checkpoint_missing")
    checkpoints.sort(key=lambda item: item[0])
    summary = _checkpoint_summary(checkpoints[-1][1], role, checkpoints[-1][0])
    summary["checkpoint_count"] = len(checkpoints)
    return summary, durable

def _semantic_validate_hub(root: Path, runner=subprocess.run) -> None:
    result = runner(
        [str(root / "bin/v2_maint"), "inspect-quarantine", "--state-dir", str(root / "v2/state/hub")],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
        text=True,
    )
    if result.returncode != 0:
        _fail("hub_checkpoint_semantic_validation_failed")
    try:
        parsed = json.loads(result.stdout)
    except (TypeError, json.JSONDecodeError):
        _fail("hub_checkpoint_semantic_validation_failed")
    if not isinstance(parsed, dict):
        _fail("hub_checkpoint_semantic_validation_failed")

def _dedupe_paths(paths: Sequence[Path]) -> list[Path]:
    result: list[Path] = []
    seen: set[Path] = set()
    for path in paths:
        resolved = path.resolve(strict=True)
        if resolved in seen:
            continue
        seen.add(resolved)
        result.append(path)
    return result

def build_inventory(
    root: Path,
    launch_agent_dir: Path,
    *,
    verify_codesign: bool = True,
    semantic_validate: bool = True,
) -> tuple[dict[str, object], dict[str, Path]]:
    _lstat_directory(root, private=True)
    runtime = validate_runtime_manifest(root, verify_codesign=verify_codesign)
    handoff, handoff_files = collect_handoff(root, runtime)
    hub_state, hub_files = collect_state(root, "hub")
    agent_state, agent_files = collect_state(root, "agent")
    operator, plist_files, referenced_files = collect_launchd_profile(root, launch_agent_dir)
    authority = _read_authority(root / "mutation-authority")
    if semantic_validate:
        _semantic_validate_hub(root)
    root_files = _dedupe_paths([
        *runtime["files"], *handoff_files, *hub_files, *agent_files,
        root / "mutation-authority/mutation-authority.json", *referenced_files,
    ])
    sources: dict[str, Path] = {}
    canonical_root = root.resolve(strict=True)
    for path in root_files:
        try:
            rel = path.resolve(strict=True).relative_to(canonical_root).as_posix()
        except ValueError:
            _fail("inventory_file_outside_install_root")
        sources[f"payload/{rel}"] = path
    for path in plist_files:
        sources[f"launchd/{path.name}"] = path
    if len(sources) > MAX_FILES:
        _fail("backup_file_count_exceeded")
    total = sum(_lstat_regular(path).st_size for path in sources.values())
    if total > MAX_TOTAL_BYTES:
        _fail("backup_total_bytes_exceeded")
    metadata = {
        "schema_version": BACKUP_SCHEMA_VERSION,
        "profile": BACKUP_PROFILE,
        "install_root": str(root),
        "launch_agent_dir": str(launch_agent_dir),
        "runtime": {key: value for key, value in runtime.items() if key != "files"},
        "handoff": handoff,
        "hub_state": hub_state,
        "agent_state": agent_state,
        "mutation_authority": authority,
        "operator": operator,
        "excluded_policy": {
            "state_locks": True,
            "pending_checkpoints": True,
            "browser_staging": True,
            "playwright_artifacts": True,
            "stale_recovery_key_copies": True,
            "runtime_logs_sockets_caches": True,
            "upgrade_rollback_assets": True,
        },
    }
    return metadata, sources


def _manifest_with_records(metadata: dict[str, object], sources: dict[str, Path]) -> dict[str, object]:
    manifest = dict(metadata)
    manifest["created_at_ms"] = int(time.time() * 1000)
    manifest["files"] = [_file_record(source, rel) for rel, source in sorted(sources.items())]
    return manifest

def _validate_manifest_shape(manifest: dict) -> None:
    expected = {
        "schema_version", "profile", "created_at_ms", "install_root", "launch_agent_dir",
        "runtime", "handoff", "hub_state", "agent_state", "mutation_authority",
        "operator", "excluded_policy", "files",
    }
    if set(manifest) != expected:
        _fail("backup_manifest_schema_mismatch")
    if manifest["schema_version"] != BACKUP_SCHEMA_VERSION or manifest["profile"] != BACKUP_PROFILE:
        _fail("backup_manifest_schema_mismatch")
    if not isinstance(manifest["created_at_ms"], int) or manifest["created_at_ms"] <= 0:
        _fail("backup_manifest_time_invalid")
    if not isinstance(manifest["install_root"], str) or not Path(manifest["install_root"]).is_absolute():
        _fail("backup_manifest_install_root_invalid")
    if not isinstance(manifest["launch_agent_dir"], str) or not Path(manifest["launch_agent_dir"]).is_absolute():
        _fail("backup_manifest_launchagent_root_invalid")
    if not isinstance(manifest["files"], list) or not manifest["files"]:
        _fail("backup_manifest_files_invalid")

def _verify_file_records(backup: Path, manifest: dict) -> None:
    expected: dict[str, dict] = {}
    for record in manifest["files"]:
        if not isinstance(record, dict) or set(record) != {"path", "size", "mode", "sha256"}:
            _fail("backup_file_record_invalid")
        rel = record["path"]
        if not isinstance(rel, str):
            _fail("backup_file_record_invalid")
        _safe_rel(rel)
        if rel in expected:
            _fail("backup_file_record_duplicate")
        if (
            not isinstance(record["size"], int) or record["size"] <= 0 or record["size"] > MAX_FILE_BYTES
            or not isinstance(record["mode"], int) or record["mode"] < 0 or record["mode"] > 0o7777
            or not isinstance(record["sha256"], str) or not SHA256_RE.fullmatch(record["sha256"])
        ):
            _fail("backup_file_record_invalid")
        expected[rel] = record
    actual: set[str] = set()
    for path in backup.rglob("*"):
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode):
            _fail("backup_contains_symlink")
        if path.is_dir():
            if info.st_mode & 0o022:
                _fail("backup_weak_directory")
            continue
        if not stat.S_ISREG(info.st_mode):
            _fail("backup_contains_special_file")
        if path == backup / BACKUP_MANIFEST:
            continue
        actual.add(path.relative_to(backup).as_posix())
    if actual != set(expected):
        _fail("backup_file_set_mismatch")
    for rel, record in expected.items():
        path = backup / rel
        info = _lstat_regular(path)
        if (
            info.st_size != record["size"]
            or stat.S_IMODE(info.st_mode) != record["mode"]
            or _sha256_file(path) != record["sha256"]
        ):
            _fail("backup_file_verification_failed")

def verify_backup(
    backup: Path,
    expected_manifest_sha256: str,
    *,
    verify_codesign: bool = True,
    semantic_validate: bool = True,
) -> dict:
    _lstat_directory(backup, private=True)
    if not SHA256_RE.fullmatch(expected_manifest_sha256):
        _fail("expected_manifest_digest_invalid")
    manifest_path = backup / BACKUP_MANIFEST
    _lstat_regular(manifest_path, private=True, max_bytes=MAX_MANIFEST_BYTES)
    raw = manifest_path.read_bytes()
    if _sha256_bytes(raw) != expected_manifest_sha256:
        _fail("backup_manifest_digest_mismatch")
    try:
        manifest = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError):
        _fail("backup_manifest_invalid")
    if not isinstance(manifest, dict):
        _fail("backup_manifest_invalid")
    _validate_manifest_shape(manifest)
    _verify_file_records(backup, manifest)
    payload = backup / "payload"
    runtime = validate_runtime_manifest(payload, verify_codesign=verify_codesign)
    recorded_runtime = manifest["runtime"]
    if not isinstance(recorded_runtime, dict):
        _fail("backup_runtime_identity_invalid")
    comparable_runtime = {key: value for key, value in runtime.items() if key != "files"}
    if comparable_runtime != recorded_runtime:
        _fail("backup_runtime_identity_mismatch")
    handoff = manifest["handoff"]
    if not isinstance(handoff, dict):
        _fail("backup_handoff_identity_invalid")
    generation_rel = handoff.get("active_generation_rel")
    if not isinstance(generation_rel, str):
        _fail("backup_handoff_identity_invalid")
    _safe_rel(generation_rel)
    verified_handoff, _ = validate_handoff_generation(
        payload / generation_rel, str(recorded_runtime["source_commit"])
    )
    for key in ("schema_version", "cumg_source_commit", "handoff_source_commit", "generation_name"):
        if verified_handoff.get(key) != handoff.get(key):
            _fail("backup_handoff_identity_mismatch")
    if collect_state(payload, "hub")[0] != manifest["hub_state"]:
        _fail("backup_hub_state_summary_mismatch")
    if collect_state(payload, "agent")[0] != manifest["agent_state"]:
        _fail("backup_agent_state_summary_mismatch")
    if _read_authority(payload / "mutation-authority") != manifest["mutation_authority"]:
        _fail("backup_mutation_authority_mismatch")
    if (payload / "mutation-authority/mutation-authority.lock").exists():
        _fail("backup_contains_coordination_lock")
    if semantic_validate:
        _semantic_validate_hub(payload)
    return manifest

def create_backup(
    root: Path,
    launch_agent_dir: Path,
    output: Path,
    *,
    domain: str | None = None,
    check_services: bool = True,
    verify_codesign: bool = True,
    semantic_validate: bool = True,
) -> str:
    if output.exists() or output.is_symlink():
        _fail("backup_destination_exists")
    domain = domain or _default_domain()
    if check_services:
        require_services_stopped(domain)
    output.parent.mkdir(parents=True, exist_ok=True)
    stage = Path(tempfile.mkdtemp(prefix=f".{output.name}.partial-", dir=output.parent))
    os.chmod(stage, 0o700)
    try:
        with hold_mutation_authority(root / "mutation-authority") as locked_authority:
            metadata, sources = build_inventory(
                root, launch_agent_dir,
                verify_codesign=verify_codesign,
                semantic_validate=semantic_validate,
            )
            if metadata["mutation_authority"] != locked_authority:
                _fail("mutation_authority_changed_during_snapshot")
            manifest = _manifest_with_records(metadata, sources)
            for record in manifest["files"]:
                rel = str(record["path"])
                _copy_exact(sources[rel], stage / rel, int(record["mode"]))
            _harden_private_tree(stage)
            encoded = _canonical_json(manifest)
            if len(encoded) > MAX_MANIFEST_BYTES:
                _fail("backup_manifest_too_large")
            _write_private(stage / BACKUP_MANIFEST, encoded)
            digest = _sha256_bytes(encoded)
            verify_backup(
                stage, digest,
                verify_codesign=verify_codesign,
                semantic_validate=semantic_validate,
            )
        os.replace(stage, output)
        return digest
    except Exception:
        shutil.rmtree(stage, ignore_errors=True)
        raise

def inspect_backup(backup: Path) -> dict[str, object]:
    _lstat_directory(backup, private=True)
    manifest_path = backup / BACKUP_MANIFEST
    _lstat_regular(manifest_path, private=True, max_bytes=MAX_MANIFEST_BYTES)
    raw = manifest_path.read_bytes()
    try:
        manifest = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError):
        _fail("backup_manifest_invalid")
    if not isinstance(manifest, dict):
        _fail("backup_manifest_invalid")
    _validate_manifest_shape(manifest)
    return {
        "manifest_sha256": _sha256_bytes(raw),
        "profile": manifest["profile"],
        "created_at_ms": manifest["created_at_ms"],
        "install_root": manifest["install_root"],
        "runtime_source_commit": manifest["runtime"]["source_commit"],
        "handoff_source_commit": manifest["handoff"]["handoff_source_commit"],
        "hub_checkpoint_sequence": manifest["hub_state"]["latest_sequence"],
        "agent_checkpoint_sequence": manifest["agent_state"]["latest_sequence"],
        "quarantine_count": manifest["hub_state"]["quarantine_count"],
        "mutation_owner": manifest["mutation_authority"]["owner"],
        "mutation_epoch": manifest["mutation_authority"]["epoch"],
    }

def _stage_default(root: Path, digest: str) -> Path:
    return root.parent / f".{root.name}.restore-stage-{digest[:12]}"

def _ensure_clean_restore_target(root: Path, launch_agent_dir: Path) -> None:
    if root.exists() or root.is_symlink():
        _fail("restore_target_not_clean")
    if launch_agent_dir.exists():
        info = launch_agent_dir.lstat()
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
            _fail("launchagent_directory_unsafe")
        for filename in PLISTS.values():
            candidate = launch_agent_dir / filename
            if candidate.exists() or candidate.is_symlink():
                _fail("restore_launchagent_target_not_clean")

def _stage_file_mapping(
    manifest: dict, backup: Path, stage: Path
) -> dict[str, tuple[Path, Path, int]]:
    mapping: dict[str, tuple[Path, Path, int]] = {}
    for record in manifest["files"]:
        rel = str(record["path"])
        source = backup / rel
        if rel.startswith("payload/"):
            destination = stage / "root" / rel[len("payload/"):]
        elif rel.startswith("launchd/"):
            destination = stage / "launchd" / rel[len("launchd/"):]
        else:
            _fail("backup_file_namespace_invalid")
        mapping[rel] = (source, destination, int(record["mode"]))
    return mapping

def verify_staged_restore(stage: Path, backup: Path, digest: str, manifest: dict) -> None:
    _lstat_directory(stage, private=True)
    marker = _read_json(stage / STAGE_MARKER, max_bytes=64 * 1024)
    expected_marker = {
        "schema_version": STAGE_SCHEMA_VERSION,
        "manifest_sha256": digest,
        "install_root": manifest["install_root"],
        "launch_agent_dir": manifest["launch_agent_dir"],
    }
    if marker != expected_marker:
        _fail("restore_stage_marker_mismatch")
    mapping = _stage_file_mapping(manifest, backup, stage)
    expected_destinations = {destination for _, destination, _ in mapping.values()}
    fresh_lock = stage / "root/mutation-authority/mutation-authority.lock"
    marker_path = stage / STAGE_MARKER
    actual_files: set[Path] = set()
    for path in stage.rglob("*"):
        info = path.lstat()
        if stat.S_ISLNK(info.st_mode):
            _fail("restore_stage_contains_symlink")
        if path.is_dir():
            if info.st_mode & 0o022:
                _fail("restore_stage_weak_directory")
            continue
        if not stat.S_ISREG(info.st_mode):
            _fail("restore_stage_special_file")
        actual_files.add(path)
    if actual_files != expected_destinations | {fresh_lock, marker_path}:
        _fail("restore_stage_file_set_mismatch")
    for _, (source, destination, mode) in mapping.items():
        source_info = _lstat_regular(source)
        dest_info = _lstat_regular(destination)
        if (
            source_info.st_size != dest_info.st_size
            or stat.S_IMODE(dest_info.st_mode) != mode
            or _sha256_file(source) != _sha256_file(destination)
        ):
            _fail("restore_stage_file_mismatch")
    lock_info = fresh_lock.lstat()
    if stat.S_ISLNK(lock_info.st_mode) or not stat.S_ISREG(lock_info.st_mode):
        _fail("restore_stage_lock_invalid")
    if lock_info.st_size != 0 or stat.S_IMODE(lock_info.st_mode) != 0o600:
        _fail("restore_stage_lock_invalid")
    staged_root = stage / "root"
    runtime = validate_runtime_manifest(staged_root, verify_codesign=False)
    comparable_runtime = {key: value for key, value in runtime.items() if key != "files"}
    if comparable_runtime != manifest["runtime"]:
        _fail("restore_stage_runtime_mismatch")
    generation_rel = str(manifest["handoff"]["active_generation_rel"])
    handoff, _ = validate_handoff_generation(
        staged_root / generation_rel, str(manifest["runtime"]["source_commit"])
    )
    for key in ("schema_version", "cumg_source_commit", "handoff_source_commit", "generation_name"):
        if handoff.get(key) != manifest["handoff"].get(key):
            _fail("restore_stage_handoff_mismatch")
    if collect_state(staged_root, "hub")[0] != manifest["hub_state"]:
        _fail("restore_stage_hub_state_mismatch")
    if collect_state(staged_root, "agent")[0] != manifest["agent_state"]:
        _fail("restore_stage_agent_state_mismatch")
    if _read_authority(staged_root / "mutation-authority") != manifest["mutation_authority"]:
        _fail("restore_stage_mutation_authority_mismatch")

def stage_restore(
    backup: Path,
    digest: str,
    *,
    install_root: Path | None = None,
    launch_agent_dir: Path | None = None,
    stage: Path | None = None,
    domain: str | None = None,
    check_services: bool = True,
    verify_codesign: bool = True,
    semantic_validate: bool = True,
) -> Path:
    manifest = verify_backup(
        backup, digest,
        verify_codesign=verify_codesign,
        semantic_validate=semantic_validate,
    )
    root = install_root or Path(str(manifest["install_root"]))
    launchdir = launch_agent_dir or Path(str(manifest["launch_agent_dir"]))
    if str(root) != manifest["install_root"] or str(launchdir) != manifest["launch_agent_dir"]:
        _fail("restore_profile_path_mismatch")
    domain = domain or _default_domain()
    if check_services:
        require_services_stopped(domain)
    _ensure_clean_restore_target(root, launchdir)
    stage = stage or _stage_default(root, digest)
    if stage.exists() or stage.is_symlink():
        _fail("restore_stage_exists")
    if stage.parent != root.parent:
        _fail("restore_stage_must_be_install_sibling")
    stage.mkdir(mode=0o700)
    try:
        mapping = _stage_file_mapping(manifest, backup, stage)
        for _, (source, destination, mode) in mapping.items():
            _copy_exact(source, destination, mode)
        authority_lock = stage / "root/mutation-authority/mutation-authority.lock"
        _write_private(authority_lock, b"", allow_empty=True)
        marker = {
            "schema_version": STAGE_SCHEMA_VERSION,
            "manifest_sha256": digest,
            "install_root": str(root),
            "launch_agent_dir": str(launchdir),
        }
        _write_private(stage / STAGE_MARKER, _canonical_json(marker))
        _harden_private_tree(stage)
        verify_staged_restore(stage, backup, digest, manifest)
        return stage
    except Exception:
        shutil.rmtree(stage, ignore_errors=True)
        raise


def _install_launchagents(stage_launchd: Path, launch_agent_dir: Path) -> list[Path]:
    if launch_agent_dir.exists():
        info = launch_agent_dir.lstat()
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISDIR(info.st_mode):
            _fail("launchagent_directory_unsafe")
    else:
        launch_agent_dir.mkdir(parents=True, mode=0o755)
    installed: list[Path] = []
    try:
        for label in LABELS:
            filename = PLISTS[label]
            source = stage_launchd / filename
            target = launch_agent_dir / filename
            if target.exists() or target.is_symlink():
                _fail("restore_launchagent_target_not_clean")
            temp = launch_agent_dir / f".{filename}.restore-{os.getpid()}"
            if temp.exists() or temp.is_symlink():
                _fail("launchagent_temp_collision")
            shutil.copyfile(source, temp)
            os.chmod(temp, stat.S_IMODE(source.stat().st_mode))
            os.replace(temp, target)
            installed.append(target)
        return installed
    except Exception:
        for path in installed:
            try:
                path.unlink()
            except OSError:
                pass
        raise

def _run_json_command(argv: Sequence[str]) -> tuple[dict, int]:
    result = subprocess.run(
        list(argv),
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
        text=True,
        timeout=45,
    )
    try:
        value = json.loads(result.stdout)
    except (TypeError, json.JSONDecodeError):
        _fail("post_restore_health_json_invalid")
    if not isinstance(value, dict):
        _fail("post_restore_health_json_invalid")
    return value, result.returncode

def _validate_post_restore_health(
    status: dict,
    status_rc: int,
    doctor: dict,
    doctor_rc: int,
    quarantine_count: int,
) -> None:
    if quarantine_count == 0:
        if (
            status_rc != 0
            or status.get("overall") != "healthy"
            or status.get("next_action") != "none"
            or doctor_rc != 0
            or doctor.get("overall") != "healthy"
        ):
            _fail("post_restore_health_failed")
        return

    if (
        status.get("overall") != "action_required"
        or status.get("primary_reason") != "previous_operation_outcome_unknown"
        or status.get("next_action") != "review_incident"
    ):
        _fail("post_restore_quarantine_status_unexpected")
    checks = doctor.get("checks")
    if not isinstance(checks, list):
        _fail("post_restore_health_json_invalid")
    allowed_error_checks = {"live_quarantine", "recovery_mode"}
    for check in checks:
        if not isinstance(check, dict):
            _fail("post_restore_health_json_invalid")
        if check.get("status") == "error" and check.get("name") not in allowed_error_checks:
            _fail("post_restore_unrelated_health_failure")
    if doctor.get("overall") != "unsafe" or doctor_rc == 0:
        _fail("post_restore_quarantine_doctor_unexpected")

def _inspect_quarantine_count(root: Path) -> int:
    inspect = subprocess.run(
        [
            str(root / "bin/v2_maint"),
            "inspect-quarantine",
            "--state-dir", str(root / "v2/state/hub"),
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
        text=True,
    )
    if inspect.returncode != 0:
        _fail("post_restore_quarantine_inspection_failed")
    try:
        report = json.loads(inspect.stdout)
    except (TypeError, json.JSONDecodeError):
        _fail("post_restore_quarantine_inspection_failed")
    if not isinstance(report, dict) or not isinstance(report.get("quarantines"), list):
        _fail("post_restore_quarantine_inspection_failed")
    return len(report["quarantines"])

def _bootstrap_services(
    root: Path,
    launch_agent_dir: Path,
    manifest: dict,
    domain: str,
) -> dict[str, object]:
    started: list[str] = []
    try:
        for label in LABELS:
            plist = launch_agent_dir / PLISTS[label]
            result = subprocess.run(
                ["launchctl", "bootstrap", domain, str(plist)],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )
            if result.returncode != 0:
                _fail("post_restore_launchagent_start_failed")
            started.append(label)
            time.sleep(1)

        operator = manifest["operator"]
        common = [
            "--install-root", str(root),
            "--run-root", str(operator["run_root"]),
            "--cua-command", str(operator["cua_command"]),
            "--expected-cua-version", str(operator["cua_version"]),
            "--handoff-control-socket", str(operator["handoff_control_socket"]),
            "--json",
        ]
        pre_quarantine = int(manifest["hub_state"]["quarantine_count"])
        last_health_error: BackupRestoreError | None = None
        for attempt in range(15):
            status, status_rc = _run_json_command([str(root / "bin/v2_status"), *common])
            doctor, doctor_rc = _run_json_command([str(root / "bin/v2_doctor"), *common])
            after_quarantine = _inspect_quarantine_count(root)
            if after_quarantine > pre_quarantine:
                _fail("post_restore_unexpected_quarantine_growth")
            authority = _read_authority(root / "mutation-authority")
            if authority != manifest["mutation_authority"]:
                _fail("post_restore_mutation_authority_changed")
            try:
                _validate_post_restore_health(
                    status, status_rc, doctor, doctor_rc, after_quarantine
                )
            except BackupRestoreError as error:
                last_health_error = error
                if attempt + 1 < 15:
                    time.sleep(1)
                    continue
                raise
            return {
                "status": status,
                "doctor": doctor,
                "quarantine_before": pre_quarantine,
                "quarantine_after": after_quarantine,
                "authoritative_reconciliation_may_have_reduced_quarantine": after_quarantine < pre_quarantine,
            }
        if last_health_error is not None:
            raise last_health_error
        _fail("post_restore_health_failed")
    except Exception:
        for label in reversed(started):
            subprocess.run(
                ["launchctl", "bootout", f"{domain}/{label}"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )
        raise

def activate_restore(
    backup: Path,
    digest: str,
    stage: Path,
    *,
    domain: str | None = None,
    check_services: bool = True,
    verify_codesign: bool = True,
    semantic_validate: bool = True,
    start_services: bool = True,
) -> dict[str, object]:
    manifest = verify_backup(
        backup, digest,
        verify_codesign=verify_codesign,
        semantic_validate=semantic_validate,
    )
    verify_staged_restore(stage, backup, digest, manifest)
    root = Path(str(manifest["install_root"]))
    launchdir = Path(str(manifest["launch_agent_dir"]))
    domain = domain or _default_domain()
    if check_services:
        require_services_stopped(domain)
    _ensure_clean_restore_target(root, launchdir)
    staged_root = stage / "root"
    staged_launchd = stage / "launchd"
    if stage.parent != root.parent:
        _fail("restore_stage_must_be_install_sibling")
    os.replace(staged_root, root)
    installed: list[Path] = []
    try:
        installed = _install_launchagents(staged_launchd, launchdir)
        run_root = Path(str(manifest["operator"]["run_root"]))
        if run_root.exists():
            _lstat_directory(run_root, private=True)
        else:
            run_root.mkdir(parents=True, mode=0o700)
        result: dict[str, object] = {
            "install_root": str(root),
            "launch_agent_dir": str(launchdir),
            "manifest_sha256": digest,
            "started": False,
        }
        if start_services:
            result["health"] = _bootstrap_services(root, launchdir, manifest, domain)
            result["started"] = True
        shutil.rmtree(stage, ignore_errors=True)
        return result
    except Exception:
        for path in installed:
            try:
                path.unlink()
            except OSError:
                pass
        raise

def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    backup = sub.add_parser("backup")
    backup.add_argument("--install-root", required=True)
    backup.add_argument("--launch-agent-dir", required=True)
    backup.add_argument("--output", required=True)
    backup.add_argument("--domain", default=None)

    verify = sub.add_parser("verify")
    verify.add_argument("--backup", required=True)
    verify.add_argument("--expected-manifest-sha256", required=True)

    inspect = sub.add_parser("inspect")
    inspect.add_argument("--backup", required=True)

    restore = sub.add_parser("restore")
    restore.add_argument("--backup", required=True)
    restore.add_argument("--expected-manifest-sha256", required=True)
    restore.add_argument("--install-root", default=None)
    restore.add_argument("--launch-agent-dir", default=None)
    restore.add_argument("--stage-dir", default=None)
    restore.add_argument("--domain", default=None)

    activate = sub.add_parser("activate")
    activate.add_argument("--backup", required=True)
    activate.add_argument("--expected-manifest-sha256", required=True)
    activate.add_argument("--stage-dir", required=True)
    activate.add_argument("--domain", default=None)
    activate.add_argument("--no-start", action="store_true")
    return parser

def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        if args.command == "backup":
            _ensure_macos()
            digest = create_backup(
                Path(args.install_root),
                Path(args.launch_agent_dir),
                Path(args.output),
                domain=args.domain,
            )
            print(f"BACKUP_OK manifest_sha256={digest} path={args.output}")
        elif args.command == "verify":
            _ensure_macos()
            manifest = verify_backup(Path(args.backup), args.expected_manifest_sha256)
            print(
                "VERIFY_OK "
                f"source_commit={manifest['runtime']['source_commit']} "
                f"quarantine_count={manifest['hub_state']['quarantine_count']} "
                f"mutation_owner={manifest['mutation_authority']['owner']} "
                f"mutation_epoch={manifest['mutation_authority']['epoch']}"
            )
        elif args.command == "inspect":
            print(json.dumps(inspect_backup(Path(args.backup)), sort_keys=True))
        elif args.command == "restore":
            _ensure_macos()
            stage = stage_restore(
                Path(args.backup),
                args.expected_manifest_sha256,
                install_root=Path(args.install_root) if args.install_root else None,
                launch_agent_dir=Path(args.launch_agent_dir) if args.launch_agent_dir else None,
                stage=Path(args.stage_dir) if args.stage_dir else None,
                domain=args.domain,
            )
            print(
                f"RESTORE_STAGED_OK stage_dir={stage} "
                f"manifest_sha256={args.expected_manifest_sha256}"
            )
        elif args.command == "activate":
            _ensure_macos()
            result = activate_restore(
                Path(args.backup),
                args.expected_manifest_sha256,
                Path(args.stage_dir),
                domain=args.domain,
                start_services=not args.no_start,
            )
            print(json.dumps({"result": "ACTIVATE_OK", **result}, sort_keys=True))
        else:
            _fail("unsupported_command")
        return 0
    except BackupRestoreError as error:
        print(
            f"REFUSED code={error.code} next_action={_next_action(error.code)}",
            file=sys.stderr,
        )
        return 2
    except (OSError, subprocess.SubprocessError) as error:
        print(
            f"REFUSED code=backup_restore_io_failure detail={type(error).__name__}",
            file=sys.stderr,
        )
        return 2

if __name__ == "__main__":
    sys.exit(main())

