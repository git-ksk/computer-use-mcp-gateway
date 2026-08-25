#!/usr/bin/env python3
"""Privacy-bounded filesystem/import preflight for immutable Handoff runtimes."""

from __future__ import annotations

import argparse
import os
from pathlib import Path
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
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        if args.command == "resolve-executable":
            print(resolve_executable(args.path))
        elif args.command == "verify-import":
            verify_import(args.runtime_command, args.entrypoint, args.require_export)
        else:  # pragma: no cover
            raise PreflightRefusal("unsupported command")
    except (PreflightRefusal, subprocess.TimeoutExpired):
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
