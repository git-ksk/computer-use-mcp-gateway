#!/usr/bin/env python3
"""Safely prune unreferenced CUMG Handoff runtime generations.

This helper is deliberately narrow: it only considers direct runtime-* code directories under
<install-root>/v2/handoff. It never deletes checkpoint, key, env, audit, control, or rollback data.
The reviewed single-Mac upgrade invokes --apply only after the new paired runtime passed v2_doctor.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import sys

RUNTIME_RE = re.compile(r"runtime-[0-9a-f]{7,40}(?:-[0-9a-f]{7,40})?")
ENV_RUNTIME_KEYS = {
    "CUMG_V2_HANDOFF_ROOT",
    "CUMG_V2_HANDOFF_WEBRTC_HOST_EXECUTABLE",
    "CUMG_V2_HANDOFF_NATIVE_HOST_EXECUTABLE",
    "CUMG_V2_HANDOFF_NATIVE_REVOKE_EXECUTABLE",
}
FORBIDDEN_EXACT_NAMES = {"checkpoint.json", "checkpoint.key", "managed-runtime.env"}
FORBIDDEN_SUFFIXES = (".env", ".key")


class CleanupRefusal(RuntimeError):
    pass


def lstat_directory(path: Path) -> os.stat_result:
    try:
        stat = path.lstat()
    except OSError as exc:
        raise CleanupRefusal("required_directory_unavailable") from exc
    if path.is_symlink() or not path.is_dir():
        raise CleanupRefusal("unsafe_directory")
    return stat


def parse_env_file(path: Path) -> dict[str, str]:
    try:
        stat = path.lstat()
        if path.is_symlink() or not path.is_file():
            raise CleanupRefusal("unsafe_env_file")
        if os.name != "nt" and stat.st_mode & 0o077:
            raise CleanupRefusal("unsafe_env_permissions")
        lines = path.read_text(encoding="utf-8").splitlines()
    except CleanupRefusal:
        raise
    except OSError as exc:
        raise CleanupRefusal("env_file_unreadable") from exc
    values: dict[str, str] = {}
    for raw in lines:
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        key, value = key.strip(), value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        if key in ENV_RUNTIME_KEYS:
            if not value or "\x00" in value or "\n" in value or "\r" in value:
                raise CleanupRefusal("invalid_runtime_reference")
            values[key] = value
    return values


def runtime_from_path(value: str, handoff_dir: Path) -> Path | None:
    path = Path(value)
    if not path.is_absolute():
        raise CleanupRefusal("non_absolute_runtime_reference")
    try:
        relative = path.relative_to(handoff_dir)
    except ValueError:
        return None
    if not relative.parts:
        return None
    name = relative.parts[0]
    if not RUNTIME_RE.fullmatch(name):
        return None
    runtime = handoff_dir / name
    lstat_directory(runtime)
    return runtime


def plist_runtime_refs(plist_path: Path, handoff_dir: Path) -> tuple[set[Path], Path | None]:
    try:
        stat = plist_path.lstat()
        if plist_path.is_symlink() or not plist_path.is_file():
            raise CleanupRefusal("unsafe_agent_plist")
        payload = plistlib.loads(plist_path.read_bytes())
    except CleanupRefusal:
        raise
    except Exception as exc:
        raise CleanupRefusal("agent_plist_unreadable") from exc
    env = payload.get("EnvironmentVariables") or {}
    refs: set[Path] = set()
    script = env.get("CUMG_V2_HANDOFF_RUNTIME_SCRIPT")
    if script:
        runtime = runtime_from_path(str(script), handoff_dir)
        if runtime:
            refs.add(runtime)
    env_file_value = env.get("CUMG_V2_HANDOFF_RUNTIME_ENV_FILE")
    env_file = Path(str(env_file_value)) if env_file_value else None
    return refs, env_file


def current_runtime_refs(agent_plist: Path, handoff_dir: Path) -> set[Path]:
    refs, env_file = plist_runtime_refs(agent_plist, handoff_dir)
    if env_file is None:
        raise CleanupRefusal("handoff_env_missing")
    for value in parse_env_file(env_file).values():
        runtime = runtime_from_path(value, handoff_dir)
        if runtime:
            refs.add(runtime)
    if not refs:
        raise CleanupRefusal("active_runtime_unresolved")
    return refs


def archive_is_self_contained(bundle: Path) -> bool:
    archived = bundle / "handoff" / "runtime-generation"
    manifest = archived / "runtime-generation-manifest.json"
    if not archived.exists():
        return False
    try:
        lstat_directory(archived)
        stat = manifest.lstat()
        if manifest.is_symlink() or not manifest.is_file():
            return False
        data = json.loads(manifest.read_text(encoding="utf-8"))
        if (
            data.get("schema_version") != 1
            or data.get("archive_complete") is not True
            or not isinstance(data.get("handoff_source_commit"), str)
            or not isinstance(data.get("files"), list)
        ):
            return False
        verified = set()
        for item in data["files"]:
            if not isinstance(item, dict) or not isinstance(item.get("path"), str) or not isinstance(item.get("sha256"), str):
                return False
            relative = Path(item["path"])
            if relative.is_absolute() or ".." in relative.parts or not relative.parts:
                return False
            candidate = archived / relative
            candidate_stat = candidate.lstat()
            if candidate.is_symlink() or not candidate.is_file() or candidate_stat.st_size > 128 * 1024 * 1024:
                return False
            digest = hashlib.sha256(candidate.read_bytes()).hexdigest()
            if digest != item["sha256"]:
                return False
            verified.add(relative.as_posix())
        return (
            "v2_handoff_runtime.mjs" in verified
            and "handoff-root/dist/index.js" in verified
            and "handoff-root/package.json" in verified
            and "handoff-root/package-lock.json" in verified
            and "handoff-root/node_modules/werift/package.json" in verified
            and any(name.startswith("takeover-") for name in verified)
        )
    except (CleanupRefusal, OSError, ValueError, json.JSONDecodeError):
        return False


def legacy_rollback_runtime_refs(rollback_root: Path, handoff_dir: Path) -> set[Path]:
    refs: set[Path] = set()
    if not rollback_root.exists():
        return refs
    lstat_directory(rollback_root)
    for bundle in sorted(rollback_root.glob("runtime-upgrade-*")):
        if bundle.is_symlink() or not bundle.is_dir():
            raise CleanupRefusal("unsafe_rollback_bundle")
        if archive_is_self_contained(bundle):
            continue
        launchd = bundle / "launchd"
        if launchd.is_dir() and not launchd.is_symlink():
            for plist_path in launchd.glob("*agent*.plist"):
                plist_refs, _ = plist_runtime_refs(plist_path, handoff_dir)
                refs.update(plist_refs)
        paths = bundle / "handoff" / "paths.tsv"
        if paths.exists():
            if paths.is_symlink() or not paths.is_file():
                raise CleanupRefusal("unsafe_rollback_reference")
            for raw in paths.read_text(encoding="utf-8").splitlines():
                columns = raw.split("\t")
                if len(columns) < 3:
                    raise CleanupRefusal("invalid_rollback_reference")
                runtime = runtime_from_path(columns[2], handoff_dir)
                if runtime:
                    refs.add(runtime)
    return refs


def validate_runtime_tree(runtime: Path, handoff_dir: Path) -> None:
    if runtime.parent != handoff_dir or not RUNTIME_RE.fullmatch(runtime.name):
        raise CleanupRefusal("unsafe_runtime_candidate")
    lstat_directory(runtime)
    for root, directories, files in os.walk(runtime, topdown=True, followlinks=False):
        root_path = Path(root)
        for name in [*directories, *files]:
            entry = root_path / name
            try:
                stat = entry.lstat()
            except OSError as exc:
                raise CleanupRefusal("runtime_candidate_unreadable") from exc
            if entry.is_symlink():
                raise CleanupRefusal("runtime_candidate_contains_symlink")
            lowered = name.lower()
            if lowered in FORBIDDEN_EXACT_NAMES or lowered.endswith(FORBIDDEN_SUFFIXES):
                raise CleanupRefusal("runtime_candidate_contains_forbidden_material")
            if not entry.is_dir() and not entry.is_file():
                raise CleanupRefusal("runtime_candidate_contains_special_file")
            _ = stat


def verify_health_manifest(path: Path, expected_source_commit: str) -> None:
    if not re.fullmatch(r"[0-9a-f]{40}", expected_source_commit):
        raise CleanupRefusal("invalid_expected_source_commit")
    try:
        stat = path.lstat()
        if path.is_symlink() or not path.is_file():
            raise CleanupRefusal("unsafe_runtime_manifest")
        manifest = json.loads(path.read_text(encoding="utf-8"))
    except CleanupRefusal:
        raise
    except Exception as exc:
        raise CleanupRefusal("runtime_manifest_unreadable") from exc
    if (
        manifest.get("schema_version") != 2
        or manifest.get("source_commit") != expected_source_commit
        or not isinstance(manifest.get("hub_agent_schema_version"), int)
        or manifest.get("hub_agent_schema_version", 0) <= 0
    ):
        raise CleanupRefusal("runtime_manifest_not_paired")


def cleanup(args: argparse.Namespace) -> tuple[int, int, int]:
    install_root = Path(args.install_root)
    handoff_dir = install_root / "v2" / "handoff"
    lstat_directory(handoff_dir)
    verify_health_manifest(Path(args.runtime_manifest), args.expected_source_commit)
    if args.apply and not args.health_confirmed:
        raise CleanupRefusal("health_confirmation_required")

    active = current_runtime_refs(Path(args.agent_plist), handoff_dir)
    rollback = legacy_rollback_runtime_refs(Path(args.rollback_root), handoff_dir)
    protected = active | rollback
    candidates = []
    for child in handoff_dir.iterdir():
        if RUNTIME_RE.fullmatch(child.name):
            validate_runtime_tree(child, handoff_dir)
            candidates.append(child)

    unprotected = [path for path in candidates if path not in protected]
    unprotected.sort(key=lambda path: (path.stat().st_mtime_ns, path.name), reverse=True)
    retained_recent = set(unprotected[: args.keep_recent])
    removable = [path for path in unprotected if path not in retained_recent]

    # Validate the complete deletion plan before removing the first directory.
    for runtime in removable:
        validate_runtime_tree(runtime, handoff_dir)
    if args.apply:
        for runtime in removable:
            shutil.rmtree(runtime)
    return len(removable), len(protected), len(retained_recent)


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description="Safely prune unreferenced CUMG Handoff runtime generations")
    p.add_argument("--install-root", required=True)
    p.add_argument("--agent-plist", required=True)
    p.add_argument("--rollback-root", required=True)
    p.add_argument("--runtime-manifest", required=True)
    p.add_argument("--expected-source-commit", required=True)
    p.add_argument("--keep-recent", type=int, default=2)
    p.add_argument("--health-confirmed", action="store_true")
    p.add_argument("--apply", action="store_true")
    return p


def main() -> int:
    args = parser().parse_args()
    if args.keep_recent < 0 or args.keep_recent > 10:
        print("REFUSED reason=invalid_keep_recent", file=sys.stderr)
        return 2
    try:
        removed, protected, retained = cleanup(args)
    except CleanupRefusal as exc:
        print(f"REFUSED reason={exc}", file=sys.stderr)
        return 2
    mode = "applied" if args.apply else "planned"
    print(f"CLEANUP_OK mode={mode} removed={removed} protected={protected} retained_recent={retained}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
