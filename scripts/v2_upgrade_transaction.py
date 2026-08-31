#!/usr/bin/env python3
"""Durable owner-private status for the reviewed single-Mac upgrade transaction.

This record is operational evidence only. It never authorizes quarantine resolution,
mutation-authority transfer, replay, retry, rollback, or any desktop operation.
"""
from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import secrets
import stat
import sys
import time
from typing import Any

SCHEMA_VERSION = 1
MAX_RECORD_BYTES = 64 * 1024
HEX40_RE = re.compile(r"^[0-9a-f]{40}$")
SAFE_TOKEN_RE = re.compile(r"^[A-Za-z0-9._-]{1,180}$")
SAFE_REASON_RE = re.compile(r"^[a-z0-9_]{1,80}$")
PHASES = (
    "build_or_stage", "handoff_stage", "backup", "service_drain",
    "authority_migration", "install", "restart", "post_verify",
    "cleanup", "completed",
)
STATUSES = (
    "in_progress", "completed", "failed_before_install",
    "failed_closed_after_stop", "operator_action_required",
)
COMPLETION_FLAGS = (
    "runtime_manifest_verified", "launchd_topology_safe",
    "mutation_authority_verified", "quarantine_clear",
    "handoff_runtime_paired", "services_restarted", "doctor_healthy",
    "cleanup_completed", "rollback_asset_created",
)
UNSAFE_REPLACE_STATUSES = {"in_progress", "failed_closed_after_stop", "operator_action_required"}

class TransactionError(RuntimeError):
    pass

def _now_ms() -> int:
    return time.time_ns() // 1_000_000

def _validate_token(value: str, name: str) -> str:
    if not SAFE_TOKEN_RE.fullmatch(value):
        raise TransactionError(f"invalid_{name}")
    return value

def _validate_commit(value: str, name: str) -> str:
    if not HEX40_RE.fullmatch(value):
        raise TransactionError(f"invalid_{name}")
    return value

def _validate_private_dir(path: pathlib.Path, *, create: bool) -> None:
    if create and not path.exists():
        path.mkdir(parents=True, mode=0o700)
    metadata = path.lstat()
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise TransactionError("unsafe_transaction_directory")
    if hasattr(os, "getuid") and metadata.st_uid != os.getuid():
        raise TransactionError("unsafe_transaction_directory_owner")
    if metadata.st_mode & 0o077:
        raise TransactionError("unsafe_transaction_directory_permissions")

def _read(path: pathlib.Path) -> dict[str, Any]:
    _validate_private_dir(path.parent, create=False)
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    try:
        fd = os.open(path, flags)
    except OSError as error:
        raise TransactionError("unsafe_transaction_record") from error
    try:
        metadata = os.fstat(fd)
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
            raise TransactionError("unsafe_transaction_record")
        if hasattr(os, "getuid") and metadata.st_uid != os.getuid():
            raise TransactionError("unsafe_transaction_record_owner")
        if metadata.st_mode & 0o077 or metadata.st_nlink != 1:
            raise TransactionError("unsafe_transaction_record_permissions")
        if metadata.st_size <= 0 or metadata.st_size > MAX_RECORD_BYTES:
            raise TransactionError("invalid_transaction_record_size")
        with os.fdopen(fd, "rb") as handle:
            fd = -1
            payload = handle.read(MAX_RECORD_BYTES + 1)
        if len(payload) > MAX_RECORD_BYTES:
            raise TransactionError("invalid_transaction_record_size")
        try:
            value = json.loads(payload.decode("utf-8"))
        except (UnicodeError, json.JSONDecodeError) as error:
            raise TransactionError("invalid_transaction_record") from error
    finally:
        if fd >= 0:
            os.close(fd)
    if not isinstance(value, dict):
        raise TransactionError("invalid_transaction_record")
    _validate_record(value)
    return value

def _atomic_write(path: pathlib.Path, record: dict[str, Any]) -> None:
    _validate_private_dir(path.parent, create=True)
    _validate_record(record)
    payload = (json.dumps(record, indent=2, sort_keys=True) + "\n").encode("utf-8")
    if len(payload) > MAX_RECORD_BYTES:
        raise TransactionError("transaction_record_too_large")
    temporary = path.with_name(f".{path.name}.new.{os.getpid()}.{secrets.token_hex(4)}")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(temporary, flags, 0o600)
    try:
        with os.fdopen(fd, "wb") as handle:
            fd = -1
            handle.write(payload); handle.flush(); os.fsync(handle.fileno())
        os.replace(temporary, path)
        directory_fd = os.open(path.parent, os.O_RDONLY)
        try: os.fsync(directory_fd)
        finally: os.close(directory_fd)
    except Exception:
        if fd >= 0: os.close(fd)
        try: temporary.unlink()
        except FileNotFoundError: pass
        raise
def _validate_record(record: dict[str, Any]) -> None:
    expected = {
        "schema_version", "transaction_id", "status", "phase", "started_at_ms",
        "updated_at_ms", "cumg_source_commit", "handoff_source_commit",
        "runtime_generation", "rollback_asset", "mutation_authority", "completion",
        "failure_reason", "operator_action",
    }
    if set(record) != expected or record.get("schema_version") != SCHEMA_VERSION:
        raise TransactionError("invalid_transaction_schema")
    _validate_token(record["transaction_id"], "transaction_id")
    if record["status"] not in STATUSES or record["phase"] not in PHASES:
        raise TransactionError("invalid_transaction_state")
    if not isinstance(record["started_at_ms"], int) or not isinstance(record["updated_at_ms"], int):
        raise TransactionError("invalid_transaction_timestamp")
    if record["started_at_ms"] <= 0 or record["updated_at_ms"] < record["started_at_ms"]:
        raise TransactionError("invalid_transaction_timestamp")
    _validate_commit(record["cumg_source_commit"], "cumg_source_commit")
    _validate_commit(record["handoff_source_commit"], "handoff_source_commit")
    for key in ("runtime_generation", "rollback_asset", "failure_reason", "operator_action"):
        value = record[key]
        if value is not None and not isinstance(value, str):
            raise TransactionError(f"invalid_{key}")
    if record["runtime_generation"] is not None:
        _validate_token(record["runtime_generation"], "runtime_generation")
    if record["rollback_asset"] is not None:
        _validate_token(record["rollback_asset"], "rollback_asset")
    if record["failure_reason"] is not None and not SAFE_REASON_RE.fullmatch(record["failure_reason"]):
        raise TransactionError("invalid_failure_reason")
    if record["operator_action"] is not None and not SAFE_REASON_RE.fullmatch(record["operator_action"]):
        raise TransactionError("invalid_operator_action")
    authority = record["mutation_authority"]
    if not isinstance(authority, dict) or set(authority) != {"owner", "epoch"}:
        raise TransactionError("invalid_mutation_authority")
    if authority["owner"] not in (None, "v1", "v2"):
        raise TransactionError("invalid_mutation_authority_owner")
    if authority["epoch"] is not None and (not isinstance(authority["epoch"], int) or authority["epoch"] < 1):
        raise TransactionError("invalid_mutation_authority_epoch")
    completion = record["completion"]
    if not isinstance(completion, dict) or set(completion) != set(COMPLETION_FLAGS):
        raise TransactionError("invalid_completion_contract")
    if any(not isinstance(value, bool) for value in completion.values()):
        raise TransactionError("invalid_completion_contract")
    if record["status"] == "completed":
        if record["phase"] != "completed" or record["failure_reason"] is not None:
            raise TransactionError("invalid_completed_transaction")
        if not all(completion.values()):
            raise TransactionError("incomplete_completion_contract")
        if record["runtime_generation"] is None or record["rollback_asset"] is None:
            raise TransactionError("incomplete_completion_identity")
        if authority["owner"] != "v2" or authority["epoch"] is None:
            raise TransactionError("incomplete_mutation_authority_contract")
    elif record["phase"] == "completed":
        raise TransactionError("invalid_terminal_phase")

def start(path: pathlib.Path, cumg: str, handoff: str, transaction_id: str | None = None) -> dict[str, Any]:
    _validate_commit(cumg, "cumg_source_commit"); _validate_commit(handoff, "handoff_source_commit")
    if path.exists() or path.is_symlink():
        prior = _read(path)
        if prior["status"] in UNSAFE_REPLACE_STATUSES:
            raise TransactionError("prior_upgrade_requires_operator_action")
    now = _now_ms()
    transaction_id = transaction_id or f"upgrade-{now}-{os.getpid()}-{secrets.token_hex(4)}"
    _validate_token(transaction_id, "transaction_id")
    record = {
        "schema_version": SCHEMA_VERSION, "transaction_id": transaction_id,
        "status": "in_progress", "phase": "build_or_stage", "started_at_ms": now,
        "updated_at_ms": now, "cumg_source_commit": cumg, "handoff_source_commit": handoff,
        "runtime_generation": None, "rollback_asset": None,
        "mutation_authority": {"owner": None, "epoch": None},
        "completion": {flag: False for flag in COMPLETION_FLAGS},
        "failure_reason": None, "operator_action": None,
    }
    _atomic_write(path, record); return record

def advance(path: pathlib.Path, *, phase: str | None = None, runtime_generation: str | None = None,
            rollback_asset: str | None = None, mutation_owner: str | None = None,
            mutation_epoch: int | None = None, flags: tuple[str, ...] = ()) -> dict[str, Any]:
    record = _read(path)
    if record["status"] != "in_progress": raise TransactionError("transaction_not_in_progress")
    if phase is not None:
        if phase not in PHASES or phase == "completed": raise TransactionError("invalid_transaction_phase")
        if PHASES.index(phase) < PHASES.index(record["phase"]): raise TransactionError("transaction_phase_regression")
        record["phase"] = phase
    if runtime_generation is not None: record["runtime_generation"] = _validate_token(runtime_generation, "runtime_generation")
    if rollback_asset is not None: record["rollback_asset"] = _validate_token(rollback_asset, "rollback_asset")
    if mutation_owner is not None or mutation_epoch is not None:
        if mutation_owner not in ("v1", "v2") or mutation_epoch is None or mutation_epoch < 1:
            raise TransactionError("invalid_mutation_authority")
        record["mutation_authority"] = {"owner": mutation_owner, "epoch": mutation_epoch}
    for flag in flags:
        if flag not in COMPLETION_FLAGS: raise TransactionError("invalid_completion_flag")
        record["completion"][flag] = True
    record["updated_at_ms"] = max(_now_ms(), record["updated_at_ms"])
    _atomic_write(path, record); return record

def fail(path: pathlib.Path, *, status: str, reason: str, operator_action: str) -> dict[str, Any]:
    if status not in {"failed_before_install", "failed_closed_after_stop", "operator_action_required"}:
        raise TransactionError("invalid_failure_status")
    if not SAFE_REASON_RE.fullmatch(reason) or not SAFE_REASON_RE.fullmatch(operator_action):
        raise TransactionError("invalid_failure_metadata")
    record = _read(path)
    if record["status"] != "in_progress": return record
    record["status"] = status; record["failure_reason"] = reason; record["operator_action"] = operator_action
    record["updated_at_ms"] = max(_now_ms(), record["updated_at_ms"])
    _atomic_write(path, record); return record

def complete(path: pathlib.Path) -> dict[str, Any]:
    record = _read(path)
    if record["status"] != "in_progress" or record["phase"] != "cleanup":
        raise TransactionError("transaction_not_ready_for_completion")
    record["status"] = "completed"; record["phase"] = "completed"
    record["failure_reason"] = None; record["operator_action"] = None
    record["updated_at_ms"] = max(_now_ms(), record["updated_at_ms"])
    _validate_record(record); _atomic_write(path, record); return record
def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--state-file", type=pathlib.Path, required=True)
    sub = parser.add_subparsers(dest="command", required=True)
    start_p = sub.add_parser("start")
    start_p.add_argument("--cumg-source-commit", required=True)
    start_p.add_argument("--handoff-source-commit", required=True)
    start_p.add_argument("--transaction-id")
    advance_p = sub.add_parser("advance")
    advance_p.add_argument("--phase", choices=PHASES[:-1])
    advance_p.add_argument("--runtime-generation")
    advance_p.add_argument("--rollback-asset")
    advance_p.add_argument("--mutation-owner", choices=("v1", "v2"))
    advance_p.add_argument("--mutation-epoch", type=int)
    advance_p.add_argument("--flag", action="append", default=[], choices=COMPLETION_FLAGS)
    fail_p = sub.add_parser("fail")
    fail_p.add_argument("--status", required=True, choices=STATUSES[2:])
    fail_p.add_argument("--reason", required=True)
    fail_p.add_argument("--operator-action", required=True)
    sub.add_parser("complete"); sub.add_parser("show")
    return parser

def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        if args.command == "start":
            record = start(args.state_file, args.cumg_source_commit, args.handoff_source_commit, args.transaction_id)
        elif args.command == "advance":
            record = advance(args.state_file, phase=args.phase, runtime_generation=args.runtime_generation,
                             rollback_asset=args.rollback_asset, mutation_owner=args.mutation_owner,
                             mutation_epoch=args.mutation_epoch, flags=tuple(args.flag))
        elif args.command == "fail":
            record = fail(args.state_file, status=args.status, reason=args.reason, operator_action=args.operator_action)
        elif args.command == "complete": record = complete(args.state_file)
        elif args.command == "show": record = _read(args.state_file)
        else: raise AssertionError("unreachable")
    except (OSError, TransactionError) as error:
        print(f"REFUSED reason={error}", file=sys.stderr); return 2
    print(json.dumps(record, sort_keys=True)); return 0

if __name__ == "__main__":
    raise SystemExit(main())
