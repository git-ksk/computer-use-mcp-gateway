#!/usr/bin/env python3
"""Read-only preflight for legacy Gateway + V2 Agent mutation authority.

The guard recognizes only the supported CUMG legacy Gateway and configured V2 Agent
running in one launchd GUI domain. When both resolve to the same explicit Cua executable,
it requires both service profiles to reference the same private mutation-authority domain.
It never stops services, changes ownership, or touches quarantine state.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import plistlib
import re
import stat
import subprocess
import sys
from dataclasses import dataclass
from typing import Callable, Iterable, Sequence

DOMAIN_RE = re.compile(r"^gui/[0-9]{1,10}$")
LABEL_RE = re.compile(r"^[A-Za-z0-9._-]{1,128}$")
LEGACY_LABEL = "com.sawadakousuke.computer-use-mcp-gateway"
STATE_FILE = "mutation-authority.json"
LOCK_FILE = "mutation-authority.lock"
MAX_STATE_BYTES = 1024

Runner = Callable[..., subprocess.CompletedProcess[bytes]]


class PreflightError(RuntimeError):
    def __init__(self, reason: str, details: str = "") -> None:
        super().__init__(reason)
        self.reason = reason
        self.details = details


@dataclass(frozen=True)
class ServiceProfile:
    label: str
    backend_command: pathlib.Path
    authority_dir: pathlib.Path | None


@dataclass(frozen=True)
class AuthorityState:
    owner: str
    epoch: int


def _validate_label(label: str) -> str:
    if not LABEL_RE.fullmatch(label):
        raise PreflightError("invalid_launchd_label")
    return label


def _validate_domain(domain: str) -> str:
    if not DOMAIN_RE.fullmatch(domain):
        raise PreflightError("invalid_launchd_domain")
    return domain


def _loaded(runner: Runner, launchctl: str, domain: str, label: str) -> bool:
    completed = runner(
        [launchctl, "print", f"{domain}/{label}"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return completed.returncode == 0


def _read_plist(path: pathlib.Path) -> dict:
    try:
        info = path.lstat()
    except FileNotFoundError as exc:
        raise PreflightError("service_profile_missing") from exc
    if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
        raise PreflightError("service_profile_unsafe")
    try:
        with path.open("rb") as stream:
            data = plistlib.load(stream)
    except (OSError, plistlib.InvalidFileException) as exc:
        raise PreflightError("service_profile_invalid") from exc
    if not isinstance(data, dict):
        raise PreflightError("service_profile_invalid")
    return data


def _arg_value(arguments: Sequence[object], option: str) -> str | None:
    values = [value for value in arguments if isinstance(value, str)]
    for index, value in enumerate(values):
        if value == option and index + 1 < len(values):
            candidate = values[index + 1].strip()
            return candidate or None
        prefix = f"{option}="
        if value.startswith(prefix):
            candidate = value[len(prefix):].strip()
            return candidate or None
    return None


def _absolute_existing_executable(value: str | None) -> pathlib.Path:
    if value is None:
        raise PreflightError("backend_identity_unproven")
    path = pathlib.Path(value)
    if not path.is_absolute():
        raise PreflightError("backend_identity_unproven")
    try:
        resolved = path.resolve(strict=True)
    except OSError as exc:
        raise PreflightError("backend_identity_unproven") from exc
    if not resolved.is_file() or not os.access(resolved, os.X_OK):
        raise PreflightError("backend_identity_unproven")
    return resolved


def _authority_path(value: str | None) -> pathlib.Path | None:
    if value is None:
        return None
    path = pathlib.Path(value)
    if not path.is_absolute():
        raise PreflightError("mutation_authority_path_not_absolute")
    return path


def load_legacy_profile(path: pathlib.Path, expected_label: str) -> ServiceProfile:
    data = _read_plist(path)
    label = data.get("Label")
    if label != expected_label:
        raise PreflightError("legacy_profile_label_mismatch")
    environment = data.get("EnvironmentVariables") or {}
    arguments = data.get("ProgramArguments") or []
    backend = environment.get("CUMG_BACKEND_COMMAND") or _arg_value(arguments, "--backend-command")
    authority = environment.get("CUMG_MUTATION_AUTHORITY_DIR") or _arg_value(
        arguments, "--mutation-authority-dir"
    )
    return ServiceProfile(
        label=label,
        backend_command=_absolute_existing_executable(backend),
        authority_dir=_authority_path(authority),
    )


def load_v2_agent_profile(path: pathlib.Path, expected_label: str) -> ServiceProfile:
    data = _read_plist(path)
    label = data.get("Label")
    if label != expected_label:
        raise PreflightError("v2_agent_profile_label_mismatch")
    environment = data.get("EnvironmentVariables") or {}
    arguments = data.get("ProgramArguments") or []
    backend = environment.get("CUMG_V2_CUA_COMMAND") or _arg_value(arguments, "--cua-command")
    authority = environment.get("CUMG_MUTATION_AUTHORITY_DIR") or _arg_value(
        arguments, "--mutation-authority-dir"
    )
    return ServiceProfile(
        label=label,
        backend_command=_absolute_existing_executable(backend),
        authority_dir=_authority_path(authority),
    )


def inspect_authority(directory: pathlib.Path) -> AuthorityState:
    try:
        root = directory.lstat()
    except FileNotFoundError as exc:
        raise PreflightError("mutation_authority_not_initialized") from exc
    if stat.S_ISLNK(root.st_mode) or not stat.S_ISDIR(root.st_mode):
        raise PreflightError("mutation_authority_unsafe_path")
    if root.st_mode & 0o077:
        raise PreflightError("mutation_authority_unsafe_permissions")

    for name in (LOCK_FILE, STATE_FILE):
        path = directory / name
        try:
            info = path.lstat()
        except FileNotFoundError as exc:
            raise PreflightError("mutation_authority_not_initialized") from exc
        if stat.S_ISLNK(info.st_mode) or not stat.S_ISREG(info.st_mode):
            raise PreflightError("mutation_authority_unsafe_path")
        if info.st_mode & 0o077:
            raise PreflightError("mutation_authority_unsafe_permissions")

    state_path = directory / STATE_FILE
    if state_path.stat().st_size <= 0 or state_path.stat().st_size > MAX_STATE_BYTES:
        raise PreflightError("mutation_authority_invalid_state")
    try:
        payload = json.loads(state_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise PreflightError("mutation_authority_invalid_state") from exc
    if not isinstance(payload, dict) or set(payload) != {"schema_version", "owner", "epoch"}:
        raise PreflightError("mutation_authority_invalid_state")
    if payload.get("schema_version") != 1 or payload.get("owner") not in {"v1", "v2"}:
        raise PreflightError("mutation_authority_invalid_state")
    epoch = payload.get("epoch")
    if not isinstance(epoch, int) or isinstance(epoch, bool) or epoch <= 0:
        raise PreflightError("mutation_authority_invalid_state")
    return AuthorityState(owner=payload["owner"], epoch=epoch)


def inspect_coexistence(
    *,
    domain: str,
    launchctl: str,
    legacy_label: str,
    agent_label: str,
    legacy_plist: pathlib.Path,
    agent_plist: pathlib.Path,
    allow_v2_uninitialized: bool = False,
    runner: Runner = subprocess.run,
) -> tuple[bool, bool, bool, pathlib.Path | None, AuthorityState | None]:
    domain = _validate_domain(domain)
    legacy_label = _validate_label(legacy_label)
    agent_label = _validate_label(agent_label)
    legacy_loaded = _loaded(runner, launchctl, domain, legacy_label)
    agent_loaded = _loaded(runner, launchctl, domain, agent_label)

    if not agent_loaded:
        return legacy_loaded, False, False, None, None

    agent = load_v2_agent_profile(agent_plist, agent_label)
    if agent.authority_dir is None:
        if allow_v2_uninitialized and not legacy_loaded:
            return False, True, False, None, None
        if allow_v2_uninitialized and legacy_loaded:
            raise PreflightError(
                "legacy_gateway_unfenced",
                f"legacy_label={legacy_label} agent_label={agent_label}",
            )
        raise PreflightError(
            "mutation_authority_missing",
            f"agent_label={agent_label}",
        )
    try:
        agent_authority = agent.authority_dir.resolve(strict=True)
    except OSError as exc:
        raise PreflightError("mutation_authority_not_initialized") from exc

    if not legacy_loaded:
        state = inspect_authority(agent_authority)
        return False, True, False, agent_authority, state

    legacy = load_legacy_profile(legacy_plist, legacy_label)
    same_backend = legacy.backend_command == agent.backend_command
    if not same_backend:
        state = inspect_authority(agent_authority)
        return True, True, False, agent_authority, state

    if legacy.authority_dir is None:
        raise PreflightError(
            "shared_mutation_authority_missing",
            f"legacy_label={legacy_label} agent_label={agent_label}",
        )
    try:
        legacy_authority = legacy.authority_dir.resolve(strict=True)
    except OSError as exc:
        raise PreflightError("mutation_authority_not_initialized") from exc
    if legacy_authority != agent_authority:
        raise PreflightError(
            "shared_mutation_authority_mismatch",
            f"legacy_label={legacy_label} agent_label={agent_label}",
        )
    state = inspect_authority(agent_authority)
    return True, True, True, agent_authority, state


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="CUMG V1/V2 mutation-authority preflight")
    parser.add_argument("--domain", required=True)
    parser.add_argument("--launchctl", required=True)
    parser.add_argument("--legacy-label", default=LEGACY_LABEL)
    parser.add_argument("--agent-label", required=True)
    parser.add_argument("--legacy-plist", type=pathlib.Path, required=True)
    parser.add_argument("--agent-plist", type=pathlib.Path, required=True)
    parser.add_argument(
        "--allow-v2-uninitialized",
        action="store_true",
        help="permit only a V2-only legacy profile to enter the stopped migration lane",
    )
    return parser


def main(argv: Iterable[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        launchctl = pathlib.Path(args.launchctl)
        if not launchctl.is_absolute() or not launchctl.is_file() or not os.access(launchctl, os.X_OK):
            raise PreflightError("launchctl_unavailable")
        legacy, agent, shared, authority, state = inspect_coexistence(
            domain=args.domain,
            launchctl=str(launchctl),
            legacy_label=args.legacy_label,
            agent_label=args.agent_label,
            legacy_plist=args.legacy_plist,
            agent_plist=args.agent_plist,
            allow_v2_uninitialized=args.allow_v2_uninitialized,
        )
        parts = [
            "MUTATION_AUTHORITY_PREFLIGHT_OK",
            f"legacy_gateway={'loaded' if legacy else 'not_loaded'}",
            f"v2_agent={'loaded' if agent else 'not_loaded'}",
            f"shared_backend={'true' if shared else 'false'}",
        ]
        if authority is not None and state is not None:
            parts.extend((f"owner={state.owner}", f"epoch={state.epoch}"))
        elif agent and args.allow_v2_uninitialized:
            parts.append("migration=required")
        print(" ".join(parts))
        return 0
    except PreflightError as error:
        suffix = f" {error.details}" if error.details else ""
        print(f"REFUSED reason={error.reason}{suffix}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
