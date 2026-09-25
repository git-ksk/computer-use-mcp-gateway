import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import shutil
import sys
import tempfile
import unittest
from unittest import mock

SCRIPT = Path(__file__).resolve().parents[1] / "v2_backup_restore.py"
SPEC = importlib.util.spec_from_file_location("v2_backup_restore", SCRIPT)
mod = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = mod
SPEC.loader.exec_module(mod)

CUMG_COMMIT = "a" * 40
HANDOFF_COMMIT = "b" * 40


class BackupRestoreTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cumg-backup-restore-")
        self.base = Path(self.temp.name)
        self.root = self.base / "install"
        self.launchd = self.base / "LaunchAgents"
        self.run_root = self.base / "run"
        self.backup = self.base / "backup"
        self.stage = self.base / ".install.restore-stage-test"
        self._make_fixture()

    def tearDown(self):
        self.temp.cleanup()

    @staticmethod
    def _write(path: Path, data: bytes, mode: int = 0o600) -> Path:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        path.chmod(mode)
        return path

    @staticmethod
    def _mkdir(path: Path, mode: int = 0o700) -> Path:
        path.mkdir(parents=True, exist_ok=True)
        path.chmod(mode)
        return path

    def _make_fixture(self):
        self._mkdir(self.root)
        self._mkdir(self.launchd, 0o755)
        self._mkdir(self.run_root)
        for relative in (
            "bin",
            "v2",
            "v2/state",
            "v2/state/hub",
            "v2/state/agent",
            "v2/handoff",
            "v2/secrets",
            "v2/trust",
            "mutation-authority",
        ):
            self._mkdir(self.root / relative)

        binary_names = (
            "v2_hub",
            "v2_agent",
            "v2_maint",
            "v2_doctor",
            "v2_status",
            "v2_recover",
            "v2_recovery_enclave_helper",
            "v2_grant_signer",
        )
        binary_records = []
        for name in binary_names:
            payload = f"fixture-{name}\n".encode()
            path = self._write(self.root / "bin" / name, payload, 0o700)
            binary_records.append({"name": name, "sha256": hashlib.sha256(payload).hexdigest()})
        runtime_manifest = {
            "schema_version": 4,
            "hub_agent_schema_version": 6,
            "control_schema_version": 12,
            "capability_schema_version": 8,
            "source_commit": CUMG_COMMIT,
            "package_version": "0.7.0",
            "binaries": binary_records,
        }
        self._write(
            self.root / "runtime-manifest.json",
            (json.dumps(runtime_manifest) + "\n").encode(),
        )

        generation = self.root / "v2/handoff" / f"runtime-{CUMG_COMMIT[:12]}-{HANDOFF_COMMIT[:12]}"
        self._mkdir(generation)
        handoff_payloads = {
            "v2_handoff_runtime.mjs": b"export {};\n",
            "takeover-webrtc-host": b"fixture-host\n",
            "handoff-root/dist/index.js": b"export {};\n",
            "handoff-root/package.json": b'{"name":"fixture"}\n',
            "handoff-root/package-lock.json": b'{"lockfileVersion":3}\n',
        }
        handoff_records = []
        for relative, payload in handoff_payloads.items():
            path = self._write(generation / relative, payload, 0o700 if "/" not in relative else 0o600)
            handoff_records.append({"path": relative, "sha256": hashlib.sha256(payload).hexdigest()})
        generation_manifest = {
            "schema_version": 1,
            "cumg_source_commit": CUMG_COMMIT,
            "handoff_source_commit": HANDOFF_COMMIT,
            "files": handoff_records,
        }
        self._write(
            generation / "runtime-generation-manifest.json",
            (json.dumps(generation_manifest) + "\n").encode(),
        )
        checkpoint = self._write(self.root / "v2/handoff/checkpoint.json", b'{"phase":"recovery"}\n')
        checkpoint_key = self._write(self.root / "v2/handoff/checkpoint.key", b"checkpoint-key\n")
        managed_env = (
            f"CUMG_V2_HANDOFF_ROOT={generation / 'handoff-root'}\n"
            f"CUMG_V2_HANDOFF_CHECKPOINT_FILE={checkpoint}\n"
            f"CUMG_V2_HANDOFF_CHECKPOINT_KEY_FILE={checkpoint_key}\n"
        ).encode()
        self._write(self.root / "v2/handoff/managed-runtime.env", managed_env)

        hub_execution = {
            "schema_version": 14,
            "admission": [],
            "operations": [{"operation": {"operation_id": "op_fixture"}, "state": "indeterminate"}],
            "quarantines": [{
                "device_id": "device-fixture",
                "operation_id": "op_fixture",
                "device_generation": 4,
                "owner": {"issuer": "fixture", "subject": "operator"},
                "reason": "backend_outcome_indeterminate",
                "since_ms": 1,
            }],
            "recoveries": [],
            "resolutions": [],
            "auto_resolutions": [],
            "retirements": [{"operation": {"operation_id": "old-op"}, "replayed": False}],
            "mutation_resume_barriers": [],
            "mutation_resumes": [],
        }
        hub_checkpoint = {
            "schema_version": 6,
            "registry": {
                "schema_version": 8,
                "devices": [],
                "revoked_device_ids": [],
            },
            "execution": hub_execution,
        }
        self._write(
            self.root / "v2/state/hub/hub-00000000000000000042.json",
            (json.dumps(hub_checkpoint) + "\n").encode(),
        )
        agent_checkpoint = {
            "schema_version": 6,
            "device_id": "device-fixture",
            "trusted_hub": {},
            "grant_ledger": {},
            "execution": {},
            "terminal_evidence": [],
            "backend_execution_receipts": [],
            "managed_job_fail_closed": False,
        }
        self._write(
            self.root / "v2/state/agent/agent-00000000000000000041.json",
            (json.dumps(agent_checkpoint) + "\n").encode(),
        )
        self._write(self.root / "v2/state/agent/recovery-resolved.json", b'{"fixture":true}\n')
        self._write(self.root / "v2/state/hub/recovery-public-key.p256", b"fixture-public-key\n")

        # Explicitly excluded runtime/ephemeral artifacts.
        self._write(self.root / "v2/state/hub/.cumg-v2-state.lock", b"lock\n")
        self._write(
            self.root / "v2/state/hub/recovery-public-key.p256.pre-rotate-20260902",
            b"stale-key\n",
        )
        self._mkdir(self.root / "v2/state/agent/browser-upload-staging")
        self._write(self.root / "v2/state/agent/browser-upload-staging/payload.bin", b"secret-upload")
        self._mkdir(self.root / "v2/state/agent/browser-download-staging")
        self._write(self.root / "v2/state/agent/browser-download-staging/payload.bin", b"secret-download")

        authority = {"schema_version": 1, "owner": "v2", "epoch": 7}
        self._write(
            self.root / "mutation-authority/mutation-authority.json",
            (json.dumps(authority) + "\n").encode(),
        )
        self._write(self.root / "mutation-authority/mutation-authority.lock", b"", 0o600)

        refs = {
            "v2/secrets/hub.key": b"hub-key",
            "v2/secrets/grant.key": b"grant-key",
            "v2/secrets/device.key": b"device-key",
            "v2/secrets/tls-server.key": b"tls-key",
            "v2/secrets/trusted-proxy-secret": b"proxy-key",
            "v2/secrets/recovery.sealed": b"sealed-key",
            "v2/trust/hub.pub": b"hub-pub",
            "v2/trust/grant.pub": b"grant-pub",
            "v2/trust/device.pub": b"device-pub",
            "v2/trust/tls-root.der": b"tls-root",
            "v2/trust/tls-server.pem": b"tls-cert",
            "v2/trust/northbound-policy.json": b'{}',
            "v2/trust/grant-signer-policy.json": b'{}',
        }
        for relative, payload in refs.items():
            self._write(self.root / relative, payload + b"\n")

        self._write_plists(generation)

    def _write_plists(self, generation: Path):
        hub_env = {
            "CUMG_V2_HUB_SECRET_FILE": str(self.root / "v2/secrets/hub.key"),
            "CUMG_V2_GRANT_SIGNER_SOCKET": str(self.run_root / "grant-signer.sock"),
            "CUMG_V2_GRANT_PUBLIC_KEY_FILE": str(self.root / "v2/trust/grant.pub"),
            "CUMG_V2_DEVICE_PUBLIC_KEY_FILE": str(self.root / "v2/trust/device.pub"),
            "CUMG_V2_TLS_CERT_PEM_FILE": str(self.root / "v2/trust/tls-server.pem"),
            "CUMG_V2_TLS_KEY_PEM_FILE": str(self.root / "v2/secrets/tls-server.key"),
            "CUMG_V2_HUB_STATE_DIR": str(self.root / "v2/state/hub"),
            "CUMG_V2_STATUS_INSTALL_ROOT": str(self.root),
            "CUMG_V2_STATUS_RUN_ROOT": str(self.run_root),
            "CUMG_V2_NORTHBOUND_POLICY_FILE": str(self.root / "v2/trust/northbound-policy.json"),
            "CUMG_V2_HANDOFF_CONTROL_SOCKET": str(self.run_root / "handoff-control.sock"),
        }
        agent_env = {
            "CUMG_V2_DEVICE_SECRET_FILE": str(self.root / "v2/secrets/device.key"),
            "CUMG_V2_HUB_PUBLIC_KEY_FILE": str(self.root / "v2/trust/hub.pub"),
            "CUMG_V2_GRANT_PUBLIC_KEY_FILE": str(self.root / "v2/trust/grant.pub"),
            "CUMG_V2_TLS_ROOT_DER_FILE": str(self.root / "v2/trust/tls-root.der"),
            "CUMG_V2_STATE_DIR": str(self.root / "v2/state/agent"),
            "CUMG_V2_EPHEMERAL_DATA_PARENT": str(self.run_root / "agent-ephemeral"),
            "CUMG_V2_CUA_COMMAND": "/bin/echo",
            "CUMG_V2_CUA_BACKEND_VERSION": "0.19.3",
            "CUMG_MUTATION_AUTHORITY_DIR": str(self.root / "mutation-authority"),
            "CUMG_V2_HANDOFF_RUNTIME_COMMAND": "/usr/bin/node",
            "CUMG_V2_HANDOFF_RUNTIME_SCRIPT": str(generation / "v2_handoff_runtime.mjs"),
            "CUMG_V2_HANDOFF_RUNTIME_ENV_FILE": str(self.root / "v2/handoff/managed-runtime.env"),
        }
        signer_env = {
            "CUMG_V2_GRANT_SIGNER_SOCKET": str(self.run_root / "grant-signer.sock"),
            "CUMG_V2_GRANT_SECRET_FILE": str(self.root / "v2/secrets/grant.key"),
            "CUMG_V2_GRANT_SIGNER_POLICY_FILE": str(self.root / "v2/trust/grant-signer-policy.json"),
        }
        specs = (
            (
                "com.github.git-ksk.cumg-v2-grant-signer",
                "v2_grant_signer",
                signer_env,
            ),
            ("com.github.git-ksk.cumg-v2-hub", "v2_hub", hub_env),
            ("com.github.git-ksk.cumg-v2-agent", "v2_agent", agent_env),
        )
        for label, binary, env in specs:
            payload = {
                "Label": label,
                "ProgramArguments": [str(self.root / "bin" / binary)],
                "EnvironmentVariables": env,
            }
            self._write(
                self.launchd / mod.PLISTS[label],
                plistlib.dumps(payload),
            )

    def _backup(self) -> str:
        return mod.create_backup(
            self.root,
            self.launchd,
            self.backup,
            check_services=False,
            verify_codesign=False,
            semantic_validate=False,
        )

    def _prepare_clean_target(self):
        shutil.rmtree(self.root)
        shutil.rmtree(self.launchd)


    def test_round_trip_preserves_quarantine_replay_and_authority(self):
        hub_path = self.root / "v2/state/hub/hub-00000000000000000042.json"
        hub_state = json.loads(hub_path.read_text())
        hub_state["schema_version"] = 7
        hub_state["durable_fence"] = {
            "schema_version": 1,
            "revision": 23,
            "writer_epoch": 9,
        }
        hub_path.write_text(json.dumps(hub_state) + "\n")
        hub_path.chmod(0o600)

        digest = self._backup()
        manifest = mod.verify_backup(
            self.backup,
            digest,
            verify_codesign=False,
            semantic_validate=False,
        )
        self.assertEqual(manifest["hub_state"]["quarantine_count"], 1)
        self.assertEqual(manifest["hub_state"]["operations_count"], 1)
        self.assertEqual(manifest["hub_state"]["retirements_count"], 1)
        self.assertEqual(manifest["hub_state"]["schema_version"], 7)
        self.assertEqual(manifest["hub_state"]["state_revision"], 23)
        self.assertEqual(manifest["hub_state"]["writer_epoch"], 9)
        self.assertEqual(manifest["mutation_authority"], {
            "schema_version": 1,
            "owner": "v2",
            "epoch": 7,
        })
        self.assertFalse((self.backup / "payload/mutation-authority/mutation-authority.lock").exists())
        self.assertFalse((self.backup / "payload/v2/state/agent/browser-upload-staging").exists())
        self.assertFalse((self.backup / "payload/v2/state/agent/browser-download-staging").exists())
        self.assertFalse(
            (self.backup / "payload/v2/state/hub/recovery-public-key.p256.pre-rotate-20260902").exists()
        )
        self.assertFalse((self.backup / "payload/v2/state/hub/.cumg-v2-state.lock").exists())
        self.assertTrue((self.backup / "payload/v2/secrets/recovery.sealed").is_file())

        original_execution = manifest["hub_state"]["execution_sha256"]
        self._prepare_clean_target()
        stage = mod.stage_restore(
            self.backup,
            digest,
            stage=self.stage,
            check_services=False,
            verify_codesign=False,
            semantic_validate=False,
        )
        staged_manifest = mod.verify_backup(
            self.backup,
            digest,
            verify_codesign=False,
            semantic_validate=False,
        )
        mod.verify_staged_restore(stage, self.backup, digest, staged_manifest)
        self.assertEqual(
            mod.collect_state(stage / "root", "hub")[0]["execution_sha256"],
            original_execution,
        )
        staged_hub = mod.collect_state(stage / "root", "hub")[0]
        self.assertEqual(staged_hub["quarantine_count"], 1)
        self.assertEqual(staged_hub["state_revision"], 23)
        self.assertEqual(staged_hub["writer_epoch"], 9)

        result = mod.activate_restore(
            self.backup,
            digest,
            stage,
            check_services=False,
            verify_codesign=False,
            semantic_validate=False,
            start_services=False,
        )
        self.assertFalse(result["started"])
        self.assertEqual(mod._read_authority(self.root / "mutation-authority")["epoch"], 7)
        lock = self.root / "mutation-authority/mutation-authority.lock"
        self.assertTrue(lock.is_file())
        self.assertEqual(lock.stat().st_size, 0)
        self.assertEqual(mod.collect_state(self.root, "hub")[0]["execution_sha256"], original_execution)
        self.assertEqual(mod.collect_state(self.root, "hub")[0]["quarantine_count"], 1)

    def test_external_manifest_anchor_rejects_wrong_digest(self):
        digest = self._backup()
        wrong = ("0" if digest[0] != "0" else "1") + digest[1:]
        with self.assertRaisesRegex(mod.BackupRestoreError, "backup_manifest_digest_mismatch"):
            mod.verify_backup(
                self.backup,
                wrong,
                verify_codesign=False,
                semantic_validate=False,
            )

    def test_tampered_missing_and_extra_files_fail_closed(self):
        for mutation in ("tamper", "missing", "extra"):
            with self.subTest(mutation=mutation):
                if self.backup.exists():
                    shutil.rmtree(self.backup)
                digest = self._backup()
                target = self.backup / "payload/v2/secrets/hub.key"
                if mutation == "tamper":
                    target.write_bytes(target.read_bytes() + b"tamper")
                elif mutation == "missing":
                    target.unlink()
                else:
                    self._write(self.backup / "payload/v2/secrets/extra.key", b"extra")
                with self.assertRaises(mod.BackupRestoreError):
                    mod.verify_backup(
                        self.backup,
                        digest,
                        verify_codesign=False,
                        semantic_validate=False,
                    )

    def test_newer_checkpoint_schema_is_refused(self):
        path = self.root / "v2/state/hub/hub-00000000000000000042.json"
        value = json.loads(path.read_text())
        value["schema_version"] = 8
        path.write_text(json.dumps(value) + "\n")
        path.chmod(0o600)
        with self.assertRaisesRegex(mod.BackupRestoreError, "checkpoint_schema_unsupported"):
            self._backup()

    def test_hub_schema7_requires_and_summarizes_durable_writer_fence(self):
        path = self.root / "v2/state/hub/hub-00000000000000000042.json"
        value = json.loads(path.read_text())
        value["schema_version"] = 7
        value["durable_fence"] = {
            "schema_version": 1,
            "revision": 17,
            "writer_epoch": 4,
        }
        path.write_text(json.dumps(value) + "\n")
        path.chmod(0o600)

        summary, _ = mod.collect_state(self.root, "hub")
        self.assertEqual(summary["schema_version"], 7)
        self.assertEqual(summary["state_revision"], 17)
        self.assertEqual(summary["writer_epoch"], 4)

        del value["durable_fence"]
        path.write_text(json.dumps(value) + "\n")
        path.chmod(0o600)
        with self.assertRaisesRegex(mod.BackupRestoreError, "hub_writer_fence_invalid"):
            mod.collect_state(self.root, "hub")

    def test_legacy_hub_schema_rejects_injected_writer_fence(self):
        path = self.root / "v2/state/hub/hub-00000000000000000042.json"
        value = json.loads(path.read_text())
        value["durable_fence"] = {
            "schema_version": 1,
            "revision": 1,
            "writer_epoch": 1,
        }
        path.write_text(json.dumps(value) + "\n")
        path.chmod(0o600)
        with self.assertRaisesRegex(mod.BackupRestoreError, "hub_writer_fence_invalid"):
            mod.collect_state(self.root, "hub")

    def test_unknown_state_entry_is_refused(self):
        self._write(self.root / "v2/state/agent/future-authority.json", b'{"schema_version":1}\n')
        with self.assertRaisesRegex(mod.BackupRestoreError, "unknown_state_entry"):
            self._backup()

    def test_external_authority_reference_is_refused(self):
        hub_plist = self.launchd / mod.PLISTS["com.github.git-ksk.cumg-v2-hub"]
        data = plistlib.loads(hub_plist.read_bytes())
        outside = self._write(self.base / "outside.key", b"outside")
        data["EnvironmentVariables"]["CUMG_V2_HUB_SECRET_FILE"] = str(outside)
        hub_plist.write_bytes(plistlib.dumps(data))
        hub_plist.chmod(0o600)
        with self.assertRaisesRegex(mod.BackupRestoreError, "external_authority_reference"):
            self._backup()

    def test_symlink_authority_reference_is_refused(self):
        real = self.root / "v2/secrets/hub.key"
        moved = self.root / "v2/secrets/hub.real.key"
        real.rename(moved)
        os.symlink(moved.name, real)
        with self.assertRaises(mod.BackupRestoreError):
            self._backup()


    def test_parent_symlink_authority_reference_is_refused(self):
        real = self.root / "v2/trust"
        alias = self.root / "v2/trust-alias"
        alias.symlink_to(real, target_is_directory=True)
        path = alias / "grant.pub"
        with self.assertRaisesRegex(mod.BackupRestoreError, "symlinked_authority_reference"):
            mod._exact_root_path(path, self.root)

    def test_weak_authority_file_permissions_are_refused(self):
        target = self.root / "v2/secrets/hub.key"
        target.chmod(0o644)
        with self.assertRaisesRegex(mod.BackupRestoreError, "private_file_permissions"):
            self._backup()

    def test_busy_mutation_authority_refuses_snapshot(self):
        lock = self.root / "mutation-authority/mutation-authority.lock"
        with lock.open("r+b") as handle:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            try:
                with self.assertRaisesRegex(mod.BackupRestoreError, "mutation_authority_busy"):
                    self._backup()
            finally:
                fcntl.flock(handle.fileno(), fcntl.LOCK_UN)

    def test_handoff_identity_mismatch_is_refused(self):
        generation = self.root / "v2/handoff" / f"runtime-{CUMG_COMMIT[:12]}-{HANDOFF_COMMIT[:12]}"
        manifest = generation / "runtime-generation-manifest.json"
        value = json.loads(manifest.read_text())
        value["handoff_source_commit"] = "c" * 40
        manifest.write_text(json.dumps(value) + "\n")
        manifest.chmod(0o600)
        with self.assertRaises(mod.BackupRestoreError):
            self._backup()


    def test_refusal_remediation_is_bounded_and_stable(self):
        self.assertEqual(
            mod._next_action("mutation_authority_busy"),
            "stop_effectful_writer_and_retry",
        )
        self.assertEqual(
            mod._next_action("checkpoint_schema_unsupported"),
            "use_version_paired_reviewed_artifact",
        )
        self.assertEqual(
            mod._next_action("post_restore_health_failed"),
            "keep_non_effectful_and_inspect_status_doctor",
        )
        self.assertEqual(
            mod._next_action("unexpected_future_code"),
            "inspect_backup_restore_runbook",
        )

    def test_restore_requires_clean_exact_profile(self):
        digest = self._backup()
        with self.assertRaisesRegex(mod.BackupRestoreError, "restore_target_not_clean"):
            mod.stage_restore(
                self.backup,
                digest,
                stage=self.stage,
                check_services=False,
                verify_codesign=False,
                semantic_validate=False,
            )
        self._prepare_clean_target()
        with self.assertRaisesRegex(mod.BackupRestoreError, "restore_profile_path_mismatch"):
            mod.stage_restore(
                self.backup,
                digest,
                install_root=self.base / "different-install",
                launch_agent_dir=self.launchd,
                stage=self.stage,
                check_services=False,
                verify_codesign=False,
                semantic_validate=False,
            )



    def test_bootstrap_starts_signer_hub_agent_and_accepts_authoritative_reconciliation(self):
        metadata, _ = mod.build_inventory(
            self.root,
            self.launchd,
            verify_codesign=False,
            semantic_validate=False,
        )
        started = []

        class Result:
            def __init__(self, returncode=0, stdout=""):
                self.returncode = returncode
                self.stdout = stdout

        def fake_run(argv, **kwargs):
            args = [str(x) for x in argv]
            if args[:2] == ["launchctl", "bootstrap"]:
                started.append(Path(args[-1]).name)
                return Result(0)
            if args[:2] == ["launchctl", "bootout"]:
                return Result(0)
            if args[0].endswith("v2_status"):
                return Result(0, json.dumps({"overall": "healthy", "next_action": "none"}))
            if args[0].endswith("v2_doctor"):
                return Result(0, json.dumps({"overall": "healthy", "checks": []}))
            if args[0].endswith("v2_maint") and "inspect-quarantine" in args:
                return Result(0, json.dumps({"quarantines": []}))
            raise AssertionError(f"unexpected subprocess: {args}")

        with mock.patch.object(mod.subprocess, "run", side_effect=fake_run), mock.patch.object(
            mod.time, "sleep", return_value=None
        ):
            result = mod._bootstrap_services(
                self.root, self.launchd, metadata, "gui/501"
            )

        self.assertEqual(
            started,
            [mod.PLISTS[label] for label in mod.LABELS],
        )
        self.assertEqual(result["quarantine_before"], 1)
        self.assertEqual(result["quarantine_after"], 0)
        self.assertTrue(result["authoritative_reconciliation_may_have_reduced_quarantine"])
        self.assertEqual(mod._read_authority(self.root / "mutation-authority")["epoch"], 7)

    def test_post_restore_health_accepts_only_expected_clean_or_quarantine_state(self):
        mod._validate_post_restore_health(
            {"overall": "healthy", "next_action": "none"},
            0,
            {"overall": "healthy", "checks": []},
            0,
            0,
        )
        mod._validate_post_restore_health(
            {
                "overall": "action_required",
                "primary_reason": "previous_operation_outcome_unknown",
                "next_action": "review_incident",
            },
            2,
            {
                "overall": "unsafe",
                "checks": [
                    {"name": "live_quarantine", "status": "error"},
                    {"name": "recovery_mode", "status": "warning"},
                    {"name": "runtime_manifest", "status": "ok"},
                ],
            },
            1,
            1,
        )
        with self.assertRaisesRegex(mod.BackupRestoreError, "post_restore_unrelated_health_failure"):
            mod._validate_post_restore_health(
                {
                    "overall": "action_required",
                    "primary_reason": "previous_operation_outcome_unknown",
                    "next_action": "review_incident",
                },
                2,
                {
                    "overall": "unsafe",
                    "checks": [
                        {"name": "live_quarantine", "status": "error"},
                        {"name": "runtime_manifest", "status": "error"},
                    ],
                },
                1,
                1,
            )

    def test_staged_restore_detects_post_copy_tamper(self):
        digest = self._backup()
        self._prepare_clean_target()
        stage = mod.stage_restore(
            self.backup,
            digest,
            stage=self.stage,
            check_services=False,
            verify_codesign=False,
            semantic_validate=False,
        )
        manifest = mod.verify_backup(
            self.backup,
            digest,
            verify_codesign=False,
            semantic_validate=False,
        )
        target = stage / "root/v2/secrets/hub.key"
        target.write_bytes(target.read_bytes() + b"tamper")
        with self.assertRaisesRegex(mod.BackupRestoreError, "restore_stage_file_mismatch"):
            mod.verify_staged_restore(stage, self.backup, digest, manifest)


if __name__ == "__main__":
    unittest.main()

