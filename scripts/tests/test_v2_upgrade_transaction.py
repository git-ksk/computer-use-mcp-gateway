import importlib.util, json, pathlib, stat, sys, tempfile, unittest
SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "v2_upgrade_transaction.py"
SPEC = importlib.util.spec_from_file_location("v2_upgrade_transaction", SCRIPT)
assert SPEC and SPEC.loader
mod = importlib.util.module_from_spec(SPEC); sys.modules[SPEC.name] = mod; SPEC.loader.exec_module(mod)
CUMG = "a" * 40; HANDOFF = "b" * 40

class UpgradeTransactionTests(unittest.TestCase):
    def fixture(self):
        temp = tempfile.TemporaryDirectory(); path = pathlib.Path(temp.name) / "maintenance" / "upgrade-transaction.json"
        return temp, path
    def test_start_and_monotonic_advance_are_owner_private(self):
        temp, path = self.fixture()
        try:
            record = mod.start(path, CUMG, HANDOFF, "upgrade-test")
            self.assertEqual(record["phase"], "build_or_stage")
            self.assertEqual(stat.S_IMODE(path.parent.stat().st_mode), 0o700)
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
            record = mod.advance(path, phase="handoff_stage", runtime_generation="runtime-a-b")
            self.assertEqual(record["runtime_generation"], "runtime-a-b")
            with self.assertRaisesRegex(mod.TransactionError, "transaction_phase_regression"): mod.advance(path, phase="build_or_stage")
        finally: temp.cleanup()
    def test_completed_requires_full_contract_and_v2_authority(self):
        temp, path = self.fixture()
        try:
            mod.start(path, CUMG, HANDOFF, "upgrade-test")
            mod.advance(path, phase="cleanup", runtime_generation="runtime-a-b", rollback_asset="runtime-upgrade-test",
                        mutation_owner="v2", mutation_epoch=2, flags=mod.COMPLETION_FLAGS[:-1])
            with self.assertRaisesRegex(mod.TransactionError, "incomplete_completion_contract"): mod.complete(path)
            mod.advance(path, flags=(mod.COMPLETION_FLAGS[-1],)); record = mod.complete(path)
            self.assertEqual(record["status"], "completed"); self.assertTrue(all(record["completion"].values()))
        finally: temp.cleanup()
    def test_incomplete_prior_transaction_blocks_new_upgrade(self):
        temp, path = self.fixture()
        try:
            mod.start(path, CUMG, HANDOFF, "upgrade-test")
            with self.assertRaisesRegex(mod.TransactionError, "prior_upgrade_requires_operator_action"): mod.start(path, CUMG, HANDOFF, "upgrade-next")
            mod.fail(path, status="failed_before_install", reason="build_storage_exhausted", operator_action="restore_capacity_and_retry")
            self.assertEqual(mod.start(path, CUMG, HANDOFF, "upgrade-next")["transaction_id"], "upgrade-next")
        finally: temp.cleanup()
    def test_disconnect_at_representative_phases_preserves_exact_durable_phase(self):
        for phase in ("service_drain", "post_verify"):
            temp, path = self.fixture()
            try:
                mod.start(path, CUMG, HANDOFF, f"upgrade-{phase}")
                mod.advance(path, phase=phase)
                persisted = mod._read(path)
                self.assertEqual(persisted["status"], "in_progress")
                self.assertEqual(persisted["phase"], phase)
            finally:
                temp.cleanup()

    def test_failure_record_is_bounded_and_does_not_change_completion_flags(self):
        temp, path = self.fixture()
        try:
            mod.start(path, CUMG, HANDOFF, "upgrade-test")
            record = mod.fail(path, status="failed_closed_after_stop", reason="doctor_failed",
                              operator_action="inspect_rollback_before_recovery")
            self.assertEqual(record["status"], "failed_closed_after_stop")
            self.assertEqual(record["failure_reason"], "doctor_failed")
            self.assertFalse(any(record["completion"].values()))
        finally: temp.cleanup()
    def test_group_writable_record_is_rejected(self):
        temp, path = self.fixture()
        try:
            mod.start(path, CUMG, HANDOFF, "upgrade-test"); path.chmod(0o660)
            with self.assertRaisesRegex(mod.TransactionError, "unsafe_transaction_record_permissions"): mod._read(path)
        finally: temp.cleanup()
    def test_unknown_schema_fields_are_rejected(self):
        temp, path = self.fixture()
        try:
            mod.start(path, CUMG, HANDOFF, "upgrade-test")
            value = json.loads(path.read_text()); value["automatic_retry"] = True
            path.write_text(json.dumps(value)); path.chmod(0o600)
            with self.assertRaisesRegex(mod.TransactionError, "invalid_transaction_schema"): mod._read(path)
        finally: temp.cleanup()

if __name__ == "__main__": unittest.main()
