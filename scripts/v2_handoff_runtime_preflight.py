#!/usr/bin/env python3
"""Privacy-bounded filesystem/import preflight for immutable Handoff runtimes."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import sys
from typing import Sequence


class PreflightRefusal(RuntimeError):
    pass


def resolve_executable(raw: str | Path) -> Path:
    path = Path(raw)
    if not path.is_absolute():
        raise PreflightRefusal("runtime command must be absolute")
    resolved = Path(os.path.realpath(path))
    try:
        mode = resolved.stat().st_mode
    except OSError as exc:
        raise PreflightRefusal("runtime command target is unavailable") from exc
    if not resolved.is_absolute() or not stat.S_ISREG(mode) or not os.access(resolved, os.X_OK):
        raise PreflightRefusal("runtime command target is not an executable regular file")
    return resolved


def _validated_manifest_path(raw: object) -> PurePosixPath:
    if not isinstance(raw, str) or not raw or "\\" in raw:
        raise PreflightRefusal("runtime generation manifest path is invalid")
    path = PurePosixPath(raw)
    if path.is_absolute() or path.as_posix() != raw or any(part in {"", ".", ".."} for part in path.parts):
        raise PreflightRefusal("runtime generation manifest path is unsafe")
    return path


def verify_generation(
    runtime_root: str | Path,
    expected_cumg_commit: str,
    expected_handoff_commit: str,
) -> None:
    root = Path(runtime_root)
    if not root.is_absolute() or root.is_symlink() or not root.is_dir():
        raise PreflightRefusal("runtime generation root is unavailable or unsafe")
    try:
        root_mode = root.stat().st_mode
    except OSError as exc:
        raise PreflightRefusal("runtime generation root is unavailable") from exc
    if stat.S_IMODE(root_mode) & 0o077:
        raise PreflightRefusal("runtime generation root is not owner-private")

    manifest_path = root / "runtime-generation-manifest.json"
    if manifest_path.is_symlink() or not manifest_path.is_file():
        raise PreflightRefusal("runtime generation manifest is unavailable or unsafe")
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise PreflightRefusal("runtime generation manifest is unreadable") from exc
    if not isinstance(manifest, dict) or manifest.get("schema_version") != 1:
        raise PreflightRefusal("runtime generation manifest schema is unsupported")
    if manifest.get("cumg_source_commit") != expected_cumg_commit:
        raise PreflightRefusal("runtime generation CUMG commit does not match")
    if manifest.get("handoff_source_commit") != expected_handoff_commit:
        raise PreflightRefusal("runtime generation Handoff commit does not match")

    records = manifest.get("files")
    if not isinstance(records, list) or not records:
        raise PreflightRefusal("runtime generation manifest file set is invalid")
    expected: dict[str, str] = {}
    for record in records:
        if not isinstance(record, dict) or set(record) != {"path", "sha256"}:
            raise PreflightRefusal("runtime generation manifest file record is invalid")
        relative = _validated_manifest_path(record.get("path"))
        digest = record.get("sha256")
        if not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
            raise PreflightRefusal("runtime generation manifest digest is invalid")
        key = relative.as_posix()
        if key == "runtime-generation-manifest.json" or key in expected:
            raise PreflightRefusal("runtime generation manifest file record is duplicated or reserved")
        expected[key] = digest

    actual: dict[str, Path] = {}
    for base, directories, files in os.walk(root, topdown=True, followlinks=False):
        base_path = Path(base)
        for name in directories:
            candidate = base_path / name
            if candidate.is_symlink() or not candidate.is_dir():
                raise PreflightRefusal("runtime generation contains an unsafe directory")
        for name in files:
            candidate = base_path / name
            if candidate == manifest_path:
                continue
            if candidate.is_symlink():
                raise PreflightRefusal("runtime generation contains a symlink")
            try:
                mode = candidate.stat().st_mode
            except OSError as exc:
                raise PreflightRefusal("runtime generation file is unavailable") from exc
            if not stat.S_ISREG(mode):
                raise PreflightRefusal("runtime generation contains a non-regular file")
            relative = candidate.relative_to(root).as_posix()
            if relative in actual:
                raise PreflightRefusal("runtime generation contains a duplicate file path")
            actual[relative] = candidate

    if set(actual) != set(expected):
        raise PreflightRefusal("runtime generation file set does not match its manifest")
    for relative, candidate in actual.items():
        try:
            digest = hashlib.sha256(candidate.read_bytes()).hexdigest()
        except OSError as exc:
            raise PreflightRefusal("runtime generation file cannot be hashed") from exc
        if digest != expected[relative]:
            raise PreflightRefusal("runtime generation file digest does not match")

    required = {
        "v2_handoff_runtime.mjs",
        "handoff-root/dist/index.js",
        "handoff-root/package.json",
        "handoff-root/package-lock.json",
    }
    if not required.issubset(actual):
        raise PreflightRefusal("runtime generation is missing required runtime files")


def verify_import(
    runtime_command: str | Path,
    entrypoint: str | Path,
    required_exports: Sequence[str] = (),
) -> None:
    command = resolve_executable(runtime_command)
    module = Path(entrypoint)
    if not module.is_absolute() or module.is_symlink() or not module.is_file():
        raise PreflightRefusal("staged entrypoint is unavailable or unsafe")
    if any(re.fullmatch(r"[A-Za-z_$][A-Za-z0-9_$]*", item) is None for item in required_exports):
        raise PreflightRefusal("required export name is invalid")

    script = (
        'import { pathToFileURL } from "node:url"; '
        'const entrypoint = process.argv[1]; '
        'const required = process.argv.slice(2); '
        'const m = await import(pathToFileURL(entrypoint).href); '
        'for (const name of required) '
        'if (!(name in m)) throw new Error(`missing export: ${name}`);'
    )
    result = subprocess.run(
        [str(command), "--input-type=module", "-e", script, str(module), *required_exports],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
        timeout=20,
    )
    if result.returncode != 0:
        raise PreflightRefusal("staged runtime import failed")


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)

    resolve = sub.add_parser("resolve-executable")
    resolve.add_argument("--path", required=True)

    verify = sub.add_parser("verify-import")
    verify.add_argument("--runtime-command", required=True)
    verify.add_argument("--entrypoint", required=True)
    verify.add_argument("--require-export", action="append", default=[])

    generation = sub.add_parser("verify-generation")
    generation.add_argument("--runtime-root", required=True)
    generation.add_argument("--expected-cumg-commit", required=True)
    generation.add_argument("--expected-handoff-commit", required=True)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        if args.command == "resolve-executable":
            print(resolve_executable(args.path))
        elif args.command == "verify-import":
            verify_import(args.runtime_command, args.entrypoint, args.require_export)
        elif args.command == "verify-generation":
            verify_generation(args.runtime_root, args.expected_cumg_commit, args.expected_handoff_commit)
        else:  # pragma: no cover
            raise PreflightRefusal("unsupported command")
    except (PreflightRefusal, subprocess.TimeoutExpired):
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
