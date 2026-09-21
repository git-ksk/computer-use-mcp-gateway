#!/usr/bin/env python3
"""Finish only a previously verified post-upgrade cleanup safety refusal.

This remediation is deliberately narrow. It never starts/retries an upgrade,
changes quarantine, transfers mutation authority, or restores rollback state.
It can complete the existing durable transaction only after the normal runtime
cleanup succeeds against the exact active runtime and release identity.
"""

from __future__ import annotations

import argparse
from pathlib import Path
import sys

SCRIPT_DIR = Path(__file__).resolve().parent
if str(SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIR))

import v2_handoff_runtime_cleanup as cleanup_module
import v2_upgrade_transaction as transaction_module


class RemediationRefusal(RuntimeError):
    pass


def active_runtime_generation(install_root: Path, agent_plist: Path) -> str:
    handoff_dir = install_root / "v2" / "handoff"
    cleanup_module.lstat_directory(handoff_dir)
    active = cleanup_module.current_runtime_refs(agent_plist, handoff_dir)
    if len(active) != 1:
        raise RemediationRefusal("active_runtime_identity_ambiguous")
    runtime = next(iter(active))
    return runtime.name


def run(args: argparse.Namespace) -> tuple[int, int, int, str]:
    if args.apply and not args.health_confirmed:
        raise RemediationRefusal("health_confirmation_required")

    install_root = Path(args.install_root)
    agent_plist = Path(args.agent_plist)
    transaction_file = Path(args.transaction_file)
    runtime_generation = active_runtime_generation(install_root, agent_plist)
    try:
        transaction = transaction_module.inspect_deferred_cleanup(
            transaction_file,
            expected_cumg_source_commit=args.expected_source_commit,
            expected_runtime_generation=runtime_generation,
        )
    except transaction_module.TransactionError as exc:
        raise RemediationRefusal(str(exc)) from exc

    cleanup_args = argparse.Namespace(
        install_root=args.install_root,
        agent_plist=args.agent_plist,
        rollback_root=args.rollback_root,
        runtime_manifest=args.runtime_manifest,
        expected_source_commit=args.expected_source_commit,
        expected_package_version=args.expected_package_version,
        expected_hub_agent_schema_version=args.expected_hub_agent_schema_version,
        expected_control_schema_version=args.expected_control_schema_version,
        expected_capability_schema_version=args.expected_capability_schema_version,
        keep_recent=args.keep_recent,
        health_confirmed=args.health_confirmed,
        apply=args.apply,
    )
    try:
        removed, protected, retained = cleanup_module.cleanup(cleanup_args)
    except cleanup_module.CleanupRefusal as exc:
        raise RemediationRefusal(str(exc)) from exc

    if args.apply:
        try:
            transaction_module.complete_deferred_cleanup(
                transaction_file,
                expected_transaction_id=transaction["transaction_id"],
                expected_cumg_source_commit=args.expected_source_commit,
                expected_runtime_generation=runtime_generation,
            )
        except transaction_module.TransactionError as exc:
            raise RemediationRefusal(str(exc)) from exc
    return removed, protected, retained, transaction["transaction_id"]


def parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--install-root", required=True)
    p.add_argument("--agent-plist", required=True)
    p.add_argument("--rollback-root", required=True)
    p.add_argument("--runtime-manifest", required=True)
    p.add_argument("--transaction-file", required=True)
    p.add_argument("--expected-source-commit", required=True)
    p.add_argument("--expected-package-version", required=True)
    p.add_argument("--expected-hub-agent-schema-version", required=True, type=int)
    p.add_argument("--expected-control-schema-version", required=True, type=int)
    p.add_argument("--expected-capability-schema-version", required=True, type=int)
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
        removed, protected, retained, transaction_id = run(args)
    except (OSError, RemediationRefusal) as exc:
        print(f"REFUSED reason={exc}", file=sys.stderr)
        return 2
    mode = "applied" if args.apply else "planned"
    print(
        f"DEFERRED_CLEANUP_OK mode={mode} removed={removed} protected={protected} "
        f"retained_recent={retained} transaction_id={transaction_id}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
