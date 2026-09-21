import argparse
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
if str(SCRIPTS) not in sys.path:
    sys.path.insert(0, str(SCRIPTS))
SCRIPT = SCRIPTS / "v2_deferred_cleanup_recovery.py"
spec = importlib.util.spec_from_file_location("v2_deferred_cleanup_recovery", SCRIPT)
mod = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(mod)

tx = mod.transaction_module

COMMIT = "a" * 40
HANDOFF = "b" * 40
PACKAGE = "0.5.0"
HUB_AGENT = 6
CONTROL = 10
CAPABILITY = 6


def write_plist(path: Path, script: Path, env_file: Path) -> None:
    path.write_bytes(plistlib.dumps({
        "Label": "test-agent",
        "EnvironmentVariables": {
            "CUMG_V2_HANDOFF_RUNTIME_SCRIPT": str(script),
            "CUMG_V2_HANDOFF_RUNTIME_ENV_FILE": str(env_file),
        },
    }))


class DeferredCleanupRecoveryTests(unittest.TestCase):
    def fixture(self):
        temp = tempfile.TemporaryDirectory(prefix="cumg-deferred-cleanup-")
        base = Path(temp.name)
        root = base / "install"
        handoff = root / "v2" / "handoff"
        rollback = root / "rollback"
        maintenance = root / "v2" / "maintenance"
        handoff.mkdir(parents=True)
        rollback.mkdir()
        maintenance.mkdir()
        os.chmod(maintenance, 0o700)
        active = handoff / "runtime-aaaaaaa-bbbbbbb"
        old = handoff / "runtime-ccccccc-ddddddd"
        active.mkdir(); old.mkdir()
        (active / "v2_handoff_runtime.mjs").write_text("export {};\n", encoding="utf-8")
        (old / "v2_handoff_runtime.mjs").write_text("export {};\n", encoding="utf-8")
        env_file = handoff / "managed-runtime.env"
        env_file.write_text(f"CUMG_V2_HANDOFF_ROOT={active / 'handoff-root'}\n", encoding="utf-8")
        os.chmod(env_file, 0o600)
        agent_plist = base / "agent.plist"
        write_plist(agent_plist, active / "v2_handoff_runtime.mjs", env_file)
        manifest = root / "runtime-manifest.json"
        manifest.write_text(json.dumps({
            "schema_version": 4,
            "hub_agent_schema_version": HUB_AGENT,
            "control_schema_version": CONTROL,
            "capability_schema_version": CAPABILITY,
            "source_commit": COMMIT,
            "package_version": PACKAGE,
        }), encoding="utf-8")
        transaction = maintenance / "upgrade-transaction.json"
        tx.start(transaction, COMMIT, HANDOFF, "upgrade-cleanup")
        flags = tuple(flag for flag in tx.COMPLETION_FLAGS if flag != "cleanup_completed")
        tx.advance(
            transaction, phase="cleanup", runtime_generation=active.name,
            rollback_asset="runtime-upgrade-test", mutation_owner="v2", mutation_epoch=1,
            flags=flags,
        )
        tx.fail(
            transaction, status="operator_action_required", reason="cleanup_safety_refusal",
            operator_action="inspect_upgrade_status",
        )
        return temp, root, rollback, manifest, agent_plist, transaction, active, old

    def args(self, root, rollback, manifest, agent_plist, transaction, apply=True):
        return argparse.Namespace(
            install_root=str(root), agent_plist=str(agent_plist), rollback_root=str(rollback),
            runtime_manifest=str(manifest), transaction_file=str(transaction),
            expected_source_commit=COMMIT, expected_package_version=PACKAGE,
            expected_hub_agent_schema_version=HUB_AGENT,
            expected_control_schema_version=CONTROL,
            expected_capability_schema_version=CAPABILITY,
            keep_recent=0, health_confirmed=True, apply=apply,
        )

    def test_successful_cleanup_completes_exact_failed_transaction(self):
        temp, root, rollback, manifest, agent_plist, transaction, active, old = self.fixture()
        with temp:
            removed, protected, retained, transaction_id = mod.run(
                self.args(root, rollback, manifest, agent_plist, transaction)
            )
            self.assertEqual((removed, protected, retained), (1, 1, 0))
            self.assertEqual(transaction_id, "upgrade-cleanup")
            self.assertTrue(active.exists())
            self.assertFalse(old.exists())
            record = tx._read(transaction)
            self.assertEqual(record["status"], "completed")
            self.assertEqual(record["phase"], "completed")
            self.assertTrue(all(record["completion"].values()))

    def test_plan_is_read_only_and_leaves_transaction_failed(self):
        temp, root, rollback, manifest, agent_plist, transaction, _, old = self.fixture()
        with temp:
            removed, _, _, _ = mod.run(
                self.args(root, rollback, manifest, agent_plist, transaction, apply=False)
            )
            self.assertEqual(removed, 1)
            self.assertTrue(old.exists())
            self.assertEqual(tx._read(transaction)["status"], "operator_action_required")

    def test_identity_mismatch_refuses_before_cleanup(self):
        temp, root, rollback, manifest, agent_plist, transaction, _, old = self.fixture()
        with temp:
            args = self.args(root, rollback, manifest, agent_plist, transaction)
            args.expected_source_commit = "c" * 40
            with self.assertRaises(mod.RemediationRefusal):
                mod.run(args)
            self.assertTrue(old.exists())
            self.assertEqual(tx._read(transaction)["status"], "operator_action_required")


if __name__ == "__main__":
    unittest.main()
