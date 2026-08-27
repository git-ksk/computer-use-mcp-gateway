#!/usr/bin/env python3
"""Fail-closed single-Mac launchd family guard.

This helper knows only the mutually-exclusive Hub/Agent service labels used by reviewed CUMG
single-Mac deployments. It never reads CUMG payload/state, never edits plist files, and never
resolves quarantine/replay state.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from typing import Callable, Iterable, Sequence

LABEL_RE = re.compile(r"^[A-Za-z0-9._-]{1,128}$")
DOMAIN_RE = re.compile(r"^(?:gui|user)/[0-9]{1,10}$")

FAMILIES = {
    "github": {
        "hub": "com.github.git-ksk.cumg-v2-hub",
        "agent": "com.github.git-ksk.cumg-v2-agent",
    },
    "sawadakousuke": {
        "hub": "com.sawadakousuke.cumg-v2-hub",
        "agent": "com.sawadakousuke.cumg-v2-agent",
    },
}

Runner = Callable[..., subprocess.CompletedProcess[bytes]]


class GuardError(RuntimeError):
    def __init__(self, reason: str, details: str = "") -> None:
        super().__init__(reason)
        self.reason = reason
        self.details = details


@dataclass(frozen=True)
class Topology:
    hub_loaded: tuple[str, ...]
    agent_loaded: tuple[str, ...]


def _validate_label(label: str) -> str:
    if not LABEL_RE.fullmatch(label):
        raise GuardError("invalid_launchd_label")
    return label


def _validate_domain(domain: str) -> str:
    if not DOMAIN_RE.fullmatch(domain):
        raise GuardError("invalid_launchd_domain")
    return domain


def _family_for(role: str, label: str) -> str | None:
    for family, labels in FAMILIES.items():
        if labels[role] == label:
            return family
    return None


def _role_labels(role: str, configured: str) -> tuple[str, ...]:
    labels: list[str] = [configured]
    for family in FAMILIES.values():
        candidate = family[role]
        if candidate not in labels:
            labels.append(candidate)
    return tuple(labels)


def _run(runner: Runner, argv: Sequence[str]) -> subprocess.CompletedProcess[bytes]:
    return runner(argv, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)


def _loaded(runner: Runner, launchctl: str, domain: str, label: str) -> bool:
    return _run(runner, [launchctl, "print", f"{domain}/{label}"]).returncode == 0


def inspect_topology(
    *,
    domain: str,
    hub_label: str,
    agent_label: str,
    launchctl: str,
    runner: Runner = subprocess.run,
) -> Topology:
    domain = _validate_domain(domain)
    hub_label = _validate_label(hub_label)
    agent_label = _validate_label(agent_label)

    configured_hub_family = _family_for("hub", hub_label)
    configured_agent_family = _family_for("agent", agent_label)
    if (
        configured_hub_family is not None
        and configured_agent_family is not None
        and configured_hub_family != configured_agent_family
    ):
        raise GuardError(
            "configured_launchd_family_mismatch",
            f"hub_label={hub_label} agent_label={agent_label}",
        )

    hub_loaded = tuple(
        label for label in _role_labels("hub", hub_label) if _loaded(runner, launchctl, domain, label)
    )
    agent_loaded = tuple(
        label for label in _role_labels("agent", agent_label) if _loaded(runner, launchctl, domain, label)
    )

    if len(hub_loaded) > 1:
        raise GuardError(
            "conflicting_launchd_labels",
            f"role=hub labels={','.join(hub_loaded)}",
        )
    if len(agent_loaded) > 1:
        raise GuardError(
            "conflicting_launchd_labels",
            f"role=agent labels={','.join(agent_loaded)}",
        )

    if len(hub_loaded) == 1 and len(agent_loaded) == 1:
        hub_family = _family_for("hub", hub_loaded[0])
        agent_family = _family_for("agent", agent_loaded[0])
        if hub_family is not None and agent_family is not None and hub_family != agent_family:
            raise GuardError(
                "mixed_launchd_families",
                f"hub_label={hub_loaded[0]} agent_label={agent_loaded[0]}",
            )

    return Topology(hub_loaded=hub_loaded, agent_loaded=agent_loaded)


def retire_alternates(
    *,
    domain: str,
    hub_label: str,
    agent_label: str,
    launchctl: str,
    runner: Runner = subprocess.run,
) -> tuple[str, ...]:
    domain = _validate_domain(domain)
    hub_label = _validate_label(hub_label)
    agent_label = _validate_label(agent_label)

    configured_hub_family = _family_for("hub", hub_label)
    configured_agent_family = _family_for("agent", agent_label)
    if (
        configured_hub_family is not None
        and configured_agent_family is not None
        and configured_hub_family != configured_agent_family
    ):
        raise GuardError(
            "configured_launchd_family_mismatch",
            f"hub_label={hub_label} agent_label={agent_label}",
        )

    retired: list[str] = []
    for role, configured in (("hub", hub_label), ("agent", agent_label)):
        for label in _role_labels(role, configured):
            if label == configured:
                continue
            target = f"{domain}/{label}"
            if _loaded(runner, launchctl, domain, label):
                bootout = _run(runner, [launchctl, "bootout", target])
                if bootout.returncode != 0 and _loaded(runner, launchctl, domain, label):
                    raise GuardError(
                        "alternate_launchd_bootout_failed",
                        f"role={role} label={label}",
                    )
            disable = _run(runner, [launchctl, "disable", target])
            if disable.returncode != 0:
                raise GuardError(
                    "alternate_launchd_disable_failed",
                    f"role={role} label={label}",
                )
            if _loaded(runner, launchctl, domain, label):
                raise GuardError(
                    "alternate_launchd_still_loaded",
                    f"role={role} label={label}",
                )
            retired.append(label)
    return tuple(retired)


def _validate_launchctl(path: str) -> str:
    if not os.path.isabs(path) or not os.path.isfile(path) or not os.access(path, os.X_OK):
        raise GuardError("launchctl_unavailable")
    return path


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="CUMG single-Mac launchd topology guard")
    parser.add_argument("mode", choices=("check", "retire-alternates"))
    parser.add_argument("--domain", required=True)
    parser.add_argument("--hub-label", required=True)
    parser.add_argument("--agent-label", required=True)
    parser.add_argument("--launchctl", required=True)
    return parser


def main(argv: Iterable[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    try:
        launchctl = _validate_launchctl(args.launchctl)
        if args.mode == "check":
            topology = inspect_topology(
                domain=args.domain,
                hub_label=args.hub_label,
                agent_label=args.agent_label,
                launchctl=launchctl,
            )
            hub = topology.hub_loaded[0] if topology.hub_loaded else "none"
            agent = topology.agent_loaded[0] if topology.agent_loaded else "none"
            print(f"LAUNCHD_TOPOLOGY_OK hub={hub} agent={agent}")
        else:
            retired = retire_alternates(
                domain=args.domain,
                hub_label=args.hub_label,
                agent_label=args.agent_label,
                launchctl=launchctl,
            )
            print(f"LAUNCHD_ALTERNATES_RETIRED count={len(retired)}")
        return 0
    except GuardError as error:
        suffix = f" {error.details}" if error.details else ""
        print(f"REFUSED reason={error.reason}{suffix}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
